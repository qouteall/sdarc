//! Stress test that can run in Miri.
//!
//! Exercises the same patterns as the full stress test — `Sdarc` clone/drop,
//! `AtomicSdarc` load/store/swap/borrow (hazard-pointer path), `WeakSdarc`
//! downgrade/upgrade, cross-thread message passing, and short-lived child
//! threads — but with `std::thread` only.
//!
//! Iteration counts and thread counts are selected with `cfg(miri)`: full
//! values for native runs, much smaller values under Miri so it finishes in
//! reasonable time.
//!
//! All setup/thread code is wrapped in `{}` so that every `Sdarc` reference
//! is guaranteed dropped before `collector_update_now_and_wait()` is called.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Barrier, mpsc};
use std::thread;
use crate::atomic_sdarc::AtomicSdarc;
use crate::collector::collector_update_now_and_wait;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

struct CheapRng(u64);
impl CheapRng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn usize(&mut self, max: usize) -> usize {
        (self.next() as usize) % max
    }
    fn bool(&mut self, pct: u8) -> bool {
        self.usize(100) < pct as usize
    }
}

// ===========================================================================
// TrackedDrop — global counter for leak detection
// ===========================================================================

static TRACKED_ALLOC_COUNT: AtomicI64 = AtomicI64::new(0);

#[derive(Debug)]
struct TrackedDrop {
    id: u64,
    _pad: [u64; 4],
}

impl TrackedDrop {
    fn new(id: u64) -> Self {
        TRACKED_ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        Self { id, _pad: [0; 4] }
    }
}

impl Clone for TrackedDrop {
    fn clone(&self) -> Self {
        TRACKED_ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        Self {
            id: self.id,
            _pad: self._pad,
        }
    }
}

impl Drop for TrackedDrop {
    fn drop(&mut self) {
        TRACKED_ALLOC_COUNT.fetch_sub(1, Ordering::Relaxed);
    }
}

fn tracked_alloc_count() -> i64 {
    TRACKED_ALLOC_COUNT.load(Ordering::Relaxed)
}

// ===========================================================================
// Cache entry
// ===========================================================================

#[derive(Clone, Debug)]
struct CacheEntry {
    #[allow(dead_code)]
    key: u64,
    #[allow(dead_code)]
    value: u64,
    generation: u64,
    #[allow(dead_code)]
    payload: TrackedDrop,
}

impl CacheEntry {
    fn new(key: u64, value: u64, generation: u64) -> Self {
        Self {
            key,
            value,
            generation,
            payload: TrackedDrop::new(key),
        }
    }
}

// ===========================================================================
// SharedContext — held in Sdarc, shared by all workers
// ===========================================================================

use crate::sdarc::{ Sdarc};
use crate::weak_sdarc::WeakSdarc;

struct SharedContext {
    /// The "cache database" — periodically swapped by the updater thread.
    atomic_cache: AtomicSdarc<HashMap<u64, CacheEntry>>,

    /// Pool of hot items for clone/drop contention.
    hot_pool: Vec<Sdarc<TrackedDrop>>,

    /// Metrics.
    ops: AtomicU64,
    upgrades_ok: AtomicU64,
    upgrades_fail: AtomicU64,
    swaps: AtomicU64,
    forwards: AtomicU64,
    spawns: AtomicU64,
    borrows: AtomicU64,
    invariant_ok: AtomicBool,

    /// Stop signal.
    stop: AtomicBool,
}

impl SharedContext {
    fn new(
        atomic_cache: AtomicSdarc<HashMap<u64, CacheEntry>>,
        hot_pool: Vec<Sdarc<TrackedDrop>>,
    ) -> Self {
        Self {
            atomic_cache,
            hot_pool,
            ops: AtomicU64::new(0),
            upgrades_ok: AtomicU64::new(0),
            upgrades_fail: AtomicU64::new(0),
            swaps: AtomicU64::new(0),
            forwards: AtomicU64::new(0),
            spawns: AtomicU64::new(0),
            borrows: AtomicU64::new(0),
            invariant_ok: AtomicBool::new(true),
            stop: AtomicBool::new(false),
        }
    }
}

// ===========================================================================
// Per-iteration counts — much smaller under Miri so it can finish.
// ===========================================================================

/// Number of iterations each worker thread performs.
#[cfg(not(miri))]
const WORKER_ITERATIONS: usize = 200000;
#[cfg(miri)]
const WORKER_ITERATIONS: usize = 300;

/// Number of iterations the background updater performs.
#[cfg(not(miri))]
const UPDATER_ITERATIONS: usize = 300000;
#[cfg(miri)]
const UPDATER_ITERATIONS: usize = 400;

/// Number of iterations each producer performs.
#[cfg(not(miri))]
const PRODUCER_ITERATIONS: usize = 150000;
#[cfg(miri)]
const PRODUCER_ITERATIONS: usize = 200;

/// Number of iterations each borrower thread performs.
#[cfg(not(miri))]
const BORROWER_ITERATIONS: usize = 400000;
#[cfg(miri)]
const BORROWER_ITERATIONS: usize = 500;

/// Number of std worker threads.
#[cfg(not(miri))]
const STD_WORKERS: usize = 10;
#[cfg(miri)]
const STD_WORKERS: usize = 3;

/// Number of producer threads.
#[cfg(not(miri))]
const PRODUCERS: usize = 10;
#[cfg(miri)]
const PRODUCERS: usize = 2;

/// Number of borrower threads hammering `AtomicSdarc::borrow`.
#[cfg(not(miri))]
const BORROWERS: usize = 4;
#[cfg(miri)]
const BORROWERS: usize = 2;

/// Size of the hot pool.
#[cfg(not(miri))]
const HOT_POOL_SIZE: usize = 40;
#[cfg(miri)]
const HOT_POOL_SIZE: usize = 8;

/// Maximum spawned child threads per worker.
#[cfg(not(miri))]
const MAX_SPAWNED_CHILDREN: usize = 40;
#[cfg(miri)]
const MAX_SPAWNED_CHILDREN: usize = 4;

/// Maximum held entries per worker before truncation.
const MAX_HAND_SIZE: usize = 16;

/// Maximum held hot items per worker before truncation.
const MAX_HOT_HAND_SIZE: usize = 12;

// ===========================================================================
// Scenario
// ===========================================================================

fn scenario_stress() {
    let ops;
    let upgrades_ok;
    let upgrades_fail;
    let swaps;
    let forwards;
    let spawns;
    let borrows;
    let invariant_ok;

    // ---- All Sdarc references live inside this block ----
    {
        // ---- Build the hot pool ----
        let hot_pool: Vec<Sdarc<TrackedDrop>> = (0..HOT_POOL_SIZE)
            .map(|i| Sdarc::new(TrackedDrop::new(i as u64)))
            .collect();

        let initial_cache = AtomicSdarc::new(HashMap::new());

        // ---- Build shared context ----
        let context = Sdarc::new(SharedContext::new(initial_cache, hot_pool));

        // ---- Per-worker mpsc channels ----
        let mut worker_receivers: Vec<mpsc::Receiver<Sdarc<CacheEntry>>> = vec![];
        let worker_senders: Vec<mpsc::Sender<Sdarc<CacheEntry>>> = (0..STD_WORKERS)
            .map(|_| {
                let (tx, rx) = mpsc::channel();
                worker_receivers.push(rx);
                tx
            })
            .collect();

        // =====================================================================
        // 1. Background cache updater
        // =====================================================================
        let updater_ctx = context.clone();
        let updater_handle = thread::spawn(move || {
            let mut rng = CheapRng::new(0xc0ffee);
            let mut generation: u64 = 0;
            for _ in 0..UPDATER_ITERATIONS {
                if updater_ctx.stop.load(Ordering::Relaxed) {
                    break;
                }
                generation += 1;
                let entry_count = 3 + rng.usize(12);
                let mut map = HashMap::with_capacity(entry_count);
                for _ in 0..entry_count {
                    let k = rng.next();
                    map.insert(k, CacheEntry::new(k, rng.next(), generation));
                }
                let _old = updater_ctx.atomic_cache.swap(Sdarc::new(map));
                updater_ctx.swaps.fetch_add(1, Ordering::Relaxed);
                thread::yield_now();
            }
        });

        // =====================================================================
        // 2. Request producers — push CacheEntry clones into worker inboxes
        // =====================================================================
        let producer_senders = worker_senders.clone();
        let producer_ctx = context.clone();
        let producers: Vec<thread::JoinHandle<()>> = (0..PRODUCERS)
            .map(|p| {
                let senders = producer_senders.clone();
                let ctx = producer_ctx.clone();
                thread::spawn(move || {
                    let mut rng = CheapRng::new((p + 100) as u64 * 0x9e3779b9);
                    for _ in 0..PRODUCER_ITERATIONS {
                        if ctx.stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let cache = ctx.atomic_cache.load_owned();
                        if !cache.is_empty() {
                            let keys: Vec<u64> = cache.keys().copied().collect();
                            let k = keys[rng.usize(keys.len())];
                            if let Some(entry) = cache.get(&k) {
                                let clone = Sdarc::new(entry.clone());
                                let dst = rng.usize(STD_WORKERS);
                                let _ = senders[dst].send(clone);
                                ctx.ops.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        drop(cache);
                    }
                })
            })
            .collect();

        // =====================================================================
        // 3. Borrower threads — hammer `AtomicSdarc::borrow` while the
        //    updater swaps the cache. Exercises the hazard-pointer path and
        //    the owned-load fallback under contention.
        // =====================================================================
        let borrower_ctx = context.clone();
        let borrowers: Vec<thread::JoinHandle<()>> = (0..BORROWERS)
            .map(|b| {
                let ctx = borrower_ctx.clone();
                thread::spawn(move || {
                    let mut rng = CheapRng::new((b + 500) as u64 * 0x85ebca6b);
                    for _ in 0..BORROWER_ITERATIONS {
                        if ctx.stop.load(Ordering::Relaxed) {
                            break;
                        }
                        {
                            let guard = ctx.atomic_cache.borrow();
                            if !guard.is_empty() {
                                // The borrowed map is immutable while borrowed,
                                // and no generation-0 entry may ever be visible.
                                for entry in guard.values() {
                                    if entry.generation == 0 {
                                        ctx.invariant_ok.store(false, Ordering::Relaxed);
                                    }
                                }
                            }
                            // guard dropped here, hazard pointer unpublished
                        }
                        ctx.borrows.fetch_add(1, Ordering::Relaxed);
                        if rng.bool(30) {
                            thread::yield_now();
                        }
                    }
                })
            })
            .collect();

        // =====================================================================
        // 4. Std worker threads
        // =====================================================================
        let worker_barrier = std::sync::Arc::new(Barrier::new(STD_WORKERS));
        let worker_senders_shared = worker_senders.clone();
        let workers: Vec<thread::JoinHandle<()>> = (0..STD_WORKERS)
            .map(|w| {
                let rx = std::mem::replace(&mut worker_receivers[w], {
                    let (_, rx) = mpsc::channel::<Sdarc<CacheEntry>>();
                    rx
                });
                let senders = worker_senders_shared.clone();
                let ctx = context.clone();
                let barrier = std::sync::Arc::clone(&worker_barrier);
                thread::spawn(move || {
                    let mut rng = CheapRng::new((w + 1) as u64 * 0x7f4a7c15);
                    let mut hand: Vec<Sdarc<CacheEntry>> = vec![];
                    let mut hot_hand: Vec<Sdarc<TrackedDrop>> = vec![];
                    let mut weak_set: Vec<WeakSdarc<CacheEntry>> = vec![];
                    let mut spawned: Vec<thread::JoinHandle<()>> = vec![];

                    barrier.wait();
                    for _iter in 0..WORKER_ITERATIONS {
                        if ctx.stop.load(Ordering::Relaxed) {
                            break;
                        }

                        // Drain inbox.
                        while let Ok(msg) = rx.try_recv() {
                            hand.push(msg);
                            ctx.ops.fetch_add(1, Ordering::Relaxed);
                        }

                        match rng.usize(10) {
                            0 => {
                                // Load cache, clone entry.
                                let cache = ctx.atomic_cache.load_owned();
                                if !cache.is_empty() {
                                    let keys: Vec<u64> = cache.keys().copied().collect();
                                    let k = keys[rng.usize(keys.len())];
                                    if let Some(entry) = cache.get(&k) {
                                        hand.push(Sdarc::new(entry.clone()));
                                    }
                                }
                                drop(cache);
                                ctx.ops.fetch_add(1, Ordering::Relaxed);
                            }
                            1 => {
                                // Clone from hot pool.
                                let idx = rng.usize(ctx.hot_pool.len());
                                hot_hand.push(ctx.hot_pool[idx].clone());
                                ctx.ops.fetch_add(1, Ordering::Relaxed);
                            }
                            2 => {
                                // Clear hands.
                                hand.clear();
                                hot_hand.clear();
                            }
                            3 => {
                                // Downgrade → store weak ref.
                                if let Some(entry) = hand.pop() {
                                    weak_set.push(entry.downgrade());
                                }
                            }
                            4 => {
                                // Try upgrade a weak ref.
                                if let Some(idx) = weak_set.iter().position(|_| rng.bool(50)) {
                                    let weak_ref = weak_set.swap_remove(idx);
                                    match weak_ref.upgrade() {
                                        Some(u) => {
                                            assert!(u.generation > 0);
                                            ctx.upgrades_ok.fetch_add(1, Ordering::Relaxed);
                                            hand.push(u);
                                        }
                                        None => {
                                            ctx.upgrades_fail.fetch_add(1, Ordering::Relaxed);
                                        }
                                    }
                                }
                            }
                            5 => {
                                // Forward to another worker.
                                if let Some(entry) = hand.pop() {
                                    let dst = rng.usize(STD_WORKERS);
                                    let _ = senders[dst].send(entry);
                                    ctx.forwards.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            6 => {
                                // Spawn short-lived helper thread.
                                if spawned.len() < MAX_SPAWNED_CHILDREN {
                                    let child_ctx = ctx.clone();
                                    let child_senders = senders.clone();
                                    let mut rng2 = CheapRng::new(rng.next());
                                    let child = thread::spawn(move || {
                                        let idx = rng2.usize(child_ctx.hot_pool.len());
                                        let clone = child_ctx.hot_pool[idx].clone();
                                        for _ in 0..rng2.usize(4) {
                                            let _c2 = clone.clone();
                                            child_ctx.ops.fetch_add(1, Ordering::Relaxed);
                                        }
                                        let _ = child_senders[rng2.usize(child_senders.len())]
                                            .send(Sdarc::new(CacheEntry::new(
                                                rng2.next(),
                                                rng2.next(),
                                                999,
                                            )));
                                        child_ctx.forwards.fetch_add(1, Ordering::Relaxed);
                                        drop(clone);
                                    });
                                    spawned.push(child);
                                    ctx.spawns.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            7 => {
                                // Verify invariant: no generation-0 entries reach workers.
                                for entry in &hand {
                                    if entry.generation == 0 {
                                        ctx.invariant_ok.store(false, Ordering::Relaxed);
                                    }
                                }
                            }
                            8 => {
                                // Borrow the cache via hazard pointer, clone an entry.
                                let guard = ctx.atomic_cache.borrow();
                                if !guard.is_empty() {
                                    let keys: Vec<u64> = guard.keys().copied().collect();
                                    let k = keys[rng.usize(keys.len())];
                                    if let Some(entry) = guard.get(&k) {
                                        if entry.generation == 0 {
                                            ctx.invariant_ok.store(false, Ordering::Relaxed);
                                        }
                                        hand.push(Sdarc::new(entry.clone()));
                                    }
                                }
                                drop(guard);
                                ctx.borrows.fetch_add(1, Ordering::Relaxed);
                            }
                            9 => {
                                // Hold several borrows alive at once to exercise
                                // multiple hazard pointer slots per thread.
                                let g1 = ctx.atomic_cache.borrow();
                                let g2 = ctx.atomic_cache.borrow();
                                let g3 = ctx.atomic_cache.borrow();
                                let len_sum = g1.len() + g2.len() + g3.len();
                                let _ = std::hint::black_box(len_sum);
                                ctx.borrows.fetch_add(3, Ordering::Relaxed);
                            }
                            _ => unreachable!(),
                        }

                        // Prune finished spawned children.
                        spawned.retain(|h| !h.is_finished());

                        // Bound memory.
                        if hand.len() > MAX_HAND_SIZE {
                            hand.truncate(MAX_HAND_SIZE / 2);
                        }
                        if hot_hand.len() > MAX_HOT_HAND_SIZE {
                            hot_hand.clear();
                        }
                        if weak_set.len() > MAX_HAND_SIZE {
                            weak_set.clear();
                        }
                    }

                    // Join remaining spawned children.
                    for h in spawned {
                        let _ = h.join();
                    }
                    // hand, hot_hand, weak_set, rx, senders, ctx
                    // are all dropped when the closure returns
                })
            })
            .collect();

        // =====================================================================
        // Wait for all threads to finish.
        // =====================================================================
        for p in producers {
            p.join().unwrap();
        }
        for b in borrowers {
            b.join().unwrap();
        }
        for w in workers {
            w.join().unwrap();
        }
        updater_handle.join().unwrap();

        // Snapshot metrics before dropping context.
        ops = context.ops.load(Ordering::Relaxed);
        upgrades_ok = context.upgrades_ok.load(Ordering::Relaxed);
        upgrades_fail = context.upgrades_fail.load(Ordering::Relaxed);
        swaps = context.swaps.load(Ordering::Relaxed);
        forwards = context.forwards.load(Ordering::Relaxed);
        spawns = context.spawns.load(Ordering::Relaxed);
        borrows = context.borrows.load(Ordering::Relaxed);
        invariant_ok = context.invariant_ok.load(Ordering::Relaxed);

        // Drain real channels (receivers were moved into workers and dropped
        // when workers exited; worker_receivers now holds only dummy receivers).
        // Drop senders to ensure channels are disconnected, then drain dummies.
        drop(worker_senders);
        drop(worker_receivers);

        // context, worker_senders_shared, producer_senders, producer_ctx, etc.
        // are all dropped at the end of this block
    }

    eprintln!(
        "=== stress done ===\n  ops={ops}  up(ok={upgrades_ok} fail={upgrades_fail})  swaps={swaps}  fwd={forwards}  spawns={spawns}  borrows={borrows}",
    );

    assert!(invariant_ok, "invariant violated");

    eprintln!(
        "  before collector: tracked={}, slots={}",
        tracked_alloc_count(),
        crate::sharded_alloc::total_sharded_alloc_used_slots()
    );

    collector_update_now_and_wait();

    let remaining = tracked_alloc_count();
    let used_slots = crate::sharded_alloc::total_sharded_alloc_used_slots();

    eprintln!("  after collector: tracked={remaining}, slots={used_slots}");

    assert_eq!(
        remaining, 0,
        "TrackedDrop leak: {remaining} instances still alive"
    );
    assert_eq!(
        used_slots, 0,
        "sharded alloc leak: {used_slots} slots still used"
    );
}

// ===========================================================================
// Test entry point
// ===========================================================================

#[test]
#[serial_test::serial]
fn stress_test() {
    let shard_count = crate::env_params::shard_count_from_env_var();
    let collector_params = crate::env_params::CollectorParams::new_from_env_var();
    let disable_maintenance = crate::env_params::disable_sharded_alloc_maintenance();

    println!(
        "=== stress config: shard_count={}, collector_interval_ms={}, disable_maintenance={} ===",
        shard_count.map_or("default".to_string(), |s| s.as_usize().to_string()),
        collector_params.interval.as_millis(),
        disable_maintenance,
    );

    scenario_stress();
}

// How to run:
// MIRIFLAGS="-Zmiri-ignore-leaks -Zmiri-env-forward=RUST_SDARC_SHARD_COUNT -Zmiri-env-forward=RUST_SDARC_COLLECTOR_INTERVAL_MS -Zmiri-env-forward=RUST_SDARC_TEST_DISABLE_SHARDED_ALLOC_MAINTENANCE" RUST_SDARC_SHARD_COUNT=4 RUST_SDARC_COLLECTOR_INTERVAL_MS=0 RUST_SDARC_TEST_DISABLE_SHARDED_ALLOC_MAINTENANCE=1 cargo +nightly miri test stress_test -- --nocapture
// cannot run it with miri in windows due to parking_lot compatibility
// the collector not exiting when app finishes is normal behavior. without miri-ignore-leaks it will treat it as leak.
