use crate::collector;
use crate::collector::on_new_sdarc_allocated;
use crate::reader_critical_section::READER_CRITICAL_SECTION;
use crate::sharded_alloc::{ShardedBox, ShardedDataPtr};
use crate::tagged_counter::AtomicTaggedCounter;
use std::any::type_name;
use std::fmt::{Debug, Formatter};
use std::mem;
use std::mem::offset_of;
use std::ops::Deref;
use std::ptr::{NonNull, null_mut};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicPtr, Ordering};

/// Sharded deferred atomic reference counting.
///
/// Its counters are sharded. Each clone or drop will only change the counter shard corresponding to current thread.
/// So it will have much fewer cache contention than std `Arc`.
///
/// When the counter sum goes 0, it's not immediately freed. It's freed by the background collector deferred.
///
/// It doesn't support variable-sized type due to internal implementation.
pub struct Sdarc<T> {
    inner_ptr: NonNull<SdarcInner<T>>,
}

impl<T: Send + Sync> Sdarc<T> {
    pub fn new(value: T) -> Sdarc<T> {
        /// dropped in [`drop_sdarc_inner_impl`]
        let ptr: NonNull<SdarcInner<T>> = Box::leak(Box::new(SdarcInner::new(value))).into();

        on_new_sdarc_allocated(
            SdarcInnerFatPtr {
                ptr: SdarcInnerPtrErased::from_typed(ptr),
                vtable_ref: get_sdarc_vtable_ref::<T>(),
            },
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
    unsafe fn nullable_from_raw_ptr(old_ptr: *mut SdarcInner<T>) -> Option<Sdarc<T>> {
        match NonNull::new(old_ptr) {
            None => None,
            Some(old_ptr) => Some(unsafe { Sdarc::from_raw_ptr(old_ptr) }),
        }
    }

    /// Consuming `Sdarc` into raw pointer without decrementing reference count
    fn into_raw_ptr(self: Sdarc<T>) -> NonNull<SdarcInner<T>> {
        let result = self.inner_ptr;
        // don't decrement reference count
        mem::forget(self);
        result
    }

    fn nullable_into_raw_ptr(sdarc: Option<Sdarc<T>>) -> *mut SdarcInner<T> {
        match sdarc {
            None => null_mut(),
            Some(sdarc) => sdarc.into_raw_ptr().as_ptr(),
        }
    }

    fn nullable_get_raw_ptr(sdarc: &Option<Sdarc<T>>) -> *mut SdarcInner<T> {
        match sdarc {
            None => null_mut(),
            Some(sdarc) => sdarc.inner_ptr.as_ptr(),
        }
    }

    pub fn is_same_pointee(a: &Sdarc<T>, b: &Sdarc<T>) -> bool {
        a.inner_ptr == b.inner_ptr
    }
}

// TODO impl Eq, Hash, Ord etc. for Sdarc

impl<T> Deref for Sdarc<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner_ref().data
    }
}

impl<T> Sdarc<T> {
    fn inner_ref(&self) -> &SdarcInner<T> {
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
        /// Why use Release ordering:
        /// If the collector observes the decremented reference count (with tag set) using Acquire ordering,
        /// it should synchronize-with the decrement,
        /// which ensures that collector can see the counter increments before the decrement.
        ///
        /// What about incrementing a Sdarc reference count then send to another thread to decrement?
        /// The sending data between threads will do synchronization that ensures increment happens-before decrement.
        ///
        /// What about current thread change shard index using [`shard_index::set_current_thread_shard_index`]?
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

        /// If it's dropped in collector thread, will notify collector to re-check it.
        collector::on_sdarc_drop(self.inner_ref().counters.0);
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

pub struct AtomicNullableSdarc<T> {
    inner_ptr: AtomicPtr<SdarcInner<T>>,
}

unsafe impl<T: Send> Send for AtomicNullableSdarc<T> {}
unsafe impl<T: Sync> Sync for AtomicNullableSdarc<T> {}

impl<T: Send + Sync> AtomicNullableSdarc<T> {
    pub fn new() -> Self {
        Self {
            inner_ptr: AtomicPtr::new(null_mut()),
        }
    }

    pub fn new_with_value(value: T) -> Self {
        let r = Self::new();
        r.swap(Some(Sdarc::new(value)));
        r
    }
}

impl<T> AtomicNullableSdarc<T> {
    /// Load the atomic pointer. If not null, it will increment counter and give owned `Sdarc<T>`.
    pub fn load(&self) -> Option<Sdarc<T>> {
        // There is a chance thread A get stuck right after loading pointer but right before incrementing counter,
        // the thread B mutates atomic pointer and drop the original Sdarc, then inner data freed by background collector,
        // then thread A resumes and then use-after-free.
        // The reader critical section avoids it. Background collector will only free if no thread is stuck in reader side critical section.
        READER_CRITICAL_SECTION.reader_critical_section(|| {
            /// Why use Acquire ordering: synchronizes-with atomic pointer mutator.
            /// Ensure that the atomic pointer mutator thread's changed before mutating
            /// atomic pointer is visible.
            let ptr = self.inner_ptr.load(Ordering::Acquire);
            match NonNull::new(ptr) {
                None => None,
                Some(ptr) => {
                    let sdarc = unsafe { Sdarc::from_raw_ptr(ptr) };

                    // Increment counter
                    // Use Relaxed ordering because the reader critical section already does protection.
                    sdarc
                        .inner_ref()
                        .counters
                        .at_curr_thread_shard()
                        .increment_ref_count_relaxed();

                    Some(sdarc)
                }
            }
        })
    }

    /// Set the atomic pointer and get the replaced one.
    pub fn swap(&self, sdarc: Option<Sdarc<T>>) -> Option<Sdarc<T>> {
        let new_ptr = Sdarc::nullable_into_raw_ptr(sdarc);

        /// Why use AcqRel ordering: synchronize-with [`Self::load`]'s loading pointer in Acquire ordering.
        /// The thread calling [`Self::load`] should observe all mutations to the content pointed by `new_ptr`.
        /// The old pointer is returned, so if user code uses the swapped-out Sdarc,
        /// later user code observes all mutations before the pointer writer writes the old pointer.
        /// (It involves reads/writes in user code, not in current library).
        let old_ptr = self.inner_ptr.swap(new_ptr, Ordering::AcqRel);

        unsafe { Sdarc::nullable_from_raw_ptr(old_ptr) }
    }

    /// Set the atomic pointer and discard the original one.
    pub fn store(&self, sdarc: Option<Sdarc<T>>) {
        let new_ptr = Sdarc::nullable_into_raw_ptr(sdarc);

        /// Why use Release ordering, but the [`Self::swap`] uses AcqRel:
        /// The old pointer is dropped rather than returned.
        /// No user code could depend on anything from the old pointer.
        /// The reference count decrement is synchronized with collector in other ways.
        let old_ptr = self.inner_ptr.swap(new_ptr, Ordering::Release);

        let _to_drop = unsafe { Sdarc::nullable_from_raw_ptr(old_ptr) };
    }

    /// If the pointer matches `if_matches`, it succeeds and sets pointer. In that case it returns Ok containing the original `Sdarc` (it points to the same as `if_matches`).
    /// If the pointer doesn't match `if_matches`, it returns Err.
    ///
    /// The Sdarc delayed reclamation makes it free of ABA problem.
    pub fn compare_and_set(
        &self,
        if_matches: &Option<Sdarc<T>>,
        then_set: &Option<Sdarc<T>>,
    ) -> Result<Option<Sdarc<T>>, ()> {
        self.raw_compare_and_set(
            Sdarc::nullable_get_raw_ptr(if_matches),
            Sdarc::nullable_get_raw_ptr(then_set),
        )
    }

    fn raw_compare_and_set(
        &self,
        if_matches_ptr: *mut SdarcInner<T>,
        then_set_ptr: *mut SdarcInner<T>,
    ) -> Result<Option<Sdarc<T>>, ()> {
        let r = self.inner_ptr.compare_exchange(
            if_matches_ptr,
            then_set_ptr,
            // Use the kind-of strong memory ordering, probably needed by user code.
            // If the user wants SeqCst, they can add their memory barriers.
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        match r {
            Ok(original_ptr) => {
                assert_eq!(original_ptr, if_matches_ptr);

                // Setting succeeded, but the `then_set_ptr` comes from a borrowed Sdarc. There is no Sdarc ownership transfer.
                // We need to increment counter to compensate.
                // No need to use critical section here, because at this time at least one strong reference of `then_set` exists.
                if let Some(then_set_inner) = unsafe { then_set_ptr.as_ref() } {
                    then_set_inner
                        .counters
                        .at_curr_thread_shard()
                        .increment_ref_count_relaxed();
                }

                // The original pointer was overwritten. Create a Sdarc to compensate.
                Ok(unsafe { Sdarc::nullable_from_raw_ptr(original_ptr) })
            }
            Err(_original_ptr) => Err(()),
        }
    }
}

impl<T> Drop for AtomicNullableSdarc<T> {
    fn drop(&mut self) {
        self.store(None);
    }
}

pub struct AtomicSdarc<T>(AtomicNullableSdarc<T>);

impl<T: Send + Sync> AtomicSdarc<T> {
    pub fn new(value: T) -> Self {
        Self(AtomicNullableSdarc::new_with_value(value))
    }

    /// Load the atomic pointer and give owned `Sdarc<T>`.
    pub fn load(&self) -> Sdarc<T> {
        self.0.load().unwrap()
    }

    /// Set the atomic pointer and get the replaced one.
    pub fn swap(&self, new_sdarc: Sdarc<T>) -> Sdarc<T> {
        self.0.swap(Some(new_sdarc)).unwrap()
    }

    pub fn store(&self, new_sdarc: Sdarc<T>) {
        self.0.store(Some(new_sdarc));
    }

    /// If the pointer matches `if_matches`, it succeeds and sets pointer. In that case it returns Ok containing the original `Sdarc` (it points to the same as `if_matches`).
    /// If the pointer doesn't match `if_matches`, it returns Err.
    ///
    /// The Sdarc delayed reclamation makes it free of ABA problem.
    pub fn compare_and_set(
        &self,
        if_matches: &Sdarc<T>,
        then_set: &Sdarc<T>,
    ) -> Result<Sdarc<T>, ()> {
        match self
            .0
            .raw_compare_and_set(if_matches.inner_ptr.as_ptr(), then_set.inner_ptr.as_ptr())
        {
            Ok(original_sdarc) => Ok(original_sdarc.unwrap()),
            Err(()) => Err(()),
        }
    }
}

pub(crate) struct WeakSdarcInner<T> {
    /// There is a circular reference. `SdarcInner` has `Sdarc<WeakSdarcInner>`, this references back.
    /// When initialized, it's not null.
    /// When collector thinks that a `SdarcInner`'s strong count sum reach zero (observed zero sum once, clear tags, observed zero sum in next iteration with no tag set),
    /// If [`SdarcInner::weak_inner_ref`] is initialized, this backref will be set to null.
    /// Upgrade can only succeed if it's not null, and upgrade is under reader critical section.
    ///
    /// Note: it's possible that a concurrent upgrade resurrects the SdarcInner whose strong count sum is 0.
    /// After resurrection, `Sdarc` can still downgrade.
    /// The `WeakSdarc` may be unable to upgrade or may can upgrade after resurrection,
    /// depending on whether backref is cleared, which depends on collector timing.
    back_ref: AtomicPtr<SdarcInner<T>>,
}

unsafe impl<T: Send> Send for WeakSdarcInner<T> {}
unsafe impl<T: Sync> Sync for WeakSdarcInner<T> {}

impl<T> Drop for WeakSdarcInner<T> {
    fn drop(&mut self) {
        // use Relaxed ordering because it's just an assertion
        assert!(
            self.back_ref.load(Ordering::Relaxed).is_null(),
            "WeakSdarcInner's backref is not cleared"
        );
    }
}

/// The weak reference version of [`Sdarc`].
///
/// The weak reference behavior is very different to std `Arc` and `Weak`.
/// When there is no strong reference of `Sdarc`, the [`WeakSdarc::upgrade`] may still succeed.
/// Then the dead `Sdarc` will be resurrected.
///
/// Why have the weird resurrection mechanism, instead of ensuring that resurrection is not possible:
/// Avoiding resurrection requires [`WeakSdarc::upgrade`] to ensure whether strong count sum is 0 immediately.
/// Without locking, it's not possible. We avoid locking of counters to improve scalability.
pub struct WeakSdarc<T> {
    sdarc_weak_inner: Sdarc<WeakSdarcInner<T>>,
}

pub(crate) enum ClearWeakBackRefResult {
    WeakRefNotInvolved,
    WeakBackRefCleared,
    WeakBackRefWasAlreadyNull,
}

/// When this function is called, the strong count sum reaches 0.
/// But there may be weak references, and the weak references can still upgrade at the same time.
///
/// But the [`SdarcInner::weak_inner_ref`] will never be initialized at that time if it is not initialized,
/// because it can only be initialized from strong reference, and strong reference doesn't exist
/// if no weak reference to it exists.
///
/// If [`SdarcInner::weak_inner_ref`] has been initialized, it will clear the backref.
/// After clearing, weak ref's upgrade will fail. And the backref will never become non-null again.
///
/// If the `Sdarc` has never been downgraded, it will return [`ClearWeakBackRefResult::WeakRefNotInvolved`],
/// and the collector will free it once strong count sum reaches 0 and counters keeps being same across one iteration.
///
/// If the `Sdarc` has been downgraded, and it's the first time that `clear_weak_backref_impl` get called for it,
/// then it will return [`ClearWeakBackRefResult::WeakBackRefCleared`],
/// and the collector will assume that it may resurrect, and will not free despite strong counter sum being 0 and not changing.
///
/// If the `Sdarc` has been downgraded, then resurrected, then `clear_weak_backref_impl` may be called for it again.
/// In that case, the backref has already been cleared. No more upgrade is possible. The collector will free it
/// once strong count sum reaches 0 and counters keep being same across one iteration.
///
/// Note that if it dies then resurrects quickly, without the "confirmed dead" state being observed by collector,
/// then this function won't be called at that time.
fn clear_weak_backref_impl<T>(ptr: SdarcInnerPtrErased) -> ClearWeakBackRefResult {
    let p: NonNull<SdarcInner<T>> = ptr.into_typed::<T>();

    let r: &SdarcInner<T> = unsafe { p.as_ref() };

    if let Some(inner) = r.weak_inner_ref.get() {
        /// Reset the backref to null. the weak ref will no longer be able to upgrade.
        /// The clearing is one-way. after clearing, it cannot become non-null.
        ///
        /// Why use Relaxed ordering: see comment in [`WeakSdarc::upgrade`]
        let swapped_ptr = inner.back_ref.swap(null_mut(), Ordering::Relaxed);
        if swapped_ptr.is_null() {
            ClearWeakBackRefResult::WeakBackRefWasAlreadyNull
        } else {
            ClearWeakBackRefResult::WeakBackRefCleared
        }
    } else {
        /// When this function is called, the strong count sum reaches 0.
        /// It's only initialized in [`Sdarc::downgrade`] which requires a strong reference.
        /// So if it's not initialized now, it will never initialize, then there will be no weak ref to it,
        /// and no upgrade is possible.
        ClearWeakBackRefResult::WeakRefNotInvolved
    }
}

impl<T: Send + Sync> Sdarc<T> {
    pub fn downgrade(&self) -> WeakSdarc<T> {
        let inner_ptr = self.inner_ptr;
        let inner = self.inner_ref();
        let r: &Sdarc<WeakSdarcInner<T>> = inner.weak_inner_ref.get_or_init(|| {
            Sdarc::new(WeakSdarcInner {
                back_ref: AtomicPtr::new(inner_ptr.as_ptr()),
            })
        });
        WeakSdarc {
            sdarc_weak_inner: r.clone(),
        }
    }
}

impl<T: Send + Sync> WeakSdarc<T> {
    /// Unlike std `Arc` and `Weak`, `Sdarc` and `WeakSdarc` have resurrection mechanism.
    /// Even after strong count sum reaches zero, upgrade may still succeed, then it will be resurrected.
    ///
    /// If the strong count sum has reached 0, then it's not deterministic whether upgrade will succeed
    /// (depending on collector timing). Upgrade may fail despite there exists strong references to same pointee.
    ///
    /// If the strong count sum never reaches 0, upgrade will succeed.
    pub fn upgrade(&self) -> Option<Sdarc<T>> {
        let weak_inner: &WeakSdarcInner<T> = self.sdarc_weak_inner.deref();
        // Similar to loading from atomic Sdarc, it may be stuck between loading pointer and incrementing counter,
        // so use reader side critical section.
        READER_CRITICAL_SECTION.reader_critical_section(|| {
            /// Why use Relaxed ordering:
            /// If it observes backref being null, upgrade fails, then it won't need to ensure visibility to any data
            /// that collector writes before clearing it.
            /// The `swap` in [`clear_weak_backref_impl`] is atomic regardless of ordering.
            let back_ref_loaded = weak_inner.back_ref.load(Ordering::Relaxed);

            match NonNull::new(back_ref_loaded) {
                None => {
                    // backref has been cleared, won't be able to upgrade
                    None
                }
                Some(sdarc_inner) => {
                    let upgraded = unsafe { Sdarc::from_raw_ptr(sdarc_inner) };

                    // Increment counter.
                    // use Relaxed ordering because reader critical section already does protection.
                    upgraded
                        .inner_ref()
                        .counters
                        .at_curr_thread_shard()
                        .increment_ref_count_relaxed();

                    Some(upgraded)
                }
            }
        })
    }
}
