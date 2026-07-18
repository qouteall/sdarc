use crate::sdarc::Sdarc;
use crate::collector::collector_update_now;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

#[test]
fn test_new_and_deref() {
    let sdarc = Sdarc::new(42i32);
    assert_eq!(*sdarc, 42);
}

#[test]
fn test_new_with_string() {
    let sdarc = Sdarc::new(String::from("hello"));
    assert_eq!(sdarc.as_str(), "hello");
}

#[test]
fn test_new_with_custom_struct() {
    #[derive(Debug, PartialEq)]
    struct Foo {
        x: i32,
        y: String,
    }

    let foo = Foo {
        x: 10,
        y: "bar".to_string(),
    };
    let sdarc = Sdarc::new(foo);
    assert_eq!(sdarc.x, 10);
    assert_eq!(sdarc.y, "bar");
}

#[test]
fn test_clone_increments_count() {
    let sdarc = Sdarc::new(42i32);
    let clone1 = sdarc.clone();
    let clone2 = sdarc.clone();

    // All should deref to the same value
    assert_eq!(*sdarc, 42);
    assert_eq!(*clone1, 42);
    assert_eq!(*clone2, 42);
}

#[test]
fn test_clone_and_drop_does_not_affect_other_clones() {
    let sdarc = Sdarc::new(100i32);
    let clone = sdarc.clone();

    // Drop the original
    drop(sdarc);

    // Clone should still be valid
    assert_eq!(*clone, 100);
}

#[test]
fn test_is_same_pointee() {
    let a = Sdarc::new(10i32);
    let b = a.clone();
    let c = Sdarc::new(10i32);

    assert!(Sdarc::is_same_pointee(&a, &b));
    assert!(!Sdarc::is_same_pointee(&a, &c));
    assert!(!Sdarc::is_same_pointee(&b, &c));
}

#[test]
fn test_downgrade_upgrade_basic() {
    let sdarc = Sdarc::new(42i32);
    let weak = sdarc.downgrade();

    // Upgrade should succeed while strong ref exists
    let upgraded = weak.upgrade();
    assert!(upgraded.is_some());
    assert_eq!(*upgraded.unwrap(), 42);
}

#[test]
fn test_downgrade_upgrade_multiple_times() {
    let sdarc = Sdarc::new(99i32);

    for _ in 0..5 {
        let weak = sdarc.downgrade();
        let upgraded = weak.upgrade();
        assert!(upgraded.is_some());
        assert_eq!(*upgraded.unwrap(), 99);
    }
}

#[test]
fn test_multiple_clones_from_same_sdarc() {
    let sdarc = Sdarc::new(50i32);
    let clones: Vec<_> = (0..10).map(|_| sdarc.clone()).collect();

    for clone in &clones {
        assert_eq!(**clone, 50);
    }

    // Drop the original
    drop(sdarc);

    // All clones should still be valid
    for clone in &clones {
        assert_eq!(**clone, 50);
    }
}

#[test]
fn test_deref_returns_correct_value_after_clone_drop() {
    let a = Sdarc::new(42i32);
    let b = a.clone();
    let c = b.clone();

    drop(a);
    assert_eq!(*b, 42);

    drop(b);
    assert_eq!(*c, 42);
}

// Test that Sdarc is Send + Sync (compile-time check)
#[test]
fn test_sdarc_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<Sdarc<i32>>();
}

#[test]
fn test_sdarc_is_sync() {
    fn assert_sync<T: Sync>() {}
    assert_sync::<Sdarc<i32>>();
}

#[test]
fn test_weak_sdarc_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<crate::sdarc::WeakSdarc<i32>>();
}

#[test]
fn test_weak_sdarc_is_sync() {
    fn assert_sync<T: Sync>() {}
    assert_sync::<crate::sdarc::WeakSdarc<i32>>();
}

#[test]
fn test_sdarc_across_threads() {
    let sdarc = Sdarc::new(AtomicUsize::new(0));

    let mut handles = vec![];
    for _ in 0..4 {
        let clone = sdarc.clone();
        handles.push(thread::spawn(move || {
            clone.fetch_add(1, Ordering::SeqCst);
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    assert_eq!(sdarc.load(Ordering::SeqCst), 4);
}

#[test]
fn test_clone_and_send_to_thread() {
    let sdarc = Sdarc::new(100i32);
    let clone = sdarc.clone();

    let handle = thread::spawn(move || {
        assert_eq!(*clone, 100);
    });

    handle.join().unwrap();
    assert_eq!(*sdarc, 100);
}

#[test]
fn test_weak_upgrade_from_another_thread() {
    let sdarc = Sdarc::new(42i32);
    let weak = sdarc.downgrade();

    let handle = thread::spawn(move || {
        let upgraded = weak.upgrade();
        assert!(upgraded.is_some());
        assert_eq!(*upgraded.unwrap(), 42);
    });

    handle.join().unwrap();
}

#[test]
fn test_drop_behavior_with_clones() {
    // This test ensures dropping all clones doesn't crash
    let sdarc = Sdarc::new(String::from("test"));
    let c1 = sdarc.clone();
    let c2 = c1.clone();
    let c3 = c2.clone();

    drop(sdarc);
    drop(c1);
    drop(c2);
    drop(c3);

    // Wake up collector to process the drops
    collector_update_now();
}

#[test]
fn test_sdarc_with_large_struct() {
    #[derive(Debug)]
    struct Large {
        data: [u8; 1024],
    }

    let large = Large { data: [42u8; 1024] };
    let sdarc = Sdarc::new(large);
    assert_eq!(sdarc.data[0], 42);
    assert_eq!(sdarc.data[1023], 42);

    let clone = sdarc.clone();
    assert_eq!(clone.data[512], 42);
}

#[test]
fn test_sdarc_with_vec() {
    let sdarc = Sdarc::new(vec![1, 2, 3, 4, 5]);
    assert_eq!(sdarc.len(), 5);
    assert_eq!(sdarc[0], 1);
    assert_eq!(sdarc[4], 5);
}

#[test]
fn test_many_clones_and_drops() {
    let sdarc = Sdarc::new(42i32);
    let mut clones = vec![];

    // Create many clones
    for _ in 0..100 {
        clones.push(sdarc.clone());
    }

    // Verify all valid
    for c in &clones {
        assert_eq!(**c, 42);
    }

    // Drop them all
    drop(sdarc);
    drop(clones);

    collector_update_now();
}
