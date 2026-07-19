//! Atomic Sdarc pointers.
//!
//! The safety relies on:
//! - The collector takes two iterations observing zero ref count sum to free one object.
//! - The collector will run a `membarrier2::heavy()` before each iteration.
//! -
//!
//! The hazard pointer mechanism
//!
//! A hazard pointer being not null means that
//! the thread is borrowing on the Sdarc content but didn't increment reference count.
//! A non-dangling hazard pointer means "reference count debt".
//! The thread should increment reference count, but haven't.
//!
//! When collector is about to free one `SdarcInner` it scans all hazard pointers (read-only for collector).
//! If there is corresponding hazard pointer, collector won't free it in current iteration.
//!
//! When current thread's hazard pointer slots are full,
//! it will pay reference count debt by incrementing reference count.
//! Then when borrowing stops, it should decrement reference count to compensate.

use crate::sdarc::{Sdarc, SdarcInner, SdarcInnerFatPtr, SdarcInnerPtrErased, SdarcVTable};
use append_only_vec::AppendOnlyVec;
use crossbeam::channel::at;
use crossbeam::utils::CachePadded;
use parking_lot::Mutex;
use rustc_hash::FxHashSet;
use std::cell::Cell;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ops::{Deref, Not};
use std::ptr::{NonNull, null_mut};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering, compiler_fence};
use std::{array, hint};

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
    pub fn load(&self) -> Option<Sdarc<T>> {
        load_atomic_ptr_owned(&self.inner_ptr)
    }

    /// Set the atomic pointer and get the replaced one.
    pub fn swap(&self, sdarc: Option<Sdarc<T>>) -> Option<Sdarc<T>> {
        let new_ptr = Sdarc::nullable_into_raw_ptr(sdarc);

        // Use SeqCst to sync with collector's heavy barrier
        // (actually light barrier is also or AcqRel can also sync with heavy barrier, use SeqCst because API user may need it)
        let old_ptr = self.inner_ptr.swap(new_ptr, Ordering::SeqCst);

        unsafe { Sdarc::nullable_from_raw_ptr(old_ptr) }
    }

    /// Set the atomic pointer and discard the original one.
    pub fn store(&self, sdarc: Option<Sdarc<T>>) {
        self.swap(sdarc);
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
            // Use SeqCst to sync with collector's heavy barrier
            // (actually light barrier is also or AcqRel can also sync with heavy barrier, use SeqCst because API user may need it)
            Ordering::SeqCst,
            Ordering::SeqCst,
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

    /// Borrow from the atomic pointer.
    /// The inner object will be kept alive using hazard pointer mechanism and "debt paying" mechanism.
    /// The borrow stays valid as long as the guard object is live.
    /// After the atomic pointer changes, the existing guard still borrows the previous pointee.
    ///
    /// The guard can keep the Sdarc pointee alive even when its reference count sum reach zero.
    ///
    /// It returns None if the loaded pointer is null.
    #[allow(clippy::needless_lifetimes)]
    pub fn borrow<'a>(&'a self) -> Option<HazardPointerGuard<'a, T>> {
        borrow_from_atomic_ptr_using_hazard_pointer(&self.inner_ptr)
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

    #[allow(clippy::needless_lifetimes)]
    pub fn borrow<'a>(&'a self) -> HazardPointerGuard<'a, T> {
        self.0.borrow().unwrap()
    }
}

/// Why 15: the [`PerThreadSharedHazardData`] contains hazard pointer slots and two `AtomicBool`s.
/// And it's hold with `CachePadded` which is 128 align in mainstream platform.
/// If we use 8, then it wastes space. Using 15 will make padding space less than a pointer width.
/// The added scanning is cheap because they are in same cache line.
/// Mod by 15 is not slow because compiler can optimize it into multiplication.
const HZ_PTR_SLOT_COUNT: usize = 15;

#[derive(Copy, Clone, Debug)]
struct HzSlotIndex(u8);

/// It's used for protecting the process between loading an atomic pointer and incrementing ref count.
///
/// It can be seen as a special spinlock, except that reader thread never spins and directly acquires lock (always succeed),
/// collector just keeps polling it until it's not locked.
///
/// It can also be seen as a "universal" hazard pointer that correspond to any data managed by Sdarc.
///
/// It must be used in SeqCst ordering. Collector reading it using Acquire ordering is not safe,
/// because Acquire pure load can load stale values. The Release-Acquire ordering only gives guarantee
/// when Acquire reader reads new value. It has no guarantee when Acquire reads old value.
///
/// The atomic pointer must also be accessed with SeqCst.
///
/// Reader thread:
/// 1. set is_loading_atomic_sdarc_as_owned to true
/// 2. light barrier
/// 3. load atomic pointer
/// (Assume that Reader thread can be un-scheduled here)
/// 4. increment ref count
/// 5. light barrier
/// 6. set is_loading_atomic_sdarc_as_owned to false
///
/// Writer thread:
/// 1. swap atomic pointer
/// 2. decrement ref count of original object, Release
///
/// Collector thread:
/// 1. heavy barrier
/// 2. for each thread's is_loading_atomic_sdarc_as_owned in SeqCst, poll until observing false
/// 3. read counter sum in Acquire ordering. if it stays 0, clear tags
/// 4. in next collector inner iteration, heavy barrier again, poll until observing false again
/// 5. read counter sum in Acquire ordering, if it stays 0 and no tag is observed, free the memory
///
/// Right after collector finishing polling, a reader thread can start reading. This is fine
/// because it takes at least two inner iterations to free one object.
///
/// Note: the SeqCst ordering is required (when light barrier synchronizes with heavy barrier it provides SeqCst ordering).
/// Using only Release-Acquire ordering is unsound.
/// Then it's possible that Reader's setting is_loading_atomic_sdarc_as_owned to true is not visible to collector
/// for long time, and reader is un-scheduled for long time between 2 and 3, then collector frees the object
/// whose pointer was the load result of reader.
pub(crate) struct PerThreadReaderCriticalSection(AtomicBool);

impl PerThreadReaderCriticalSection {
    fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    fn reader_critical_section<R>(&self, func: impl FnOnce() -> R) -> R {
        assert!(
            self.0.load(Ordering::Relaxed).not(),
            "reader_critical_section cannot nest"
        );

        self.0.store(true, Ordering::Relaxed);

        membarrier2::light();

        let _guard = scopeguard::guard((), |()| {
            membarrier2::light();

            self.0.store(false, Ordering::Relaxed);
        });

        let result = func();

        result
    }

    fn direct_poll(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    fn collector_poll_until_false(&self) {
        loop {
            let r = self.0.load(Ordering::Relaxed);
            if r.not() {
                return;
            }

            // this is just for preventing compiler from merging reads
            compiler_fence(Ordering::SeqCst);

            hint::spin_loop();
        }
    }
}

/// It will only be written by local thread.
/// The collector thread only reads it.
/// This makes it much simpler than arc-swap's hazard pointer implementation.
pub(crate) struct PerThreadSharedHazardData {
    pub hazard_ptr_slots: [AtomicPtr<u8>; HZ_PTR_SLOT_COUNT],
    pub is_used: AtomicBool,

    pub reader_critical_section: PerThreadReaderCriticalSection,
}

impl PerThreadSharedHazardData {
    fn load_relaxed(&self, index: HzSlotIndex) -> *mut u8 {
        self.hazard_ptr_slots[index.0 as usize].load(Ordering::Relaxed)
    }

    fn store_relaxed(&self, index: HzSlotIndex, ptr: *mut u8) {
        self.hazard_ptr_slots[index.0 as usize].store(ptr, Ordering::Relaxed)
    }
}

impl HzSlotIndex {
    fn offset(self, i: usize) -> HzSlotIndex {
        assert!(self.0 < HZ_PTR_SLOT_COUNT as u8);
        assert!(i < HZ_PTR_SLOT_COUNT);

        let num = (self.0 as usize) + i;
        HzSlotIndex((num % HZ_PTR_SLOT_COUNT) as u8)
    }

    fn all() -> impl Iterator<Item = HzSlotIndex> {
        (0..HZ_PTR_SLOT_COUNT).map(|n| HzSlotIndex(n as u8))
    }

    fn all_start_from(start_index: HzSlotIndex) -> impl Iterator<Item = HzSlotIndex> {
        (0..HZ_PTR_SLOT_COUNT).map(move |n| start_index.offset(n))
    }
}

// The AppendOnlyVec is simpler than manually making a lock-free linked list
static SHARED_HAZARD_DATA: AppendOnlyVec<CachePadded<PerThreadSharedHazardData>> =
    AppendOnlyVec::new();

fn obtain_per_thread_shared_hazard_data() -> &'static PerThreadSharedHazardData {
    for e in SHARED_HAZARD_DATA.iter() {
        /// Why use Acquire: make is_used being true visible before subsequent hazard pointer operations
        let old_is_used = e.is_used.swap(true, Ordering::Acquire);
        if !old_is_used {
            // claimed by current thread
            return e.deref();
        }
    }

    let r = PerThreadSharedHazardData {
        hazard_ptr_slots: array::from_fn(|_| (AtomicPtr::new(null_mut()))),
        is_used: AtomicBool::new(true),
        reader_critical_section: PerThreadReaderCriticalSection::new(),
    };
    let idx = SHARED_HAZARD_DATA.push(CachePadded::new(r));
    let result = &SHARED_HAZARD_DATA[idx];
    assert!(result.is_used.load(Ordering::Relaxed));
    result
}

fn release_hazard_data(r: &'static PerThreadSharedHazardData) {
    assert!(
        r.hazard_ptr_slots
            .iter()
            .all(|ptr| ptr.load(Ordering::Relaxed).is_null())
    );
    assert!(r.reader_critical_section.direct_poll().not());

    let old_used = r.is_used.swap(false, Ordering::Release);
    assert!(old_used);
}

struct PerThreadSharedHazardDataRef(&'static PerThreadSharedHazardData);

impl Drop for PerThreadSharedHazardDataRef {
    fn drop(&mut self) {
        release_hazard_data(self.0);
    }
}

thread_local! {
    static CURR_THREAD_SHARD_HAZARD_DATA_REF: PerThreadSharedHazardDataRef =
        PerThreadSharedHazardDataRef(obtain_per_thread_shared_hazard_data());
}

fn curr_thread_shared_hazard_data() -> &'static PerThreadSharedHazardData {
    CURR_THREAD_SHARD_HAZARD_DATA_REF.with(|r| r.0)
}

pub(crate) fn traverse_all_hazard_datum(mut func: impl FnMut(&'static PerThreadSharedHazardData)) {
    for e in SHARED_HAZARD_DATA.iter() {
        func(e.deref());
    }
}

pub(crate) struct HazardPointerSet(FxHashSet<NonNull<u8>>);

impl HazardPointerSet {
    pub fn contains(&self, ptr: SdarcInnerPtrErased) -> bool {
        self.0.contains(&ptr.0)
    }
}

pub(crate) fn traverse_atomic_reader_signals() -> HazardPointerSet {
    // Important. The heavy barrier has total order with each thread's light barriers.
    // However, different threads' light barriers have no total order between.
    membarrier2::heavy();

    let mut hazard_ptr_set: FxHashSet<NonNull<u8>> = Default::default();

    traverse_all_hazard_datum(|data| {
        for index in HzSlotIndex::all() {
            let ptr = data.load_relaxed(index);
            if let Some(ptr) = NonNull::new(ptr) {
                hazard_ptr_set.insert(ptr);
            }
        }

        // Spin until observing is_loading_owned_sdarc being false
        // Use SeqCst ordering. (Using Acquire ordering pure load may observe arbitrarily stale data)
        data.reader_critical_section.collector_poll_until_false();
    });

    HazardPointerSet(hazard_ptr_set)
}

#[derive(Copy, Clone, Debug)]
pub(crate) enum HazardSlotStatus {
    Unused,
    UsedMaybeDangling,
    UsedNotDangling(SdarcInnerFatPtr),
}

/// Purely accessed by local thread. Not shared.
struct LocalOnlyHazardStatus {
    slot_statuses: [Cell<HazardSlotStatus>; HZ_PTR_SLOT_COUNT],
    index_to_start_check: Cell<HzSlotIndex>,
}

impl LocalOnlyHazardStatus {
    fn get_status(&self, index: HzSlotIndex) -> HazardSlotStatus {
        self.slot_statuses[index.0 as usize].get()
    }

    fn set_status(&self, index: HzSlotIndex, status: HazardSlotStatus) {
        self.slot_statuses[index.0 as usize].set(status)
    }
}

thread_local! {
    static LOCAL_ONLY_HAZARD_STATUS: LocalOnlyHazardStatus =
        LocalOnlyHazardStatus{
            slot_statuses: std::array::from_fn(|_| Cell::new(HazardSlotStatus::Unused)),
            index_to_start_check: Cell::new(HzSlotIndex(0))
        };
}

pub struct HazardPointerGuard<'a, T> {
    hazard_slot_index: HzSlotIndex,
    loaded_ptr: ManuallyDrop<Sdarc<T>>,
    // limit the lifetime of guard object
    _limit_lifetime: PhantomData<&'a SdarcInner<T>>,
    // make this type not Send or Sync
    _no_send_sync: PhantomData<*mut u8>,
}

impl<'a, T> Drop for HazardPointerGuard<'a, T> {
    fn drop(&mut self) {
        unpublish_hazard_pointer_on_borrow_finish(
            self.hazard_slot_index,
            self.loaded_ptr.inner_ptr,
        );
    }
}

impl<'a, T> Deref for HazardPointerGuard<'a, T> {
    type Target = Sdarc<T>;

    fn deref(&self) -> &Self::Target {
        &self.loaded_ptr
    }
}

#[allow(clippy::needless_lifetimes)]
pub(crate) fn borrow_from_atomic_ptr_using_hazard_pointer<'a, T>(
    atomic_ptr: &'a AtomicPtr<SdarcInner<T>>,
) -> Option<HazardPointerGuard<'a, T>> {
    // Use Relaxed load because it's just a pre-read.
    // The publishing of hazard pointer use AckRel, and hazard pointer has data dependency to it,
    // so it cannot be moved to after publishing hazard pointer.
    let mut pre_loaded_ptr = atomic_ptr.load(Ordering::Relaxed);

    loop {
        let pre_loaded_ptr_nn = match NonNull::new(pre_loaded_ptr) {
            None => {
                // If it's null, directly finish, no need to touch hazard pointer
                return None;
            }
            Some(p) => p,
        };

        // after the pre-loading, current thread may be un-scheduled,
        // then atomic pointer can change and the original SdarcInner may be freed.
        // at this time, the pre_loaded_ptr may be dangling.

        // publish the maybe-dangling ptr as hazard pointer.
        // the collector will only compare hazard pointers with non-dangling pointers.
        // the collector will never dereference hazard pointer.
        // (it's possible that the dangling pointer coincidentally equals a living SdarcInner pointer with another type,
        // it's fine because it will just delay collection. this is temporary and won't cause long-term leak.)
        let hazard_slot_index = publish_maybe_dangling_hazard_pointer(pre_loaded_ptr_nn);

        // This light barrier has SeqCst ordering between collector's heavy barrier.
        // If the heavy barrier is ordered after this, collector can observe hazard pointer.
        // If the heavy barrier is ordered before this, there are two cases:
        // - atomic ptr writer's SeqCst write is before collector heavy barrier, then the re-load can observe the new pointer
        // - atomic ptr writer's SeqCst write is after collector heavy barrier.
        //   in that case the reader and writer has no ordering because light barrier doesn't sync with SeqCst.
        //   the reader re-load can load stale pointer, which points to SdarcInner whose ref count sum is 0.
        //   but it's still safe because collector requires two iterations to free.
        //   the next collector iteration's heavy barrier will be likely after this light barrier, so collector can
        //   observe the hazard pointer if borrowing hasn't finished.
        //   however there is a rare case where collector's next iteration's heavy barrier is still before this light barrier, but after the writer SeqCst write,
        //   so collector can free it and borrowing is unsound! TODO
        membarrier2::light();

        let re_loaded_ptr = atomic_ptr.load(Ordering::Relaxed);

        // if pointer equals, everything is fine. pointer is not dangling. borrow succeeded.
        // it's possible that object pointed by pre_loaded_ptr was freed, then another object is allocated in same address coincidentally.
        // it's fine because the type system ensures type is same. we just borrow the new object.
        // (in that case, the re_loaded_ptr has different provenance to pre_loaded_ptr, so it uses re_loaded_ptr)
        if re_loaded_ptr == pre_loaded_ptr {
            let re_loaded_ptr = NonNull::new(re_loaded_ptr).unwrap();
            mark_hazard_pointer_non_dangling(hazard_slot_index, re_loaded_ptr);
            return Some(HazardPointerGuard {
                hazard_slot_index,
                loaded_ptr: ManuallyDrop::new(unsafe { Sdarc::from_raw_ptr(re_loaded_ptr) }),
                _limit_lifetime: PhantomData,
                _no_send_sync: PhantomData,
            });
        } else {
            // The atomic ptr changed. the pointer may be dangling. un-publish hazard pointer.
            // in normal hazard pointer implementation, it will retry, but in this library
            // it will use reader critical section to load owned Sdarc.
            // The reader critical section is slower than hazard pointer. Reader critical section
            // takes 3 atomic RMW(read-modify-write) ops to load (two for critical section counter,
            // 1 for reference count increment). Hazard pointer takes only 1 RMW to start borrowing if succeeded.
            // But hazard pointer borrowing may fail. If it retries on fail, then it may take

            unpublish_hazard_pointer_on_load_failed(hazard_slot_index);
            pre_loaded_ptr = re_loaded_ptr;
            continue;
        }
    }
}

// Consider this interleave:
// - reader pre-reads atomic pointer
// - collector finished checking hazard pointers
// - reader publishes hazard pointer
// - reader re-reads, pointer matches, start borrowing
// - writer changes atomic pointer, decrement original object's reference count sum to 0
// - collector reads the original object's reference count sum, observed 0
// This case is still safe because collector requires at least two iterations to drop one SdarcInner.
// In the next collector iteration it re-reads hazard pointers. If borrow still hasn't finished,
// collector won't free the SdarcInner.

fn publish_maybe_dangling_hazard_pointer<T>(ptr: NonNull<SdarcInner<T>>) -> HzSlotIndex {
    let shared = curr_thread_shared_hazard_data();

    LOCAL_ONLY_HAZARD_STATUS.with(|local| {
        let start_index = local.index_to_start_check.get();

        for index in HzSlotIndex::all_start_from(start_index) {
            match local.get_status(index) {
                HazardSlotStatus::Unused => {
                    local.index_to_start_check.set(index.offset(1));
                    shared.store_relaxed(index, ptr.as_ptr() as *mut u8);
                    local.set_status(index, HazardSlotStatus::UsedMaybeDangling);
                    return (index);
                }
                HazardSlotStatus::UsedMaybeDangling => {
                    panic!(
                        "Should not meet UsedMaybeDangling state in publish_hazard_pointer_seqcst"
                    )
                }
                HazardSlotStatus::UsedNotDangling(_) => {
                    // slot used, see other slots
                    continue;
                }
            }
        }

        let first_index = HzSlotIndex(0);

        let old_status_at_first_slot = local.get_status(first_index);
        let fat_ptr = match old_status_at_first_slot {
            HazardSlotStatus::UsedNotDangling(fat_ptr) => fat_ptr,
            _ => {
                panic!("Not expecting state {old_status_at_first_slot:?}");
            }
        };
        unsafe {
            fat_ptr
                .get_counters_ptr()
                .ptr_at_curr_thread_shard()
                .as_ref()
                .increment_ref_count_relaxed()
        };

        // If collector observes old hazard pointer being replaced,
        // collector must observe incremented reference count later.
        // Collector reads hazard pointers before reading reference counts.
        membarrier2::light();

        shared.store_relaxed(first_index, ptr.as_ptr() as *mut u8);
        local.set_status(first_index, HazardSlotStatus::UsedMaybeDangling);

        first_index
    })
}

fn unpublish_hazard_pointer_on_load_failed(index: HzSlotIndex) {
    let shared = curr_thread_shared_hazard_data();
    LOCAL_ONLY_HAZARD_STATUS.with(|local| {
        let status = local.get_status(index);
        assert!(matches!(
            status,
            HazardSlotStatus::UsedMaybeDangling | HazardSlotStatus::UsedNotDangling(_)
        ));
        shared.store_relaxed(index, null_mut());
        local.set_status(index, HazardSlotStatus::Unused);
    });
}

fn unpublish_hazard_pointer_on_borrow_finish<T>(
    index: HzSlotIndex,
    borrowed_ptr: NonNull<SdarcInner<T>>,
) {
    let shared = curr_thread_shared_hazard_data();
    LOCAL_ONLY_HAZARD_STATUS.with(|local| {
        let status = local.get_status(index);
        // The "debt" means it obtains a borrow but doesn't increment reference count.
        // (If it's cloned into an owned Sdarc, then borrowing the owned Sdarc involves no
        // reference count debt because reference count increased.)
        // The debt can be payed by incrementing reference count.
        // If the debt was payed, it should compensate by decrementing reference count when borrow finishes.
        // If the debt was never payed, we just clear the hazard pointer when borrow finishes, and the debt just vanishes.
        let was_ref_count_debt_payed = match status {
            HazardSlotStatus::Unused => {
                true
            }
            HazardSlotStatus::UsedMaybeDangling => {
                panic!("Not expecting UsedMaybeDangling in unpublish_hazard_pointer_on_borrow_finish_acqrel")
            }
            HazardSlotStatus::UsedNotDangling(fat_ptr) => {
                if fat_ptr.ptr == SdarcInnerPtrErased::from_typed(borrowed_ptr) {
                    false
                } else {
                    true
                }
            }
        };
        if !was_ref_count_debt_payed {
            unsafe { borrowed_ptr.as_ref() }
                .counters.at_curr_thread_shard().decrement_ref_count_and_set_tag_release();
        } else {
             shared.store_relaxed(index, null_mut());
        }
    })
}

fn mark_hazard_pointer_non_dangling<T>(
    index: HzSlotIndex,
    sdarc_inner_ptr: NonNull<SdarcInner<T>>,
) {
    LOCAL_ONLY_HAZARD_STATUS.with(|local| {
        let old_status = local.get_status(index);
        assert!(matches!(old_status, HazardSlotStatus::UsedMaybeDangling));
        local.set_status(
            index,
            HazardSlotStatus::UsedNotDangling(SdarcInnerFatPtr::new(sdarc_inner_ptr)),
        )
    });
}

pub(crate) fn load_atomic_ptr_owned<T>(atomic_ptr: &AtomicPtr<SdarcInner<T>>) -> Option<Sdarc<T>> {
    let shared = curr_thread_shared_hazard_data();

    shared.reader_critical_section.reader_critical_section(|| {
        // Using Relaxed here is fine because it was surrounded with light barriers
        // which have SeqCst ordering with collector
        let ptr = atomic_ptr.load(Ordering::Relaxed);

        match NonNull::new(ptr) {
            None => None,
            Some(ptr) => {
                let r = unsafe { ptr.as_ref() };

                r.counters
                    .at_curr_thread_shard()
                    .increment_ref_count_relaxed();

                Some(unsafe { Sdarc::from_raw_ptr(ptr) })
            }
        }
    })
}
