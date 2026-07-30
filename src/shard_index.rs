use crate::env_params::shard_count_from_env_var;
use std::cell::Cell;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::{Index, IndexMut};
use std::sync::LazyLock;
use std::thread;

// Shard count is power of 2. Because that we need to frequently turn CPU index to shared index.
// Computing modulo to non-constant is slow.
#[derive(Copy, Clone, Debug)]
pub struct ShardCount {
    exponent_of_two: u8,
    mask: usize,
}

pub(crate) const MAX_SHARD_COUNT: usize = 256;

const MIN_EXPONENT: u32 = 1;
const MAX_EXPONENT: u32 = 8;

impl ShardCount {
    pub fn from_exponent_adjusted(exponent: u32) -> ShardCount {
        let in_range = exponent.clamp(MIN_EXPONENT, MAX_EXPONENT);
        ShardCount {
            exponent_of_two: in_range as u8,
            mask: (1 << in_range) - 1,
        }
    }

    pub fn from_usize_adjusted(num: usize) -> ShardCount {
        assert_ne!(num, 0);

        if num.count_ones() == 1 {
            // it's a power of 2
            Self::from_exponent_adjusted(num.trailing_zeros())
        } else {
            // not a power of two. we need to round up. ilog2 rounds down, so add 1
            Self::from_exponent_adjusted(num.ilog2() + 1)
        }
    }

    pub fn as_usize(self) -> usize {
        1 << (self.exponent_of_two as usize)
    }

    pub fn modulo(self, num: usize) -> ShardIndex {
        ShardIndex((num & self.mask) as u8)
    }
}

static SHARD_COUNT: LazyLock<ShardCount> = LazyLock::new(init_shard_count);

fn init_shard_count() -> ShardCount {
    if let Some(c) = shard_count_from_env_var() {
        return c;
    }

    let available_parallelism: usize = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    assert_ne!(available_parallelism, 0);

    ShardCount::from_usize_adjusted(available_parallelism)
}

/// The shard count won't change after initialization
pub fn get_shard_count() -> ShardCount {
    *SHARD_COUNT
}

/// It's u8 because shard count can be at most 256.
///
/// It's ensured that the number is smaller than shard count.
#[derive(Copy, Clone, Debug, Ord, PartialOrd, Eq, PartialEq)]
pub struct ShardIndex(u8);

impl ShardIndex {
    pub fn from_usize(value: usize) -> ShardIndex {
        get_shard_count().modulo(value)
    }

    pub fn from_u64(value: u64) -> ShardIndex {
        Self::from_usize(value as usize)
    }

    pub fn as_8(self) -> u8 {
        self.0
    }

    pub fn as_usize(self) -> usize {
        self.0 as usize
    }
}

#[cfg(not(target_os = "linux"))]
thread_local! {
    static SHARD_INDEX_FROM_THREAD_ID_HASH: Cell<ShardIndex> = {
        Cell::new(shard_index_from_thread_id_hash())
    };
}

#[cfg(not(target_os = "linux"))]
fn shard_index_from_thread_id_hash() -> ShardIndex {
    let thread_id = thread::current().id();
    let mut hasher = DefaultHasher::new();
    thread_id.hash(&mut hasher);
    let value: u64 = hasher.finish();

    ShardIndex::from_u64(value)
}

/// Get the current thread's shard index. It's computed from thread id hash.
///
/// It will have some collisions. Different threads may use the same shard index.
#[cfg(not(target_os = "linux"))]
pub fn curr_thread_shard_index() -> ShardIndex {
    SHARD_INDEX_FROM_THREAD_ID_HASH.get()
}

/// In Linux it uses sched_getcpu to get shard index.
///
/// The sched_getcpu normally uses vdso which doesn't involve system call.
/// It's kind of cheap so it can be used frequently.
///
/// It doesn't use thread id hash. So it's free of shard index collision.
///
/// Even if shard index uses CPU index, the counter is still atomic, unlike Linux kernel's percpu-refcount,
/// because the thread can be scheduled to another core right after calling sched_getcpu.
#[cfg(target_os = "linux")]
pub fn curr_thread_shard_index() -> ShardIndex {
    get_shard_count().modulo(rustix::thread::sched_getcpu())
    // Side notes:
    // The CPU index can be obtained without function call. https://docs.rs/percpu/latest/percpu/
    // but it requires custom linker script.
    //
    // The `rustix::thread::sched_getcpu()` only supports Linux, Android and dragonfly.
    // Side note: in rustix's `#[cfg(any(linux_kernel, target_os = "dragonfly"))]`
    // the `linux_kernel` is kind of misleading. The `linux_kernel` has nothing to do with Rust-for-Linux.
    // It's activated for user application for Linux (also Android).
}

pub fn shard_indexes() -> impl Iterator<Item = ShardIndex> {
    (0..get_shard_count().as_usize()).map(|i| ShardIndex(i as u8))
}

#[allow(clippy::redundant_closure)]
pub fn shard_indexes_until(shard_index: ShardIndex) -> impl Iterator<Item = ShardIndex> {
    assert!((shard_index.0 as usize) <= get_shard_count().as_usize());
    (0..shard_index.0).map(|i| ShardIndex(i))
}

/// A helper type that wraps heap-allocated slice so that you can use ShardIndex as index. The user no longer need to convert ShardIndex to usize.
/// The elements will be contiguous in memory, unlike [`crate::sharded_alloc::ShardedBox`]
pub struct ShardsArr<T>(pub Box<[T]>);

impl<T> ShardsArr<T> {
    pub fn new(init_fn: impl Fn(ShardIndex) -> T) -> ShardsArr<T> {
        let shard_count = get_shard_count().as_usize();

        let mut vec: Vec<T> = Vec::with_capacity(shard_count);

        for shard_index in shard_indexes() {
            vec.push(init_fn(shard_index));
        }

        assert_eq!(vec.len(), shard_count);

        ShardsArr(vec.into_boxed_slice())
    }

    pub fn at_curr_thread_shard(&self) -> &T {
        &self.0[curr_thread_shard_index().as_usize()]
    }
}

impl<T> Index<ShardIndex> for ShardsArr<T> {
    type Output = T;

    fn index(&self, index: ShardIndex) -> &Self::Output {
        &self.0[index.as_usize()]
    }
}

impl<T> IndexMut<ShardIndex> for ShardsArr<T> {
    fn index_mut(&mut self, index: ShardIndex) -> &mut Self::Output {
        &mut self.0[index.as_usize()]
    }
}
