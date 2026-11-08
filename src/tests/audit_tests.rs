//! Exploratory tests written while auditing the crate.
//!
//! They probe the parts that looked suspicious during review:
//! - `estimate_weak_count` semantics (compared to `Arc::weak_count`)
//! - hazard-pointer protection while the collector runs
//! - hazard pointer slot exhaustion (fallback to `load_owned`)
//! - weak resurrection / confirmed-death transitions
//! - `ShardedBox` init panic safety and per-shard indexing
//! - `compare_and_set` with a borrowed `then_set`
//! - `ShardedRwLock` mutual exclusion

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::shard_index::{ShardCount, get_shard_count, shard_indexes};
use crate::{
    AtomicNullableSdarc, AtomicSdarc, Sdarc, ShardedBox, ShardedRwLock, WeakSdarc, collector_update_now_and_wait,
};

// ---------------------------------------------------------------------------
// helper payload: records when it is dropped
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Payload {
    value: u64,
    dropped: Arc<AtomicBool>,
}

impl Payload {
    fn new(value: u64) -> (Self, Arc<AtomicBool>) {
        let flag = Arc::new(AtomicBool::new(false));
        (
            Self {
                value,
                dropped: Arc::clone(&flag),
            },
            flag,
        )
    }
}

impl Drop for Payload {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// estimate_*_count
// ---------------------------------------------------------------------------

#[test]
fn audit_estimate_strong_count() {
    let a = Sdarc::new(1u64);
    assert_eq!(Sdarc::estimate_strong_count(&a), 1);

    let b = a.clone();
    assert_eq!(Sdarc::estimate_strong_count(&a), 2);

    let c = b.clone();
    assert_eq!(Sdarc::estimate_strong_count(&a), 3);

    drop(c);
    assert_eq!(Sdarc::estimate_strong_count(&a), 2);
    drop(b);
    assert_eq!(Sdarc::estimate_strong_count(&a), 1);
    drop(a);
    collector_update_now_and_wait();
}

/// `Arc::weak_count()` counts `Weak` handles: 0 after `Arc::new`, 1 after one
/// `downgrade()`, 0 again after all `Weak`s are dropped.
///
/// `Sdarc::estimate_weak_count` is documented as the weak counterpart of
/// `estimate_strong_count` (which mirrors `Arc::strong_count`), so it should
/// match those numbers. It currently reports the strong count of the internal
/// `Sdarc<WeakSdarcInner>`, which also counts the implicit reference held by
/// `SdarcInner` itself: 0 -> 2 -> 3 -> 2 -> 1 instead of 0 -> 1 -> 2 -> 1 -> 0.
#[test]
#[ignore = "known off-by-one: counts the SdarcInner's own handle to WeakSdarcInner"]
fn audit_estimate_weak_count_semantics() {
    let a = Sdarc::new(1u64);
    assert_eq!(Sdarc::estimate_weak_count(&a), 0, "no weak handles yet");

    let w1 = a.downgrade();
    assert_eq!(
        Sdarc::estimate_weak_count(&a),
        1,
        "one weak handle (Arc::weak_count semantics)"
    );

    let w2 = w1.clone();
    assert_eq!(Sdarc::estimate_weak_count(&a), 2, "two weak handles");

    drop(w2);
    assert_eq!(Sdarc::estimate_weak_count(&a), 1, "one weak handle left");

    drop(w1);
    assert_eq!(
        Sdarc::estimate_weak_count(&a),
        0,
        "all weak handles dropped, pointee still strong-alive"
    );

    drop(a);
    collector_update_now_and_wait();
}

// ---------------------------------------------------------------------------
// hazard pointer protection
// ---------------------------------------------------------------------------

/// A borrow guard must keep the pointee alive across a full deep collection,
/// even after the `AtomicSdarc` (the only owner) has been repointed.
#[test]
fn audit_borrow_guard_survives_collection() {
    let (old_payload, old_dropped) = Payload::new(42);
    let atomic = AtomicSdarc::new(old_payload);

    let guard = atomic.borrow();
    assert_eq!(guard.value, 42);

    // Replace the pointee: the old payload's only remaining protection is the
    // hazard pointer held by `guard`.
    let (new_payload, _new_dropped) = Payload::new(7);
    atomic.store(Sdarc::new(new_payload));

    // Deep collect twice; the old payload must not be freed yet.
    collector_update_now_and_wait();
    collector_update_now_and_wait();
    assert!(
        !old_dropped.load(Ordering::SeqCst),
        "pointee was freed while a borrow guard was alive"
    );
    assert_eq!(guard.value, 42, "borrow guard data corrupted");

    // Once the guard is gone the old payload can be collected.
    drop(guard);
    collector_update_now_and_wait();
    assert!(
        old_dropped.load(Ordering::SeqCst),
        "pointee was not collected after the guard was dropped"
    );

    // New pointee must still be intact.
    assert_eq!(atomic.load_owned().value, 7);
}

/// Same, but through `WeakSdarc::borrow` after the strong count reached zero.
#[test]
fn audit_weak_borrow_guard_survives_collection() {
    let (payload, dropped) = Payload::new(11);
    let strong = Sdarc::new(payload);
    let weak = strong.downgrade();
    let guard = weak.borrow().expect("pointee alive");
    assert_eq!(guard.value, 11);

    drop(strong);
    collector_update_now_and_wait();
    collector_update_now_and_wait();
    assert!(
        !dropped.load(Ordering::SeqCst),
        "weak borrow guard did not keep pointee alive"
    );
    assert_eq!(guard.value, 11);

    drop(guard);
    collector_update_now_and_wait();
    assert!(
        dropped.load(Ordering::SeqCst),
        "pointee was not collected after the weak guard was dropped"
    );
    assert!(weak.upgrade().is_none(), "upgrade after death must fail");
}

/// Clearing the atomic pointer while a guard is alive must still protect the
/// pointee (the atomic's own reference is gone).
#[test]
fn audit_guard_outlives_atomic_owner() {
    let (payload, dropped) = Payload::new(5);
    let atomic = AtomicNullableSdarc::new_with_value(payload);
    let guard = atomic.borrow().expect("not null");
    atomic.store(None);
    assert!(atomic.load_owned().is_none());

    collector_update_now_and_wait();
    collector_update_now_and_wait();
    assert!(
        !dropped.load(Ordering::SeqCst),
        "pointee freed although guard is alive"
    );
    assert_eq!(guard.value, 5);

    drop(guard);
    collector_update_now_and_wait();
    assert!(dropped.load(Ordering::SeqCst), "pointee leaked");
}

/// Holding more guards than there are hazard-pointer slots must still be safe:
/// the excess guards fall back to an owned reference.
#[test]
fn audit_hazard_slot_exhaustion() {
    // HZ_PTR_SLOT_COUNT is 15; hold more than that.
    const HELD: usize = 40;

    let (old_payload, old_dropped) = Payload::new(1);
    let atomic = AtomicSdarc::new(old_payload);

    let guards: Vec<_> = (0..HELD).map(|_| atomic.borrow()).collect();
    for g in &guards {
        assert_eq!(g.value, 1);
    }

    // Repoint + collect while all guards are held.
    let (new_payload, _new_dropped) = Payload::new(2);
    atomic.store(Sdarc::new(new_payload));
    collector_update_now_and_wait();
    collector_update_now_and_wait();

    assert!(
        !old_dropped.load(Ordering::SeqCst),
        "pointee freed while {HELD} guards were alive"
    );
    for g in &guards {
        assert_eq!(g.value, 1, "guard data corrupted under slot exhaustion");
    }

    drop(guards);
    collector_update_now_and_wait();
    assert!(
        old_dropped.load(Ordering::SeqCst),
        "pointee leaked after all guards dropped"
    );
    assert_eq!(atomic.load_owned().value, 2);
}

// ---------------------------------------------------------------------------
// weak reference lifecycle
// ---------------------------------------------------------------------------

/// Upgrade before the collector confirms death resurrects the object.
/// After the collector has run, the pointer must be dead and the payload freed.
#[test]
fn audit_weak_resurrection_and_confirmed_death() {
    let (payload, dropped) = Payload::new(3);
    let strong = Sdarc::new(payload);
    let weak = strong.downgrade();
    drop(strong);

    // Before collecting, upgrade may still succeed (resurrection).
    let resurrected = weak.upgrade();
    assert!(resurrected.is_some(), "upgrade before collection should win");
    assert!(!dropped.load(Ordering::SeqCst));
    assert_eq!(resurrected.as_ref().unwrap().value, 3);
    drop(resurrected);

    collector_update_now_and_wait();

    assert!(
        dropped.load(Ordering::SeqCst),
        "dead pointee was not reclaimed"
    );
    assert!(
        weak.upgrade().is_none(),
        "confirmed-dead pointee must not be upgradeable"
    );
    assert!(weak.borrow().is_none(), "borrow of dead pointee must fail");
}

#[test]
fn audit_weak_ptr_eq_and_counts() {
    let a = Sdarc::new(1u64);
    let b = Sdarc::new(2u64);
    let wa1 = a.downgrade();
    let wa2 = a.downgrade();
    let wb = b.downgrade();

    assert!(WeakSdarc::ptr_eq(&wa1, &wa2));
    assert!(!WeakSdarc::ptr_eq(&wa1, &wb));

    drop(wa1);
    drop(wa2);
    drop(wb);
    drop(a);
    drop(b);
    collector_update_now_and_wait();
}

// ---------------------------------------------------------------------------
// ShardedBox
// ---------------------------------------------------------------------------

#[test]
fn audit_sharded_box_indexing() {
    let count = get_shard_count().as_usize();
    let b: ShardedBox<u64> = ShardedBox::allocate_data_in_each_shard(|i| i.as_usize() as u64);

    let mut seen = 0usize;
    for i in shard_indexes() {
        assert_eq!(b[i], i.as_usize() as u64, "wrong value in shard {i:?}");
        seen += 1;
    }
    assert_eq!(seen, count);
    assert_eq!(*b.at_curr_shard(), crate::curr_shard_index().as_usize() as u64);
}

#[test]
fn audit_sharded_box_init_panic_is_contained() {
    let result = std::panic::catch_unwind(|| {
        let _ = ShardedBox::<AtomicU64>::allocate_data_in_each_shard(|i| {
            if i.as_usize() == 1 {
                panic!("init failure");
            }
            AtomicU64::new(0)
        });
    });
    assert!(result.is_err());

    // The allocator must still be usable afterwards.
    let b: ShardedBox<u64> = ShardedBox::allocate_data_in_each_shard(|_| 123);
    assert_eq!(*b.at_curr_shard(), 123);

    // And a permanent allocation must still work.
    let s = Sdarc::new(9u64);
    assert_eq!(*s, 9);
    drop(s);
    collector_update_now_and_wait();
}

/// Repeatedly allocate and free `ShardedBox`es while the collector performs
/// allocator maintenance. (Deterministic Miri reproduction of the data race
/// requires `RUST_SDARC_COLLECTOR_INTERVAL_MS=0` and maintenance enabled.)
///
/// MIRIFLAGS="-Zmiri-ignore-leaks -Zmiri-env-forward=RUST_SDARC_SHARD_COUNT -Zmiri-env-forward=RUST_SDARC_COLLECTOR_INTERVAL_MS" RUST_SDARC_SHARD_COUNT=4 RUST_SDARC_COLLECTOR_INTERVAL_MS=0 cargo +nightly miri test audit_sharded_box_churn_maintenance -- --nocapture
#[test]
fn audit_sharded_box_churn_maintenance() {
    // Make sure the collector (which performs allocator maintenance) is running.
    let warmup = Sdarc::new(0u64);
    std::hint::black_box(&warmup);

    let rounds = if cfg!(miri) { 200 } else { 200_000 };
    for _ in 0..rounds {
        let b: ShardedBox<u64> = ShardedBox::allocate_data_in_each_shard(|i| i.as_usize() as u64);
        std::hint::black_box(b.at_curr_shard());
        drop(b);
    }
    drop(warmup);
    collector_update_now_and_wait();
}

#[test]
fn audit_sharded_box_concurrent() {
    let threads: Vec<_> = (0..8)
        .map(|t| {
            std::thread::spawn(move || {
                for i in 0..2000u64 {
                    let b = ShardedBox::allocate_data_in_each_shard(|s| {
                        (t as u64) << 32 | i << 8 | s.as_usize() as u64
                    });
                    assert_eq!(b.at_curr_shard() & 0xff, crate::curr_shard_index().as_usize() as u64);
                    for s in shard_indexes() {
                        assert_eq!(b[s] >> 32, t as u64);
                        assert_eq!((b[s] >> 8) & 0xff_ffff, i);
                        assert_eq!(b[s] & 0xff, s.as_usize() as u64);
                    }
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
}

// ---------------------------------------------------------------------------
// atomic pointer edge cases
// ---------------------------------------------------------------------------

/// `compare_and_set` takes `then_set` by reference; the atomic must own a
/// reference to it after the call.
#[test]
fn audit_compare_and_set_borrowed_then_set() {
    let atomic = AtomicNullableSdarc::new_with_value(vec![1u8; 8]);

    let current = atomic.load_owned();
    let then_set = Some(Sdarc::new(vec![2u8; 8]));

    let old = atomic.compare_and_set(&current, &then_set).unwrap();
    assert_eq!(old.unwrap()[0], 1);

    // The only references to the new pointee are now the atomic and
    // `then_set`; drop the borrowed owners.
    drop(then_set);
    drop(current);

    collector_update_now_and_wait();
    assert_eq!(
        atomic.load_owned().unwrap()[0],
        2,
        "then_set pointee was freed prematurely"
    );
}

#[test]
fn audit_sync_to() {
    let atomic = AtomicSdarc::new(1u64);
    let mut local = atomic.load_owned();
    assert_eq!(*local, 1);

    atomic.sync_to(&mut local);
    assert_eq!(*local, 1);

    atomic.store(Sdarc::new(2u64));
    atomic.sync_to(&mut local);
    assert_eq!(*local, 2);

    let nullable = AtomicNullableSdarc::new_with_value(10u64);
    let mut local2 = Some(nullable.load_owned().unwrap());
    nullable.sync_to(&mut local2);
    assert_eq!(**local2.as_ref().unwrap(), 10);

    nullable.store(None);
    nullable.sync_to(&mut local2);
    assert!(local2.is_none());

    drop(atomic);
    drop(nullable);
    drop(local);
    drop(local2);
    collector_update_now_and_wait();
}

// ---------------------------------------------------------------------------
// ShardCount
// ---------------------------------------------------------------------------

#[test]
fn audit_shard_count_rounding() {
    assert_eq!(ShardCount::from_exponent_adjusted(0).as_usize(), 1);
    assert_eq!(ShardCount::from_exponent_adjusted(1).as_usize(), 2);
    assert_eq!(ShardCount::from_exponent_adjusted(8).as_usize(), 256);
    // clamped to the max exponent
    assert_eq!(ShardCount::from_exponent_adjusted(9).as_usize(), 256);
    assert_eq!(ShardCount::from_exponent_adjusted(u32::MAX).as_usize(), 256);

    assert_eq!(ShardCount::from_usize_adjusted(1).as_usize(), 1);
    assert_eq!(ShardCount::from_usize_adjusted(2).as_usize(), 2);
    assert_eq!(ShardCount::from_usize_adjusted(3).as_usize(), 4);
    assert_eq!(ShardCount::from_usize_adjusted(5).as_usize(), 8);
    assert_eq!(ShardCount::from_usize_adjusted(256).as_usize(), 256);
    // exceeds MAX_SHARD_COUNT: clamped rather than rounded up
    assert_eq!(ShardCount::from_usize_adjusted(257).as_usize(), 256);
    assert_eq!(ShardCount::from_usize_adjusted(usize::MAX).as_usize(), 256);

    for c in [
        ShardCount::from_usize_adjusted(1),
        ShardCount::from_usize_adjusted(3),
        ShardCount::from_usize_adjusted(300),
    ] {
        for n in [0usize, 1, 2, 255, 256, 257, usize::MAX] {
            let idx = c.modulo(n);
            assert!(
                idx.as_usize() < c.as_usize(),
                "index {} out of range for {} shards",
                idx.as_usize(),
                c.as_usize()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// ShardedRwLock exclusivity
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// collector / TLS edge cases
// ---------------------------------------------------------------------------

/// Several threads forcing collection at the same time must all be woken up.
#[test]
fn audit_collector_concurrent_waiters() {
    let mut flags = Vec::new();
    for i in 0..8u64 {
        let (payload, flag) = Payload::new(i);
        let s = Sdarc::new(payload);
        drop(s);
        flags.push(flag);
    }

    let waiters: Vec<_> = (0..8)
        .map(|_| std::thread::spawn(collector_update_now_and_wait))
        .collect();
    for w in waiters {
        w.join().unwrap();
    }
    collector_update_now_and_wait();

    for (i, flag) in flags.iter().enumerate() {
        assert!(
            flag.load(Ordering::SeqCst),
            "payload {i} was not reclaimed after concurrent collection"
        );
    }
}

/// Dropping a `Sdarc` stored in a thread local runs during TLS destruction;
/// `curr_shard_index` must not panic in that situation.
///
/// Under `cfg(miri)` this aborts: the miri implementation of `curr_shard_index`
/// calls `rand::rng()`, whose thread local has already been destroyed.
#[test]
#[cfg_attr(miri, ignore = "cfg(miri) curr_shard_index panics during TLS destruction")]
fn audit_drop_in_tls_at_thread_exit() {
    let (payload, dropped) = Payload::new(1);
    std::thread::spawn(move || {
        thread_local! {
            static TLS: std::cell::RefCell<Option<Sdarc<Payload>>> =
                const { std::cell::RefCell::new(None) };
        }
        TLS.with(|cell| *cell.borrow_mut() = Some(Sdarc::new(payload)));
        // thread exits here; TLS drops the `Sdarc`
    })
    .join()
    .unwrap();

    collector_update_now_and_wait();
    assert!(
        dropped.load(Ordering::SeqCst),
        "payload stored in a thread local leaked"
    );
}

/// A no-op compare-and-set (same pointer in and out) must not corrupt counts.
#[test]
fn audit_compare_and_set_same_pointer() {
    let atomic = AtomicNullableSdarc::new_with_value(3u64);
    let current = atomic.load_owned();

    let old = atomic.compare_and_set(&current, &current).unwrap();
    assert_eq!(*old.unwrap(), 3);
    assert_eq!(*atomic.load_owned().unwrap(), 3);
    assert_eq!(**current.as_ref().unwrap(), 3);

    drop(current);
    collector_update_now_and_wait();
    assert_eq!(*atomic.load_owned().unwrap(), 3);
}

/// Isolates the `cfg(miri)` TLS-destruction bug: calling `curr_shard_index`
/// from a TLS destructor panics with "cannot access a Thread Local Storage
/// value during or after destruction", because the miri implementation calls
/// `rand::rng()` whose thread local was already destroyed.
///
/// The `cfg(not(miri))` fallback implementation guards against this with
/// `try_with`, but the miri implementation does not.
#[test]
#[cfg_attr(miri, ignore = "cfg(miri) curr_shard_index panics during TLS destruction")]
fn audit_curr_shard_index_during_tls_destruction() {
    struct DestructorProbe;

    impl Drop for DestructorProbe {
        fn drop(&mut self) {
            let _ = crate::curr_shard_index();
        }
    }

    std::thread::spawn(|| {
        thread_local! {
            static PROBE: std::cell::RefCell<Option<DestructorProbe>> =
                const { std::cell::RefCell::new(None) };
        }
        // Initialize the probe's thread local *first*, then the random source's
        // thread local, so that at thread exit the random source is destroyed
        // before the probe's destructor runs.
        PROBE.with(|cell| *cell.borrow_mut() = Some(DestructorProbe));
        let _ = crate::curr_shard_index();
    })
    .join()
    .unwrap();
}

/// Writers mutate a `Vec` while readers iterate it. If mutual exclusion is
/// broken this is a data race (and can panic / be caught by miri).
#[test]
fn audit_sharded_rwlock_exclusivity() {
    let lock = Arc::new(ShardedRwLock::new(Vec::<u64>::new()));
    let stop = Arc::new(AtomicBool::new(false));

    let readers: Vec<_> = (0..4)
        .map(|_| {
            let lock = Arc::clone(&lock);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let guard = lock.read();
                    // Sum must equal len; observing a torn Vec would be UB.
                    let sum: u64 = guard.iter().copied().sum();
                    assert_eq!(sum, guard.len() as u64 * 7, "torn read");
                }
            })
        })
        .collect();

    let writers: Vec<_> = (0..2)
        .map(|_| {
            let lock = Arc::clone(&lock);
            std::thread::spawn(move || {
                for n in 0..2000u64 {
                    let mut guard = lock.write();
                    guard.clear();
                    for _ in 0..=(n % 8) {
                        guard.push(7);
                    }
                }
            })
        })
        .collect();

    // Give writers time, then stop.
    std::thread::sleep(std::time::Duration::from_millis(300));
    stop.store(true, Ordering::Relaxed);
    for r in readers {
        r.join().unwrap();
    }
    for w in writers {
        w.join().unwrap();
    }

    let guard = lock.read();
    assert!(guard.iter().all(|v| *v == 7));
}

