//! Structure of sharded alloc
#![doc= include_str!("../docs/shard_alloc.drawio.svg")]

use crate::shard_index::{
    ShardIndex, ShardsArr, curr_thread_shard_index, get_shard_count, shard_indexes,
    shard_indexes_until,
};
use crossbeam::utils::CachePadded;
use parking_lot::RwLock;
use scopeguard::guard_on_unwind;
use std::alloc::{Layout, alloc, dealloc};
use std::marker::PhantomData;
use std::ops::{Deref, Index, IndexMut, Not};
use std::ptr::{NonNull, drop_in_place};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// Each slot is 8 bytes (same size as `u64`).
/// In mainstream platforms (X86-64 and ARM64), CachePadded use 128 alignment, which is 16 `u64`s.
const SLOT_COUNT_PER_SHARD: usize = 16;

pub(crate) struct AllocUnit {
    data_ptr: NonNull<u8>,
}

// Safety: the allocation uses usage map atomically. Allocating requires `Send + Sync`.
unsafe impl Send for AllocUnit {}
unsafe impl Sync for AllocUnit {}

impl AllocUnit {
    const USAGE_FLAG_UNUSED: u64 = 0;
    const USAGE_FLAG_USED: u64 = 1;

    fn new() -> AllocUnit {
        let layout = Self::get_layout();

        let ptr: *mut u8 = unsafe { alloc(layout) };
        let ptr = NonNull::new(ptr).expect("Sharded Alloc Failure");

        // initialize all the usage flags
        let atomic_u64_ptr = ptr.cast::<AtomicU64>();
        for slot_index in 0..SLOT_COUNT_PER_SHARD {
            let usage_flag_ptr = unsafe { atomic_u64_ptr.offset(slot_index as isize) };
            unsafe {
                usage_flag_ptr.write(AtomicU64::new(Self::USAGE_FLAG_UNUSED));
            }
        }

        AllocUnit { data_ptr: ptr }
    }

    fn get_layout() -> Layout {
        let len_bytes = Self::data_len_in_bytes();

        let layout = Layout::from_size_align(
            len_bytes, 8, // align is same as u64
        )
        .unwrap();
        layout
    }

    fn data_len_in_bytes() -> usize {
        // the added 1 is for usage flags. see the svg for structure
        SLOT_COUNT_PER_SHARD * (1 + get_shard_count().0 as usize) * 8
    }

    /// The `index_of_unit` will be used for deallocating.
    ///
    /// If it will never be deallocated, `index_of_unit` can be `usize::MAX`
    fn allocate_and_initialize<T: Send + Sync>(
        &self,
        init_func: impl Fn(ShardIndex) -> T,
    ) -> Option<ShardedDataPtr<T>> {
        if let Some(sharded_data_ptr) = self.allocate_without_initializing() {
            // write data slots
            for shard_index in shard_indexes() {
                let ele_ptr: NonNull<T> = sharded_data_ptr.ptr_at_shard(shard_index);

                let init_value: T = {
                    let _unwind_guard = guard_on_unwind((), |()| {
                        // the `init_func` can panic.
                        // when it panics, drop the already-written values and de-allocate

                        for shard_index_to_drop in shard_indexes_until(shard_index) {
                            let ele_ptr_to_drop: NonNull<T> =
                                sharded_data_ptr.ptr_at_shard(shard_index_to_drop);
                            unsafe {
                                drop_in_place(ele_ptr_to_drop.as_ptr());
                            }
                        }

                        unsafe {
                            sharded_data_ptr.deallocate_without_dropping();
                        }
                    });

                    init_func(shard_index)
                };

                // Safety: ele_ptr is not dangling.
                unsafe { ele_ptr.write(init_value) };
            }
            Some(sharded_data_ptr)
        } else {
            None
        }
    }

    fn allocate_without_initializing<T: Send + Sync>(&self) -> Option<ShardedDataPtr<T>> {
        let u64_ptr = self.data_ptr.cast::<u64>();

        for slot_index in 0..SLOT_COUNT_PER_SHARD {
            let offseted_ptr = unsafe { u64_ptr.offset(slot_index as isize) };
            let usage_flag: &AtomicU64 = unsafe { offseted_ptr.cast::<AtomicU64>().as_ref() };

            /// The Acquire ordering synchronizes-with Release ordering in
            /// [`ShardedDataPtr::deallocate_without_dropping`]
            let old_usage_flag_value = usage_flag.swap(Self::USAGE_FLAG_USED, Ordering::Acquire);
            Self::assert_usage_flag_value_valid(old_usage_flag_value);
            if old_usage_flag_value == Self::USAGE_FLAG_UNUSED {
                let sharded_data_ptr = ShardedDataPtr::new(offseted_ptr);
                return Some(sharded_data_ptr);
            }
        }

        None
    }

    #[allow(clippy::needless_lifetimes)]
    fn usage_flag_atomics<'a>(&'a self) -> impl Iterator<Item = &'a AtomicU64> {
        let u64_ptr = self.data_ptr.cast::<u64>();

        (0..SLOT_COUNT_PER_SHARD)
            .into_iter()
            .map(move |slot_index| {
                let offseted_ptr = unsafe { u64_ptr.offset(slot_index as isize) };
                unsafe { offseted_ptr.cast::<AtomicU64>().as_ref() }
            })
    }

    fn is_any_slot_used(&self) -> bool {
        self.usage_flag_atomics().any(|usage_flag| {
            let usage_flag_value = usage_flag.load(Ordering::Relaxed);
            Self::assert_usage_flag_value_valid(usage_flag_value);
            usage_flag_value == Self::USAGE_FLAG_USED
        })
    }

    fn has_any_free_slot(&self) -> bool {
        self.usage_flag_atomics().any(|usage_flag| {
            let usage_flag_value = usage_flag.load(Ordering::Relaxed);
            Self::assert_usage_flag_value_valid(usage_flag_value);
            usage_flag_value == Self::USAGE_FLAG_UNUSED
        })
    }

    fn assert_usage_flag_value_valid(value: u64) {
        assert!(value == 0 || value == 1);
    }
}

impl Drop for AllocUnit {
    fn drop(&mut self) {
        assert!(self.is_any_slot_used().not());

        unsafe {
            dealloc(self.data_ptr.as_ptr(), Self::get_layout());
        }
    }
}

pub(crate) struct ShardOfShardAlloc {
    all_units: Vec<AllocUnit>,
    indexes_of_units_to_check_for_allocation: Vec<usize>,
}

impl ShardOfShardAlloc {
    fn new() -> ShardOfShardAlloc {
        ShardOfShardAlloc {
            all_units: Vec::new(),
            indexes_of_units_to_check_for_allocation: Vec::new(),
        }
    }

    /// It only requires read lock. It can set atomic usage flag to true, but cannot change the memory layout.
    fn allocate_from_existing_units<T: Send + Sync>(
        &self,
        init_func: impl Fn(ShardIndex) -> T,
    ) -> Option<ShardedDataPtr<T>> {
        for i in &self.indexes_of_units_to_check_for_allocation {
            let i = *i;
            let unit = &self.all_units[i];
            if let Some(p) = unit.allocate_and_initialize::<T>(&init_func) {
                return Some(p);
            }
        }

        None
    }

    fn allocate_using_new_unit<T: Send + Sync>(
        &mut self,
        init_func: impl Fn(ShardIndex) -> T,
    ) -> ShardedDataPtr<T> {
        let new_unit = AllocUnit::new();

        let ptr: ShardedDataPtr<T> = new_unit
            .allocate_and_initialize(init_func)
            .expect("New unit should not fail allocation");

        self.all_units.push(new_unit);
        self.indexes_of_units_to_check_for_allocation
            .push(self.all_units.len() - 1);
        ptr
    }

    fn do_maintenance(&mut self) {
        self.all_units.retain_mut(|unit| unit.is_any_slot_used());

        self.indexes_of_units_to_check_for_allocation.clear();

        for (i, unit) in self.all_units.iter().enumerate() {
            if unit.has_any_free_slot() {
                self.indexes_of_units_to_check_for_allocation.push(i);
            }
        }
    }
}

pub(crate) struct FullShardAlloc {
    shards: ShardsArr<CachePadded<RwLock<ShardOfShardAlloc>>>,
}

impl FullShardAlloc {
    fn initialize() -> FullShardAlloc {
        let shards =
            ShardsArr::new(|_shard_index| CachePadded::new(RwLock::new(ShardOfShardAlloc::new())));
        FullShardAlloc { shards }
    }

    fn allocate_and_init<T: Send + Sync>(
        &self,
        init_func: impl Fn(ShardIndex) -> T,
    ) -> ShardedDataPtr<T> {
        let shard_index = curr_thread_shard_index();
        let shard = &self.shards[shard_index];
        let lock: &RwLock<ShardOfShardAlloc> = shard.deref();

        // Firstly try to allocate under read lock. If failed, then allocate under write lock.
        {
            let g = lock.read();
            if let Some(p) = g.allocate_from_existing_units::<T>(&init_func) {
                return p;
            }
        }

        let mut g = lock.write();
        g.allocate_using_new_unit(&init_func)
    }

    pub(crate) fn do_maintenance(&self) {
        for shard_index in shard_indexes() {
            let shard = &self.shards[shard_index];
            let lock: &RwLock<ShardOfShardAlloc> = shard.deref();
            let mut guard = lock.write();
            guard.do_maintenance();
        }
    }
}

pub(crate) static FULL_SHARD_ALLOC: LazyLock<FullShardAlloc> =
    LazyLock::new(|| FullShardAlloc::initialize());

/// It represents pointer to a piece of data in same offset in every shard.
///
/// The data's size should be same as `u64`.
pub(crate) struct ShardedDataPtr<T> {
    base_ptr: NonNull<u8>,
    _phantom: PhantomData<*mut T>,
}

unsafe impl<T: Send> Send for ShardedDataPtr<T> {}
unsafe impl<T: Sync> Sync for ShardedDataPtr<T> {}

impl<T> Copy for ShardedDataPtr<T> {}

impl<T> Clone for ShardedDataPtr<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> ShardedDataPtr<T> {
    fn new(base_ptr: NonNull<u64>) -> ShardedDataPtr<T> {
        const { assert!(size_of::<T>() <= size_of::<u64>()) }
        const { assert!(align_of::<T>() <= align_of::<u64>()) }

        ShardedDataPtr {
            base_ptr: base_ptr.cast::<u8>(),
            _phantom: PhantomData,
        }
    }

    /// Creating pointer is not unsafe. But using pointer is unsafe.
    pub(crate) fn ptr_at_shard(self, shard_index: ShardIndex) -> NonNull<T> {
        let offset: usize = SLOT_COUNT_PER_SHARD * (shard_index.as_usize() + 1);

        let u64_ptr: NonNull<u64> = self.base_ptr.cast::<u64>();
        // Safety: offset is within allocation
        let offseted: NonNull<u64> = unsafe { u64_ptr.offset(offset as isize) };

        offseted.cast::<T>()
    }

    /// Creating pointer is not unsafe. But using pointer is unsafe.
    pub(crate) fn ptr_at_curr_thread_shard(self) -> NonNull<T> {
        self.ptr_at_shard(curr_thread_shard_index())
    }

    fn usage_flag_ptr(self) -> NonNull<AtomicU64> {
        self.base_ptr.cast::<AtomicU64>()
    }

    /// Safety: Ensure pointer is not dangling before deallocating. And don't use it after deallocating it.
    pub(crate) unsafe fn deallocate_without_dropping(self) {
        // Safety: caller ensures not dangling. and usage flag is never converted to mutable reference until dropping.
        let usage_flag: &AtomicU64 = unsafe { self.usage_flag_ptr().as_ref() };

        // why use Release ordering: cannot use Relaxed because Relaxed allows delaying writes before deallocating to apply after deallocating.
        // the allocation sets flag using Acquire which synchronizes-with it. no need to use SeqCst
        let original_usage = usage_flag.swap(0, Ordering::Release);
        assert_eq!(
            original_usage, 1,
            "deallocated a slot whose usage flag was not set. free of dangling pointer"
        );

        // It will only set usage flag. Other allocator maintenance work is done by background thread.
    }
}

/// It owns the shard-allocated data (similar to Box).
///
/// The size of T is at most 8 bytes, due to how the allocator work.
///
/// The data of different shards will be in different cache lines.
pub struct ShardedBox<T>(ShardedDataPtr<T>);

impl<T: Send + Sync> ShardedBox<T> {
    pub fn allocate_data_in_each_shard(init_func: impl Fn(ShardIndex) -> T) -> ShardedBox<T> {
        const { assert!(size_of::<T>() <= size_of::<u64>()) }
        const { assert!(align_of::<T>() <= align_of::<u64>()) }

        let ptr = FULL_SHARD_ALLOC.allocate_and_init(init_func);
        Self(ptr)
    }

    /// Note: the current thread's shard index can be mutated by [`shard_index::set_current_thread_shard_index`]
    pub fn at_curr_thread_shard(&self) -> &T {
        unsafe { self.0.ptr_at_curr_thread_shard().as_ref() }
    }
}

impl<T> Drop for ShardedBox<T> {
    fn drop(&mut self) {
        for shard_index in shard_indexes() {
            let ptr = self.0.ptr_at_shard(shard_index);
            unsafe { drop_in_place(ptr.as_ptr()) };
        }

        unsafe { self.0.deallocate_without_dropping() };
    }
}

impl<T> Index<ShardIndex> for ShardedBox<T> {
    type Output = T;

    fn index(&self, index: ShardIndex) -> &Self::Output {
        unsafe { self.0.ptr_at_shard(index).as_ref() }
    }
}

impl<T> IndexMut<ShardIndex> for ShardedBox<T> {
    fn index_mut(&mut self, index: ShardIndex) -> &mut Self::Output {
        unsafe { self.0.ptr_at_shard(index).as_mut() }
    }
}
