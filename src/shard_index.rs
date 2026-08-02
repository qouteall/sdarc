use crate::env_params::shard_count_from_env_var;
use std::ops::{Index, IndexMut};
use std::sync::LazyLock;
use std::thread;

// Shard count is power of 2. Because we need to frequently turn CPU index to shard index.
// Computing modulo to non-constant non-power-of-2 is slow.
#[derive(Copy, Clone, Debug)]
pub struct ShardCount {
    shard_count: usize,
    mask: usize,
}

pub(crate) const MAX_SHARD_COUNT: usize = 256;

const MIN_EXPONENT: u32 = 0;
const MAX_EXPONENT: u32 = 8;

impl ShardCount {
    pub fn from_exponent_adjusted(exponent: u32) -> ShardCount {
        let exponent_clamped = exponent.clamp(MIN_EXPONENT, MAX_EXPONENT);
        let shard_count: usize = 1 << (exponent_clamped);
        ShardCount {
            shard_count,
            mask: shard_count - 1,
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
        self.shard_count
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

    pub fn as_u8(self) -> u8 {
        self.0
    }

    pub fn as_usize(self) -> usize {
        self.0 as usize
    }
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

    pub fn at_curr_shard(&self) -> &T {
        &self.0[curr_shard_index().as_usize()]
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

// Platform-specific `curr_shard_index` -----

cfg_if::cfg_if! {
    if #[cfg(target_os = "windows")] {
        /// In Windows, the current shard index is obtained by `GetCurrentProcessorNumber`
        ///
        /// The Microsoft documentation doesn't mention whether `GetCurrentProcessorNumber` uses syscall. But according to [this blog](https://www.alex-ionescu.com/solution-to-challenge/) the reverse-engineered machine code contains no syscall in X86 (except WOW64, which only happens to 32-bit applications).
        /// So it should be fast enough. (Not sure in ARM.)
        pub fn curr_shard_index() -> ShardIndex {
            let cpu_index: u32 = unsafe { windows_sys::Win32::System::Threading::GetCurrentProcessorNumber() };
            get_shard_count().modulo(cpu_index as usize)
        }

        pub const DOES_SHARD_INDEX_USE_CPU_INDEX: bool = true;
    } else if #[cfg(all(
        target_os = "linux",
        // musl does a real syscall in `sched_getcpu` on aarch64, which is slow,
        // so that case falls back to thread id hash below.
        not(all(target_env = "musl", target_arch = "aarch64"))
    ))] {
        /// In Linux, the current shard index is obtained by libc `sched_getcpu`.
        /// In common cases, `sched_getcpu` does not do syscall. It's fast enough.
        ///
        /// Specifically, glibc uses rseq to get CPU index.
        /// musl uses vdso to get CPU index in X86-64, no syscall.
        /// However, in ARM64(aarch64), musl does syscall in `sched_getcpu`, which is slow,
        /// so musl aarch64 is excluded here and uses the thread id hash fallback below.
        pub fn curr_shard_index() -> ShardIndex {
            use std::ffi::c_int;

            // Side note: rustix also has sched_getcpu but it uses syscall in aarch64 which is slow
            let cpu_index: c_int = unsafe { libc::sched_getcpu() };
            get_shard_count().modulo(cpu_index as usize)
        }

        pub const DOES_SHARD_INDEX_USE_CPU_INDEX: bool = true;
    } else {
        thread_local! {
            static SHARD_INDEX_FROM_THREAD_ID_HASH: ShardIndex = shard_index_from_thread_id_hash();
        }

        fn shard_index_from_thread_id_hash() -> ShardIndex {
            use std::hash::{DefaultHasher, Hash, Hasher};

            let thread_id = thread::current().id();
            let mut hasher = DefaultHasher::new();
            thread_id.hash(&mut hasher);
            let value: u64 = hasher.finish();

            ShardIndex::from_u64(value)
        }

        /// Fallback case: use thread id hash modulo shard count as shard index. Cached in thread local.
        ///
        /// Used on non-Linux non-Windows platforms,
        /// and on musl aarch64 Linux (because musl in aarch64 does syscall in `sched_getcpu`, which is slow).
        ///
        /// It will have more contention. The shard index is fixed per thread,
        /// and different threads' hash modulo shard count may be same.
        ///
        /// If there are T threads running and S shards,
        /// assume that each thread's shard index is random,
        /// probability that one thread doesn't contend one shard with another thread is: ((S-1)/S)\^(T-1)
        ///
        /// The expected number of threads that doesn't contend shard is: T \* ((S-1)/S)\^(T-1)
        ///
        /// Specifically, when there are 32 shards and 32 threads, about 12 threads won't contend, about 20 threads will contend.
        /// Note that it's a theoretical estimation. In real-world workloads it won't just do
        /// counter increment/decrement.
        pub fn curr_shard_index() -> ShardIndex {
            SHARD_INDEX_FROM_THREAD_ID_HASH.with(|v| *v)
        }

        pub const DOES_SHARD_INDEX_USE_CPU_INDEX: bool = false;
    }
}
