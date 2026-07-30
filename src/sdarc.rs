use crate::collector;
use crate::collector::on_new_sdarc_allocated;
use crate::sharded_alloc::{ShardedBox, ShardedDataPtr};
use crate::tagged_counter::AtomicTaggedCounter;
use std::any::type_name;
use std::fmt::{Debug, Display, Formatter};
use std::mem;
use std::mem::offset_of;
use std::ops::Deref;
use std::hash::Hash;
use std::ptr::{NonNull, null_mut};
use std::sync::OnceLock;
use crate::weak_sdarc::{clear_weak_backref_impl, ClearWeakBackRefResult, WeakSdarcInner};

/// Sharded deferred atomic reference counting.
///
/// Its counters are sharded. Each clone or drop will only change the counter shard corresponding to current thread.
/// So it will have much fewer cache contention than std `Arc`.
///
/// When the counter sum goes 0, it's not immediately freed. It's freed by the background collector deferred.
///
/// It doesn't support variable-sized type due to internal implementation.
pub struct Sdarc<T> {
    pub(crate) inner_ptr: NonNull<SdarcInner<T>>,
}

impl<T: Send + Sync> Sdarc<T> {
    pub fn new(value: T) -> Sdarc<T> {
        /// dropped in [`drop_sdarc_inner_impl`]
        let ptr: NonNull<SdarcInner<T>> = Box::leak(Box::new(SdarcInner::new(value))).into();

        on_new_sdarc_allocated(
            SdarcInnerFatPtr::new(ptr),
            unsafe { ptr.as_ref() }.counters.0,
        );
        Sdarc { inner_ptr: ptr }
    }
}

impl<T> Sdarc<T> {
    /// Creating a `Sdarc` from raw pointer without incrementing reference count
    pub(crate) unsafe fn from_raw_ptr(ptr: NonNull<SdarcInner<T>>) -> Sdarc<T> {
        Self { inner_ptr: ptr }
    }

    /// Creating a `Sdarc` from raw pointer without incrementing reference count if not null
    #[allow(clippy::manual_map)]
    pub(crate) unsafe fn nullable_from_raw_ptr(old_ptr: *mut SdarcInner<T>) -> Option<Sdarc<T>> {
        match NonNull::new(old_ptr) {
            None => None,
            Some(old_ptr) => Some(unsafe { Sdarc::from_raw_ptr(old_ptr) }),
        }
    }

    /// Consuming `Sdarc` into raw pointer without decrementing reference count
    pub(crate) fn into_raw_ptr(self: Sdarc<T>) -> NonNull<SdarcInner<T>> {
        let result = self.inner_ptr;
        // don't decrement reference count
        mem::forget(self);
        result
    }

    pub(crate) fn nullable_into_raw_ptr(sdarc: Option<Sdarc<T>>) -> *mut SdarcInner<T> {
        match sdarc {
            None => null_mut(),
            Some(sdarc) => sdarc.into_raw_ptr().as_ptr(),
        }
    }

    pub(crate) fn nullable_get_raw_ptr(sdarc: &Option<Sdarc<T>>) -> *mut SdarcInner<T> {
        match sdarc {
            None => null_mut(),
            Some(sdarc) => sdarc.inner_ptr.as_ptr(),
        }
    }

    pub fn ptr_eq(a: &Sdarc<T>, b: &Sdarc<T>) -> bool {
        a.inner_ptr == b.inner_ptr
    }
}

impl<T: PartialEq> PartialEq for Sdarc<T> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<T: Eq> Eq for Sdarc<T> {}

impl<T: Hash> Hash for Sdarc<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (**self).hash(state);
    }
}

impl<T: PartialOrd> PartialOrd for Sdarc<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        (**self).partial_cmp(&**other)
    }
}

impl<T: Ord> Ord for Sdarc<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (**self).cmp(&**other)
    }
}

impl<T: Debug> Debug for Sdarc<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Sdarc").field(&**self).finish()
    }
}

impl<T: Display> Display for Sdarc<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&**self, f)
    }
}

impl<T: Default + Send + Sync> Default for Sdarc<T> {
    fn default() -> Self {
        Sdarc::new(T::default())
    }
}

impl<T> Deref for Sdarc<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner_ref().data
    }
}

impl<T> Sdarc<T> {
    pub(crate) fn inner_ref(&self) -> &SdarcInner<T> {
        // Safety: reference counting ensures it's not dangling.
        // And it's never mutably borrowed before dropping.
        // For non-Send+Sync types, the SdarcInner cannot be created.
        unsafe { self.inner_ptr.as_ref() }
    }
}

unsafe impl<T: Send> Send for Sdarc<T> {}
unsafe impl<T: Sync> Sync for Sdarc<T> {}

pub(crate) struct SdarcInner<T> {
    /// One counter shard can go negative. The sum of them matters.
    pub(crate) counters: ShardedBox<AtomicTaggedCounter>,
    /// It will never be initialized if [`Sdarc::downgrade`] is never called.
    pub(crate) weak_inner_ref: OnceLock<Sdarc<WeakSdarcInner<T>>>,
    pub(crate) data: T,
}

impl<T: Send + Sync> SdarcInner<T> {
    fn new(value: T) -> SdarcInner<T> {
        let counters = ShardedBox::allocate_data_in_each_shard(|_| AtomicTaggedCounter::new());

        /// Initially current shard's counter is 1, other shards' counters are 0.
        /// Why use Relaxed ordering is ok: submitting it to collector uses locking,
        /// which ensures collector doesn't see counters before this increment.
        counters
            .at_curr_thread_shard()
            .increment_ref_count_relaxed();

        SdarcInner {
            counters,
            weak_inner_ref: OnceLock::new(),
            data: value,
        }
    }
}

impl<T> Clone for Sdarc<T> {
    fn clone(&self) -> Self {
        // Why use Relaxed ordering: Similar to std `Arc`, it can only clone from an existing Sdarc.
        // Incrementing late or early is fine.
        // Sending to another thread will be synchronized,
        // so that incrementing will be before it's observable by other threads.
        self.inner_ref()
            .counters
            .at_curr_thread_shard()
            .increment_ref_count_relaxed();

        Self {
            inner_ptr: self.inner_ptr,
        }
    }
}

impl<T> Drop for Sdarc<T> {
    fn drop(&mut self) {
        /// If it's dropped in collector thread, will notify collector to re-check it.
        /// It's put before decrementing counter to avoid use-after-free (found by miri).
        collector::on_sdarc_drop(self.inner_ref().counters.0);
        
        /// Why use Release ordering:
        /// If the collector observes the decremented reference count (with tag set) using Acquire ordering,
        /// it should synchronize-with the decrement,
        /// which ensures that collector can see the counter increments before the decrement.
        ///
        /// What about incrementing a Sdarc reference count then send to another thread to decrement?
        /// The sending data between threads will do synchronization that ensures increment happens-before decrement.
        ///
        /// What about current thread shard index changed?
        /// The thread could increment one shard counter, change its shard index, then decrement another shard's
        /// counter.
        /// It's still fine, because in same thread the increment is sequenced-before decrement,
        /// even if they touch different counter shards.
        /// If the collector observes that the decremented counter shard is in decremented value,
        /// the collector can observe the increment in another counter shard.
        self.inner_ref()
            .counters
            .at_curr_thread_shard()
            .decrement_ref_count_and_set_tag_release();
    }
}

/// It's type-erased thin ptr.
///
/// It's thin ptr so it's not trivial to make Sdarc support variable-sized type.
/// It's possible to support that, TODO.
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct SdarcInnerPtrErased(pub NonNull<u8>);

unsafe impl Send for SdarcInnerPtrErased {}
unsafe impl Sync for SdarcInnerPtrErased {}

impl SdarcInnerPtrErased {
    pub fn from_typed<T>(r: NonNull<SdarcInner<T>>) -> Self {
        Self(r.cast())
    }

    /// Safety: must use the correct type. Only use within vtable function impl.
    pub fn into_typed<T>(self) -> NonNull<SdarcInner<T>> {
        self.0.cast()
    }
}

/// The vtable is needed because the collector need to handle dropping of different types.
pub(crate) struct SdarcVTable {
    /// Offset of [`SdarcInner::counters`] field.
    ///
    /// Rust compiler can reorder fields so it's not necessarily in beginning.
    pub(crate) offset_of_counter: usize,

    /// See [`clear_weak_backref_impl`]
    pub(crate) clear_weak_backref: fn(SdarcInnerPtrErased) -> ClearWeakBackRefResult,

    /// See [`drop_sdarc_inner_impl`]
    pub(crate) drop_sdarc_inner: fn(SdarcInnerPtrErased) -> (),

    pub(crate) get_type_name_for_debugging: fn() -> &'static str,
}

pub(crate) fn get_sdarc_vtable_ref<T>() -> &'static SdarcVTable {
    &SdarcVTable {
        offset_of_counter: offset_of!(SdarcInner<T>, counters),
        clear_weak_backref: clear_weak_backref_impl::<T>,
        drop_sdarc_inner: drop_sdarc_inner_impl::<T>,
        get_type_name_for_debugging: get_type_name_for_debugging_impl::<T>,
    }
}

fn drop_sdarc_inner_impl<T>(ptr: SdarcInnerPtrErased) {
    let p: NonNull<SdarcInner<T>> = ptr.into_typed::<T>();

    let _box = unsafe { Box::from_raw(p.as_ptr()) };
}

fn get_type_name_for_debugging_impl<T>() -> &'static str {
    type_name::<T>()
}

impl Debug for SdarcVTable {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "SdarcVTable({})", (self.get_type_name_for_debugging)())
    }
}

#[derive(Copy, Clone, Debug)]
pub(crate) struct SdarcInnerFatPtr {
    pub ptr: SdarcInnerPtrErased,
    pub vtable_ref: &'static SdarcVTable,
}

impl SdarcInnerFatPtr {
    pub(crate) fn new<T>(ptr: NonNull<SdarcInner<T>>) -> SdarcInnerFatPtr {
        SdarcInnerFatPtr {
            ptr: SdarcInnerPtrErased::from_typed(ptr),
            vtable_ref: get_sdarc_vtable_ref::<T>(),
        }
    }
    
    pub fn get_counters_ptr(self) -> ShardedDataPtr<AtomicTaggedCounter> {
        unsafe {
            self.ptr
                .0
                .offset(self.vtable_ref.offset_of_counter as isize)
                .cast::<ShardedBox<AtomicTaggedCounter>>()
                .as_ref()
                .0
        }
    }

    /// See [`clear_weak_backref_impl`]
    pub fn clear_weak_back_ref(self) -> ClearWeakBackRefResult {
        (self.vtable_ref.clear_weak_backref)(self.ptr)
    }

    /// See [`drop_sdarc_inner_impl`]
    pub fn free(self) {
        (self.vtable_ref.drop_sdarc_inner)(self.ptr);
    }
}

unsafe impl Send for SdarcInnerFatPtr {}
unsafe impl Sync for SdarcInnerFatPtr {}
