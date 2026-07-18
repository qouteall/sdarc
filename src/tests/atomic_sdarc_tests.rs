use crate::sdarc::{AtomicNullableSdarc, AtomicSdarc, Sdarc};
use crate::collector::collector_update_now;

// ============================================================================
// AtomicNullableSdarc tests
// ============================================================================

#[test]
fn test_atomic_nullable_new_is_none() {
    let atomic: AtomicNullableSdarc<i32> = AtomicNullableSdarc::new();
    let loaded = atomic.load();
    assert!(loaded.is_none());
}

#[test]
fn test_atomic_nullable_new_with_value() {
    let atomic = AtomicNullableSdarc::new_with_value(42i32);
    let loaded = atomic.load();
    assert!(loaded.is_some());
    assert_eq!(*loaded.unwrap(), 42);
}

#[test]
fn test_atomic_nullable_load_after_store() {
    let atomic = AtomicNullableSdarc::new();
    atomic.store(Some(Sdarc::new(100i32)));
    let loaded = atomic.load();
    assert!(loaded.is_some());
    assert_eq!(*loaded.unwrap(), 100);
}

#[test]
fn test_atomic_nullable_store_none() {
    let atomic = AtomicNullableSdarc::new_with_value(42i32);
    atomic.store(None);
    let loaded = atomic.load();
    assert!(loaded.is_none());
}

#[test]
fn test_atomic_nullable_swap() {
    let atomic = AtomicNullableSdarc::new_with_value(10i32);

    // Swap in a new value
    let old = atomic.swap(Some(Sdarc::new(20i32)));
    assert!(old.is_some());
    assert_eq!(*old.unwrap(), 10);

    // Current should be the new value
    let current = atomic.load();
    assert!(current.is_some());
    assert_eq!(*current.unwrap(), 20);
}

#[test]
fn test_atomic_nullable_swap_none_in() {
    let atomic = AtomicNullableSdarc::new_with_value(42i32);
    let old = atomic.swap(None);
    assert!(old.is_some());
    assert_eq!(*old.unwrap(), 42);

    let current = atomic.load();
    assert!(current.is_none());
}

#[test]
fn test_atomic_nullable_swap_none_out() {
    let atomic: AtomicNullableSdarc<i32> = AtomicNullableSdarc::new();
    let old = atomic.swap(Some(Sdarc::new(42i32)));
    assert!(old.is_none());

    let current = atomic.load();
    assert!(current.is_some());
    assert_eq!(*current.unwrap(), 42);
}

#[test]
fn test_atomic_nullable_compare_and_set_success() {
    let atomic = AtomicNullableSdarc::new_with_value(10i32);

    let current = atomic.load();
    let new_val = Some(Sdarc::new(20i32));

    let result = atomic.compare_and_set(&current, &new_val);
    assert!(result.is_ok());
    let old = result.unwrap();
    assert!(old.is_some());
    assert_eq!(*old.unwrap(), 10);

    // The pointer should now hold the new value
    let loaded = atomic.load();
    assert!(loaded.is_some());
    assert_eq!(*loaded.unwrap(), 20);
}

#[test]
fn test_atomic_nullable_compare_and_set_failure() {
    let atomic = AtomicNullableSdarc::new_with_value(10i32);

    // Create a different Sdarc that doesn't match the current pointer
    let non_matching = Some(Sdarc::new(999i32));
    let new_val = Some(Sdarc::new(20i32));

    let result = atomic.compare_and_set(&non_matching, &new_val);
    assert!(result.is_err());

    // The pointer should still hold the original value
    let loaded = atomic.load();
    assert!(loaded.is_some());
    assert_eq!(*loaded.unwrap(), 10);
}

#[test]
fn test_atomic_nullable_compare_and_set_none_to_some() {
    let atomic: AtomicNullableSdarc<i32> = AtomicNullableSdarc::new();
    let if_matches = None;
    let then_set = Some(Sdarc::new(42i32));

    let result = atomic.compare_and_set(&if_matches, &then_set);
    assert!(result.is_ok());
    let old = result.unwrap();
    assert!(old.is_none());

    let loaded = atomic.load();
    assert!(loaded.is_some());
    assert_eq!(*loaded.unwrap(), 42);
}

#[test]
fn test_atomic_nullable_compare_and_set_some_to_none() {
    let atomic = AtomicNullableSdarc::new_with_value(10i32);

    let if_matches = atomic.load();
    let then_set = None;

    let result = atomic.compare_and_set(&if_matches, &then_set);
    assert!(result.is_ok());
    let old = result.unwrap();
    assert!(old.is_some());
    assert_eq!(*old.unwrap(), 10);

    let loaded = atomic.load();
    assert!(loaded.is_none());
}

#[test]
fn test_atomic_nullable_multiple_loads() {
    let atomic = AtomicNullableSdarc::new_with_value(42i32);

    let a = atomic.load();
    let b = atomic.load();
    let c = atomic.load();

    assert!(a.is_some());
    assert!(b.is_some());
    assert!(c.is_some());
    assert_eq!(*a.unwrap(), 42);
    assert_eq!(*b.unwrap(), 42);
    assert_eq!(*c.unwrap(), 42);
}

// ============================================================================
// AtomicSdarc tests (non-nullable)
// ============================================================================

#[test]
fn test_atomic_sdarc_new_and_load() {
    let atomic = AtomicSdarc::new(42i32);
    let loaded = atomic.load();
    assert_eq!(*loaded, 42);
}

#[test]
fn test_atomic_sdarc_swap() {
    let atomic = AtomicSdarc::new(10i32);
    let old = atomic.swap(Sdarc::new(20i32));
    assert_eq!(*old, 10);

    let current = atomic.load();
    assert_eq!(*current, 20);
}

#[test]
fn test_atomic_sdarc_store() {
    let atomic = AtomicSdarc::new(10i32);
    atomic.store(Sdarc::new(30i32));

    let current = atomic.load();
    assert_eq!(*current, 30);
}

#[test]
fn test_atomic_sdarc_compare_and_set_success() {
    let atomic = AtomicSdarc::new(10i32);

    let if_matches = atomic.load();
    let then_set = Sdarc::new(20i32);

    let result = atomic.compare_and_set(&if_matches, &then_set);
    assert!(result.is_ok());
    let old = result.unwrap();
    assert_eq!(*old, 10);

    let current = atomic.load();
    assert_eq!(*current, 20);
}

#[test]
fn test_atomic_sdarc_compare_and_set_failure() {
    let atomic = AtomicSdarc::new(10i32);

    let non_matching = Sdarc::new(999i32);
    let then_set = Sdarc::new(20i32);

    let result = atomic.compare_and_set(&non_matching, &then_set);
    assert!(result.is_err());

    let current = atomic.load();
    assert_eq!(*current, 10);
}

#[test]
fn test_atomic_sdarc_multiple_swaps() {
    let atomic = AtomicSdarc::new(1i32);

    for i in 2..10 {
        let old = atomic.swap(Sdarc::new(i));
        assert_eq!(*old, i - 1);
    }

    assert_eq!(*atomic.load(), 9);
}

// ============================================================================
// Thread-safety tests
// ============================================================================

#[test]
fn test_atomic_nullable_sdarc_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<AtomicNullableSdarc<i32>>();
}

#[test]
fn test_atomic_nullable_sdarc_is_sync() {
    fn assert_sync<T: Sync>() {}
    assert_sync::<AtomicNullableSdarc<i32>>();
}

#[test]
fn test_atomic_sdarc_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<AtomicSdarc<i32>>();
}

#[test]
fn test_atomic_sdarc_is_sync() {
    fn assert_sync<T: Sync>() {}
    assert_sync::<AtomicSdarc<i32>>();
}

#[test]
fn test_atomic_nullable_concurrent_loads() {
    use std::sync::Arc;
    use std::thread;

    let atomic = Arc::new(AtomicNullableSdarc::new_with_value(42i32));

    let mut handles = vec![];
    for _ in 0..4 {
        let atomic_clone = Arc::clone(&atomic);
        handles.push(thread::spawn(move || {
            for _ in 0..100 {
                let loaded = atomic_clone.load();
                assert!(loaded.is_some());
                assert_eq!(*loaded.unwrap(), 42);
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn test_atomic_sdarc_concurrent_loads() {
    use std::sync::Arc;
    use std::thread;

    let atomic = Arc::new(AtomicSdarc::new(42i32));

    let mut handles = vec![];
    for _ in 0..4 {
        let atomic_clone = Arc::clone(&atomic);
        handles.push(thread::spawn(move || {
            for _ in 0..100 {
                let loaded = atomic_clone.load();
                assert_eq!(*loaded, 42);
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn test_atomic_nullable_drop_cleans_up() {
    // Just ensure Drop doesn't panic or leak
    let atomic = AtomicNullableSdarc::new_with_value(String::from("hello"));
    atomic.store(Some(Sdarc::new(String::from("world"))));
    drop(atomic);
    collector_update_now();
}

#[test]
fn test_atomic_sdarc_drop_cleans_up() {
    // Just ensure Drop doesn't panic or leak
    let atomic = AtomicSdarc::new(String::from("hello"));
    atomic.store(Sdarc::new(String::from("world")));
    drop(atomic);
    collector_update_now();
}
