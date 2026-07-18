use crate::tagged_counter::{AtomicTaggedCounter, TaggedCounter};

#[test]
fn test_new_tagged_counter_is_zero() {
    let counter = AtomicTaggedCounter::new();
    let value = counter.load_relaxed();
    assert_eq!(value.ref_count(), 0);
    assert!(!value.tag());
}

#[test]
fn test_increment_ref_count_relaxed() {
    let counter = AtomicTaggedCounter::new();
    counter.increment_ref_count_relaxed();
    let value = counter.load_relaxed();
    assert_eq!(value.ref_count(), 1);
    assert!(!value.tag());
}

#[test]
fn test_multiple_increments() {
    let counter = AtomicTaggedCounter::new();
    for _ in 0..5 {
        counter.increment_ref_count_relaxed();
    }
    let value = counter.load_relaxed();
    assert_eq!(value.ref_count(), 5);
    assert!(!value.tag());
}

#[test]
fn test_decrement_ref_count_and_set_tag_release() {
    let counter = AtomicTaggedCounter::new();
    // First increment so we have a positive count to decrement
    counter.increment_ref_count_relaxed();
    counter.increment_ref_count_relaxed();

    counter.decrement_ref_count_and_set_tag_release();
    let value = counter.load_acquire();
    // After one decrement, ref count should be 1, tag set
    assert_eq!(value.ref_count(), 1);
    assert!(value.tag());
}

#[test]
fn test_decrement_to_zero_with_tag() {
    let counter = AtomicTaggedCounter::new();
    counter.increment_ref_count_relaxed();

    counter.decrement_ref_count_and_set_tag_release();
    let value = counter.load_acquire();
    assert_eq!(value.ref_count(), 0);
    assert!(value.tag());
}

#[test]
fn test_decrement_to_negative() {
    let counter = AtomicTaggedCounter::new();
    // Decrement below zero is valid in this design (one shard can go negative)
    counter.decrement_ref_count_and_set_tag_release();
    let value = counter.load_acquire();
    assert_eq!(value.ref_count(), -1);
    assert!(value.tag());
}

#[test]
fn test_fetch_and_clear_tag_relaxed() {
    let counter = AtomicTaggedCounter::new();
    counter.increment_ref_count_relaxed();
    counter.decrement_ref_count_and_set_tag_release();

    // Tag should be set
    let before = counter.load_relaxed();
    assert!(before.tag());

    // Clear the tag
    let cleared = counter.fetch_and_clear_tag_relaxed();
    assert!(cleared.tag()); // returned value had tag set

    // After clearing, tag should be unset, ref count unchanged
    let after = counter.load_relaxed();
    assert!(!after.tag());
    assert_eq!(after.ref_count(), cleared.ref_count());
}

#[test]
fn test_fetch_and_clear_tag_when_already_clear() {
    let counter = AtomicTaggedCounter::new();
    counter.increment_ref_count_relaxed();

    // Tag is not set
    let before = counter.load_relaxed();
    assert!(!before.tag());

    let cleared = counter.fetch_and_clear_tag_relaxed();
    assert!(!cleared.tag());
    assert_eq!(cleared.ref_count(), before.ref_count());
}

#[test]
fn test_load_relaxed_vs_acquire() {
    let counter = AtomicTaggedCounter::new();
    counter.increment_ref_count_relaxed();
    counter.increment_ref_count_relaxed();

    // Both should return the same value when no concurrent modifications
    let relaxed = counter.load_relaxed();
    let acquire = counter.load_acquire();
    assert_eq!(relaxed.0, acquire.0);
    assert_eq!(relaxed.ref_count(), acquire.ref_count());
    assert_eq!(relaxed.tag(), acquire.tag());
}

#[test]
fn test_tagged_counter_ref_count_method() {
    // Test the ref_count extraction from various values
    // ref_count = value >> 1 (sign preserved)
    let tc = TaggedCounter(0);
    assert_eq!(tc.ref_count(), 0);
    assert!(!tc.tag());

    let tc = TaggedCounter(2);
    assert_eq!(tc.ref_count(), 1);
    assert!(!tc.tag());

    let tc = TaggedCounter(4);
    assert_eq!(tc.ref_count(), 2);
    assert!(!tc.tag());

    let tc = TaggedCounter(-2);
    assert_eq!(tc.ref_count(), -1);
    assert!(!tc.tag());

    let tc = TaggedCounter(1);
    assert_eq!(tc.ref_count(), 0);
    assert!(tc.tag());

    let tc = TaggedCounter(3);
    assert_eq!(tc.ref_count(), 1);
    assert!(tc.tag());

    let tc = TaggedCounter(-1);
    assert_eq!(tc.ref_count(), -1);
    assert!(tc.tag());
}

#[test]
fn test_tagged_counter_tag_method() {
    assert!(!TaggedCounter(0).tag());
    assert!(!TaggedCounter(2).tag());
    assert!(!TaggedCounter(-2).tag());
    assert!(TaggedCounter(1).tag());
    assert!(TaggedCounter(3).tag());
    assert!(TaggedCounter(-1).tag());
}

#[test]
fn test_increment_decrement_sequence() {
    let counter = AtomicTaggedCounter::new();

    // Simulate: clone (inc), clone (inc), drop (dec+tag), drop (dec+tag)
    counter.increment_ref_count_relaxed(); // count = 1
    counter.increment_ref_count_relaxed(); // count = 2
    counter.decrement_ref_count_and_set_tag_release(); // count = 1, tag set
    counter.decrement_ref_count_and_set_tag_release(); // count = 0, tag set

    let value = counter.load_acquire();
    assert_eq!(value.ref_count(), 0);
    assert!(value.tag());

    // Clear tag
    counter.fetch_and_clear_tag_relaxed();
    let after_clear = counter.load_relaxed();
    assert_eq!(after_clear.ref_count(), 0);
    assert!(!after_clear.tag());
}

#[test]
fn test_decrement_preserves_existing_tag() {
    let counter = AtomicTaggedCounter::new();
    counter.increment_ref_count_relaxed(); // count = 1
    counter.increment_ref_count_relaxed(); // count = 2

    // First decrement sets tag
    counter.decrement_ref_count_and_set_tag_release(); // count = 1, tag=1

    // Second decrement should keep tag set
    counter.decrement_ref_count_and_set_tag_release(); // count = 0, tag=1

    let value = counter.load_acquire();
    assert_eq!(value.ref_count(), 0);
    assert!(value.tag());
}

#[test]
fn test_concurrent_increments() {
    use std::sync::Arc;
    use std::thread;

    let counter = Arc::new(AtomicTaggedCounter::new());
    let num_threads = 8;
    let increments_per_thread = 1000;

    let mut handles = vec![];
    for _ in 0..num_threads {
        let c = Arc::clone(&counter);
        handles.push(thread::spawn(move || {
            for _ in 0..increments_per_thread {
                c.increment_ref_count_relaxed();
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let value = counter.load_relaxed();
    assert_eq!(value.ref_count(), (num_threads * increments_per_thread) as i64);
    assert!(!value.tag());
}
