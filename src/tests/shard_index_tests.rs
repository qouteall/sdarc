use crate::shard_index::{
    ShardIndex, ShardsArr, curr_thread_shard_index, get_shard_count,
    set_current_thread_shard_index, shard_indexes, shard_indexes_until,
};

#[test]
fn test_shard_index_from_u64() {
    let shard_count = get_shard_count().0 as u64;
    let index = ShardIndex::from_u64(0);
    assert_eq!(index.as_8(), 0);

    let index = ShardIndex::from_u64(shard_count);
    assert_eq!(index.as_8(), 0); // wraps around via modulo

    let index = ShardIndex::from_u64(1);
    if shard_count > 1 {
        assert_eq!(index.as_8(), 1);
    } else {
        assert_eq!(index.as_8(), 0);
    }
}

#[test]
fn test_shard_index_as_usize() {
    let index = ShardIndex::from_u64(5);
    assert_eq!(index.as_usize(), index.as_8() as usize);
}

#[test]
fn test_shard_index_from_bounded_u8() {
    // This should succeed
    let index = ShardIndex::from_bounded_u8(0);
    assert_eq!(index.as_8(), 0);
}

#[test]
fn test_shard_index_from_bounded_u8_out_of_range() {
    let count = get_shard_count().0;
    // When shard count is 256 (MAX), all u8 values are valid, so we can't test this case.
    if count < 256 {
        // Using a value >= shard count should panic
        let bad_value = count as u8;
        let result = std::panic::catch_unwind(|| {
            ShardIndex::from_bounded_u8(bad_value);
        });
        assert!(result.is_err(), "Expected panic for out-of-range u8 value");
    }
}

#[test]
fn test_curr_thread_shard_index() {
    let index = curr_thread_shard_index();
    // Should always be within range
    assert!((index.as_8() as u16) < get_shard_count().0);
}

#[test]
fn test_set_current_thread_shard_index() {
    let original = curr_thread_shard_index();

    // Set to a known index
    let new_index = ShardIndex::from_bounded_u8(0);
    set_current_thread_shard_index(new_index);
    assert_eq!(curr_thread_shard_index(), new_index);

    // Restore original
    set_current_thread_shard_index(original);
    assert_eq!(curr_thread_shard_index(), original);
}

#[test]
fn test_shard_indexes_iteration() {
    let shard_count = get_shard_count().as_usize();
    let indexes: Vec<ShardIndex> = shard_indexes().collect();

    assert_eq!(indexes.len(), shard_count);
    for (i, index) in indexes.iter().enumerate() {
        assert_eq!(index.as_8(), i as u8);
    }
}

#[test]
fn test_shard_indexes_until() {
    let shard_count = get_shard_count();
    let mid = ShardIndex::from_bounded_u8((shard_count.0 / 2) as u8);

    let indexes: Vec<ShardIndex> = shard_indexes_until(mid).collect();
    assert_eq!(indexes.len(), mid.as_usize());
    for (i, index) in indexes.iter().enumerate() {
        assert_eq!(index.as_8(), i as u8);
    }
}

#[test]
fn test_shard_indexes_until_zero() {
    let zero = ShardIndex::from_bounded_u8(0);
    let indexes: Vec<ShardIndex> = shard_indexes_until(zero).collect();
    assert!(indexes.is_empty());
}

#[test]
fn test_shard_index_ordering() {
    let a = ShardIndex::from_bounded_u8(0);
    let b = ShardIndex::from_bounded_u8(1);
    assert!(a < b);
    assert!(a <= b);
    assert!(b > a);
}

#[test]
fn test_shard_index_clone_and_copy() {
    let a = ShardIndex::from_bounded_u8(0);
    let b = a; // Copy
    let c = a.clone(); // Clone
    assert_eq!(a, b);
    assert_eq!(a, c);
}

#[test]
fn test_shard_index_debug() {
    let index = ShardIndex::from_bounded_u8(0);
    let debug_str = format!("{index:?}");
    assert!(debug_str.contains("ShardIndex"));
}

#[test]
fn test_shard_count_as_usize() {
    let count = get_shard_count();
    assert_eq!(count.as_usize(), count.0 as usize);
}

// ============================================================================
// ShardsArr tests
// ============================================================================

#[test]
fn test_shards_arr_new_and_index() {
    let arr = ShardsArr::new(|i| (i.as_8() as i32) * 10);

    let shard_count = get_shard_count();
    for i in 0..shard_count.0 {
        let index = ShardIndex::from_bounded_u8(i as u8);
        assert_eq!(arr[index], (i as i32) * 10);
    }
}

#[test]
fn test_shards_arr_at_curr_thread_shard() {
    let arr = ShardsArr::new(|_i| 42i32);
    assert_eq!(*arr.at_curr_thread_shard(), 42);
}

#[test]
fn test_shards_arr_with_different_shard_index() {
    let arr = ShardsArr::new(|i| format!("shard-{}", i.as_8()));

    let original = curr_thread_shard_index();

    let index = ShardIndex::from_bounded_u8(0);
    set_current_thread_shard_index(index);
    assert_eq!(arr.at_curr_thread_shard(), "shard-0");

    // Restore
    set_current_thread_shard_index(original);
}

#[test]
fn test_shards_arr_modification() {
    use std::cell::Cell;

    let arr = ShardsArr::new(|_| Cell::new(0i32));

    // Modify through index
    let index = ShardIndex::from_bounded_u8(0);
    arr[index].set(42);
    assert_eq!(arr[index].get(), 42);
}

#[test]
fn test_shards_arr_stores_different_values_per_shard() {
    let arr = ShardsArr::new(|i| (i.as_8() as u64) * 100);

    let shard_count = get_shard_count();
    for i in 0..shard_count.0 {
        let index = ShardIndex::from_bounded_u8(i as u8);
        assert_eq!(arr[index], (i as u64) * 100);
    }
}

#[test]
fn test_set_then_get_current_thread_shard_index() {
    let original = curr_thread_shard_index();

    for i in 0..get_shard_count().0 {
        let index = ShardIndex::from_bounded_u8(i as u8);
        set_current_thread_shard_index(index);
        assert_eq!(curr_thread_shard_index(), index);
    }

    set_current_thread_shard_index(original);
}
