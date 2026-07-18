use crate::sharded_rwlock::ShardedRwLock;

#[test]
fn test_new_and_read() {
    let lock = ShardedRwLock::new(42i32);
    let guard = lock.read();
    assert_eq!(*guard, 42);
}

#[test]
fn test_new_and_write() {
    let lock = ShardedRwLock::new(42i32);
    let guard = lock.write();
    assert_eq!(*guard, 42);
}

#[test]
fn test_read_guard_deref() {
    let lock = ShardedRwLock::new(String::from("hello"));
    let guard = lock.read();
    assert_eq!(guard.as_str(), "hello");
    assert_eq!(guard.len(), 5);
}

#[test]
fn test_write_guard_deref() {
    let lock = ShardedRwLock::new(String::from("hello"));
    let guard = lock.write();
    assert_eq!(guard.as_str(), "hello");
}

#[test]
fn test_write_guard_deref_mut() {
    let lock = ShardedRwLock::new(String::from("hello"));
    let mut guard = lock.write();
    guard.push_str(" world");
    assert_eq!(guard.as_str(), "hello world");
    drop(guard);

    // After dropping write guard, read should see the modification
    let read_guard = lock.read();
    assert_eq!(read_guard.as_str(), "hello world");
}

#[test]
fn test_write_then_read() {
    let lock = ShardedRwLock::new(10i32);

    {
        let mut w = lock.write();
        *w = 20;
    }

    let r = lock.read();
    assert_eq!(*r, 20);
}

#[test]
fn test_multiple_reads() {
    let lock = ShardedRwLock::new(42i32);

    let r1 = lock.read();
    let r2 = lock.read();
    let r3 = lock.read();

    assert_eq!(*r1, 42);
    assert_eq!(*r2, 42);
    assert_eq!(*r3, 42);
}

#[test]
fn test_try_read_success() {
    let lock = ShardedRwLock::new(42i32);
    let guard = lock.try_read();
    assert!(guard.is_some());
    assert_eq!(*guard.unwrap(), 42);
}

#[test]
fn test_try_read_while_reading() {
    let lock = ShardedRwLock::new(42i32);

    // Hold a read lock
    let _r1 = lock.read();

    // Another try_read should succeed because reads are shared
    let r2 = lock.try_read();
    assert!(r2.is_some());
    assert_eq!(*r2.unwrap(), 42);
}

#[test]
fn test_try_read_while_writing_fails() {
    let lock = ShardedRwLock::new(42i32);

    // Hold a write lock
    let _w = lock.write();

    // try_read should fail because write is exclusive
    let r = lock.try_read();
    assert!(r.is_none());
}

#[test]
fn test_with_custom_struct() {
    #[derive(Debug, PartialEq)]
    struct Foo {
        x: i32,
        y: String,
    }

    let lock = ShardedRwLock::new(Foo {
        x: 10,
        y: "bar".to_string(),
    });

    {
        let guard = lock.read();
        assert_eq!(guard.x, 10);
        assert_eq!(guard.y, "bar");
    }

    {
        let mut guard = lock.write();
        guard.x = 20;
        guard.y = "baz".to_string();
    }

    {
        let guard = lock.read();
        assert_eq!(guard.x, 20);
        assert_eq!(guard.y, "baz");
    }
}

#[test]
fn test_sharded_rwlock_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<ShardedRwLock<i32>>();
}

#[test]
fn test_sharded_rwlock_is_sync() {
    fn assert_sync<T: Sync>() {}
    assert_sync::<ShardedRwLock<i32>>();
}

#[test]
fn test_concurrent_reads_from_threads() {
    use std::sync::Arc;
    use std::thread;

    let lock = Arc::new(ShardedRwLock::new(42i32));
    let mut handles = vec![];

    for _ in 0..8 {
        let lock_clone = Arc::clone(&lock);
        handles.push(thread::spawn(move || {
            for _ in 0..100 {
                let guard = lock_clone.read();
                assert_eq!(*guard, 42);
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn test_write_exclusive_across_threads() {
    use std::sync::Arc;
    use std::thread;
    use std::sync::atomic::{AtomicI32, Ordering};

    let lock = Arc::new(ShardedRwLock::new(AtomicI32::new(0)));
    let mut handles = vec![];

    for _ in 0..4 {
        let lock_clone = Arc::clone(&lock);
        handles.push(thread::spawn(move || {
            for _ in 0..50 {
                let guard = lock_clone.write();
                guard.fetch_add(1, Ordering::SeqCst);
                // Write guard is held; other threads are blocked on write
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let guard = lock.read();
    assert_eq!(guard.load(Ordering::SeqCst), 200);
}

#[test]
fn test_read_guard_drop_allows_write() {
    let lock = ShardedRwLock::new(42i32);

    let r = lock.read();
    assert_eq!(*r, 42);
    drop(r);

    // After dropping read guard, should be able to acquire write
    let mut w = lock.write();
    *w = 100;
    drop(w);

    let r = lock.read();
    assert_eq!(*r, 100);
}

#[test]
fn test_write_guard_drop_allows_read() {
    let lock = ShardedRwLock::new(42i32);

    {
        let mut w = lock.write();
        *w = 99;
    }

    let r = lock.read();
    assert_eq!(*r, 99);
}

#[test]
fn test_with_vec_type() {
    let lock = ShardedRwLock::new(vec![1, 2, 3]);

    {
        let mut w = lock.write();
        w.push(4);
        w.push(5);
    }

    let r = lock.read();
    assert_eq!(*r, vec![1, 2, 3, 4, 5]);
}
