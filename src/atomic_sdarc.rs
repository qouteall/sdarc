//! Atomic Sdarc pointers.
//!
//! For usage, see [`AtomicSdarc`] and [`AtomicNullableSdarc`].
//!
//! The below is implementation explanation.
//!
//! # Implementation explanation
//!
//! It uses the [asymmetric fence](https://www.open-std.org/jtc1/sc22/wg21/docs/papers/2022/p1202r4.pdf)
//! via [membarrier2](https://docs.rs/membarrier2/latest/membarrier2/) crate.
//!
//! Summarize asymmetric fence:
//!
//! - Light fence is just compiler fence that prevents compiler from reordering instructions,
//!   but doesn't interfere CPU's out-of-order execution.
//! - Heavy fence uses OS API that sends interrupt to every core executing current process's thread.
//! - The SeqCst operation implicitly includes a compiler fence. SeqCst operation "contains" light fence.
//! - A light fence sync with heavy fence similar to SeqCst. However, the light fences don't sync with each other.
//!   The light fences can have indirect ordering via the heavy fence.
//! - The asymmetric fence is not yet included in C++ memory model (that Rust uses).
//!   Here the light fence and heavy fence will be treated as SeqCst fence but only works between the two threads,
//!   one using heavy fence and another using light fence.
//!
//! The safety relies on:
//!
//! - The collector takes two iterations observing zero ref count sum to free one object.
//! - The collector will run a `membarrier2::heavy()` before each iteration, then read hazard pointers,
//!   and spin until observing reader critical section in inactive state once.
//! - The reader light fence doesn't directly sync with writer. But they sync with collector heavy fence,
//!   so the indirect ordering can be established.
//!
//! Hazard pointer prevents collector from collecting it. It's a way to keep pointee alive even
//! when reference count sum is 0.
//!
//! ## Safety and memory ordering of reader critical section
//!
//! The reader critical section is used for protecting [`AtomicNullableSdarc::load_owned`].
//! The `load_owned` reads atomic pointer then increments ref count of one shard.
//! The problem is that right after loading pointer and before incrementing ref count,
//! the object could be freed, then incrementing ref count is use-after-free.
//! The reader critical section prevents that problem.
//!
//! There is [`PerThreadReaderCriticalSectionFlag`]. It will be used by reader and collector, but not writer.
//! Changing atomic pointer doesn't do any special synchronization with collector or reader.
//!
//! There are two ways to view the critical section flag:
//!
//! - It can be seen as a special spinlock, except that reader thread never spins
//!   and directly acquires lock (always succeed),
//!   collector just keeps polling it until it's not locked.
//! - It can also be seen as a "universal" hazard pointer that correspond to any data managed by Sdarc.
//!
//! Reader thread:
//! 1. set critical section flag to true, Relaxed
//! 2. light barrier
//! 3. load atomic pointer, Acquire
//!
//!    (Reader thread can be un-scheduled here)
//!
//! 4. increment ref count, Relaxed
//! 5. set critical section flag to false, Release
//!
//! Writer thread:
//! 1. swap atomic pointer, SeqCst
//! 2. decrement ref count of original object, Release
//!
//! Collector thread:
//! 1. heavy barrier
//! 2. for each thread's critical section flag, poll using Acquire until observing false
//! 3. read counter sum as zero in Acquire ordering. clear tags (there is a relaxed pre-read before, doesn't matter in ordering)
//! 4. in next collector inner iteration, heavy barrier again, poll until observing false again
//! 5. read counter sum in Acquire ordering, if it stays 0 and no tag is observed, free the memory
//! (assume that no weak ref is involved. if weak ref is involved there is one extra iteration)
//!
//! Normally if reader is unscheduled between 3 and 4, collector will be polling, until reader exits critical section.
//!
//! Is it safe when right after collector finishes polling reader sets flag to true?
//! If reader sets flag after collector's first iteration's polling,
//! it's still safe because it requires two collector iterations to free one object.
//! What if reader sets flag after collector's second iteration then reads a pointer that's about to be freed?
//! It's still safe, explained below.
//!
//! Consider this extreme case:
//!
//! - Writer swaps pointer and decrement reference count.
//! - Collector does first iteration, observe reference count sum is 0, clear tags
//! - Collector runs second iteration, and reader starts reading in parallel
//!
//! The collector's second iteration's heavy barrier CH2 has a total order with reader's light barrier RL.
//! There are two cases:
//!
//! 1. RL is before CH2. Reader's setting of critical section flag happens-before RL,
//!    RL happens-before CH2, CH2 happens-before collector's reading of critical section flag.
//!    So flag being true is visible to collector.
//!    Even if reader loads a stale pointer, collector's observing of flag being true blocks collection,
//!    so the stale pointer cannot be freed.
//!    When collector observes flag being false, the reader's increment is sequenced-before setting flag to false
//!    in Release, so collector observing flag being false using Acquire is able
//!    to observe incremented ref count later. Even if reader reads a stale pointer, collector can observe the
//!    incremented ref count. So it's safe.
//!
//! 2. CH2 is before RL. Consider that in previous iteration collector has observed zero ref count sum using Acquire,
//!    and writer's decrementing of ref count uses Release, and writer's swapping of pointer is sequenced-before
//!    decrementing ref count,
//!    so the swapping pointer happens-before collector's counter read in first iteration, which happens-before CH2.
//!    Now RL is after CH, so the swapped pointer should also
//!    be observable to reader. Reader won't read the stale pointer that is about to be freed.
//!
//! Someone may argue that start from C++20, the SeqCst total order is no longer required
//! to be consistent with happens-before relation. But that exception only happens
//! when one atomic variable is sometimes operated using release-acquire and sometimes using seqcst.
//! That doesn't happen in this library.
//!
//! The visibility chain in the second case (CH2 is before RL):
//!
//!  1. Writer swaps atomic pointer, SeqCst
//!  2. Writer decrements ref count of old object, Release (sequenced-after 1)
//!  3. Collector read ref count sum and get 0, Acquire (synchronizes-with 2)
//!  4. Collector second iteration heavy fence CH2 (sequenced-after 3)
//!  5. Reader light fence RL (sync between heavy fence and light fence, after 4)
//!  6. Reader read atomic pointer, Acquire (sequenced-after 5)
//!
//! The SeqCst fence contains the power of AcqRel fence so the chain holds.
//! Writer's atomic pointer swap is visible to reader.
//!
//! Someone may argue that the setting of critical section flag to true uses Relaxed ordering
//! which doesn't pair with reading of flag using Acquire.
//! Because making flag becoming true visible is done by asymmetric fence, mentioned earlier.
//! The setting flag to false uses Release which pairs with Acquire. It ensures that when
//! collector has observed flag being true, then reader's incrementing counter
//! happens-before collector's observing flag being false.
//!
//! ## Safety and memory ordering of hazard pointer
//!
//! The `load_owned` is already kind of fast. But in short-term read, it involves two atomic read-modify-write
//! instructions (increment and decrement).
//! For frequent short-term reads it's not fast enough. So there is hazard pointer mechanism.
//!
//! Hazard pointer mechanism allows a `Sdarc` pointee to be alive even if ref count sum becomes 0.
//! Each thread has fixed amount of hazard pointer slots. When slots are full,
//! it falls back to `load_owned`.
//! Also when the re-loaded pointer is different to pre-loaded pointer it also falls back to `load_owned`.
//! The hazard pointer borrower doesn't do retry. This gives an upper bound of instruction count required to
//! start borrowing hazard pointer
//! (instruction count is bounded, but time doesn't necessarily bound as OS scheduling is non-deterministic).
//!
//! Reader:
//! 1. load atomic ptr, Relaxed
//!
//!    (reader can be un-scheduled here)
//!
//! 2. publish hazard pointer, Relaxed
//!    (hazard pointer may be dangling at this time, but collector doesn't dereference hazard pointer so it's fine)
//! 3. light fence
//! 4. re-load atomic ptr, Acquire
//! 5. if two pointers equal, borrowing succeeded
//! 6. use the borrowed data
//! 7. when borrowing finishes, clear hazard pointer in Release
//!
//! Writer:
//! 1. swap atomic ptr, SeqCst
//! 2. decrement ref count of original object, Release
//!
//! Collector:
//! 1. in beginning of first iteration, heavy barrier (CH1)
//! 2. read hazard pointers, Acquire
//! 3. for a non-hazard `SdarcInner`, read reference count in Acquire
//!    (there is a Relaxed pre-read but doesn't matter in ordering)
//! 4. in beginning of second iteration, heavy barrier (CH2)
//! 5. read hazard pointers, Acquire
//! 6. re-check ref count for non-hazard `SdarcInner`s
//! 7. if ref count sum keeps being 0, and no tag is observed,
//!    and observed none of its hazard pointer in two iterations, free pointee
//!
//! Is it still safe when reader publishes hazard pointer right after collector finish checking hazard pointers in first iteration?
//! Yes because it requires two collector iterations to free one object.
//!
//! Reader light barrier doesn't sync with writer SeqCst write. Is it still safe when reader reads stale pointer?
//! Consider this extreme case:
//!
//! - Writer swap atomic pointer and decrement ref count of original object.
//! - Collector finishes first iteration, observed original object's zero ref count sum.
//! - Reader loads stale pointer. The collector second iteration runs in parallel.
//!
//! The reader's light fence Rl has total order with collector's second iteration heavy barrier CH2.
//! Similarly, there are two cases:
//!
//! - RL is before CH2. Then the writing of hazard pointer happens-before collector's reading of hazard pointer.
//!   Collector will observe the hazard pointer. Safe.
//! - CH2 is before RL. When collector observes zero ref count sum in first iteration using Acquire ordering,
//!   the changed atomic pointer should be visible to collector. The RL being after CH makes the changed atomic
//!   pointer visible to reader. Then re-read reads a different pointer, hazard pointer borrow failed. Safe.
//!   (It mixes SeqCst and Release-Acquire but still ensures reader observe new atomic pointer,
//!   similar to the previous reasoning of reader critical section.)
//!
//! What if right after the pre-read, the reader thread is unscheduled, then the pointee freed,
//! then another object coincidentally allocated with the same pointer, then that pointer
//! is stored to the atomic pointer? It's ok because the type system ensures
//! that two different objects' types are the same. The hazard borrowing succeeds on the new object.
//! In this case Miri consider the two equal pointers to have different provenance. The code
//! uses re-loaded pointer to start borrowing which carries the new provenance.
//!
//! The borrowed atomic ptr can increment counter, thus become a living `Sdarc`.
//! But in the [`crate::tagged_counter`] reasoning,
//! collector observing increment in delay is fine because increment
//! can only happen from another owner that collector can observe.
//! However, that reason breaks in hazard pointer because hazard pointer correspond to
//! no reference count but can allow cloning.
//! Is there a race condition that a hazard-pointer borrowed `Sdarc` gets cloned,
//! then borrowing finishes but the increment is not visible to collector?
//! The collector reads hazard pointer in Acquire. If collector reads the null
//! after borrowing finishes, collector can observe the incremented ref count.
//! However, in this case, null is ambiguous. The null could also be the null that's before borrowing.
//! Will it cause issue?
//!
//! Consider this case:
//!
//! Reader:
//! 1. a previous borrow to another `Sdarc` pointee finished.
//!    hazard pointer slot is set to null in Release (writing first null).
//! 2. try to borrow another pointee. publish hazard pointer.
//!    light fence (RL). re-load pointer. borrowing succeeded.
//! 3. clone the `Sdarc`, increments ref count in Relaxed
//! 4. stop borrowing. set hazard pointer slot to null in Release (writing second null).
//!
//! Collector:
//! 1. heavy barrier (CH1)
//! 2. load hazard pointers in Acquire
//! 3. load counters in Acquire
//! 4. heavy barrier (CH2)
//! 5. load hazard pointers in Acquire
//! 6. load counters in Acquire
//!
//! Consider the total order between RL, CH1 and CH2:
//!
//! - RL -> CH1 -> CH2. In that case, when collector loads null hazard pointer in Acquire, it must be the second null.
//!                     The collector's loading of hazard pointer happens-after incrementing ref count. Safe.
//! - CH1 -> RL -> CH2. Similar to the above, the collector second iteration's loading of hazard pointer in Acquire
//!                     cannot load the first null. If it's null it must be the second null. Safe.
//! - CH1 -> CH2 -> RL. The previous reasoning shows that reader re-load cannot read a stale pointer that's about to be freed.
//!
use crate::sdarc::{Sdarc, SdarcInner, SdarcInnerPtrErased};
use append_only_vec::AppendOnlyVec;
use crossbeam_utils::CachePadded;
use rustc_hash::FxHashSet;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ops::{Deref, Not};
use std::ptr::{NonNull, null_mut};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::{array, hint};

/// The atomic nullable `Sdarc` pointer.
///
/// It provides shallow interior mutability.
/// The pointer can be changed using immutable borrow.
pub struct AtomicNullableSdarc<T> {
    inner_ptr: AtomicPtr<SdarcInner<T>>,
}

unsafe impl<T: Send + Sync> Send for AtomicNullableSdarc<T> {}
unsafe impl<T: Send + Sync> Sync for AtomicNullableSdarc<T> {}

impl<T: Send + Sync> AtomicNullableSdarc<T> {
    pub const fn new() -> Self {
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

impl<T: Send + Sync> Default for AtomicNullableSdarc<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> AtomicNullableSdarc<T> {
    /// Load the atomic pointer as owned. Gives owned `Sdarc<T>` if not null. Gives None if null.
    ///
    /// If you just want to do short-term read, it's recommended to use [`AtomicNullableSdarc::borrow`] which
    /// can be faster (it uses hazard pointer, requiring no atomic read-modify-write operation in happy path).
    ///
    /// About implementation: [`PerThreadReaderCriticalSectionFlag`]
    pub fn load_owned(&self) -> Option<Sdarc<T>> {
        load_atomic_ptr_owned(&self.inner_ptr)
    }

    /// Set the atomic pointer and get the replaced one.
    pub fn swap(&self, sdarc: Option<Sdarc<T>>) -> Option<Sdarc<T>> {
        let new_ptr = Sdarc::nullable_into_raw_ptr(sdarc);

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
    /// It's free of ABA problem because `if_matches` is live.
    #[allow(clippy::result_unit_err)]
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
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        match r {
            Ok(original_ptr) => {
                assert_eq!(original_ptr, if_matches_ptr);

                // Setting succeeded, but the `then_set_ptr` comes from a borrowed Sdarc.
                // There is no Sdarc ownership transfer,
                // so we need to increment counter to compensate.
                // No need to use critical section here, because at this time at least one strong reference of `then_set` exists.
                if let Some(then_set_inner) = unsafe { then_set_ptr.as_ref() } {
                    then_set_inner
                        .counters
                        .at_curr_shard()
                        .increment_ref_count_relaxed();
                }

                // The original pointer was overwritten. Create a Sdarc to compensate.
                Ok(unsafe { Sdarc::nullable_from_raw_ptr(original_ptr) })
            }
            Err(_original_ptr) => Err(()),
        }
    }

    /// Borrow from the atomic pointer.
    /// The inner object will be kept alive using hazard pointer mechanism.
    /// The borrow stays valid as long as the guard object is live.
    /// After the atomic pointer changes, the existing guard still borrows the previous pointee.
    ///
    /// The guard can keep the Sdarc pointee alive even when its reference count sum reach zero.
    ///
    /// It returns None if the loaded pointer is null.
    ///
    /// About implementation: [`try_borrow_from_atomic_ptr_using_hazard_pointer`]
    #[allow(clippy::needless_lifetimes)]
    pub fn borrow<'a>(&'a self) -> Option<AtomicSdarcBorrowGuard<'a, T>> {
        borrow_from_atomic_ptr(&self.inner_ptr)
    }

    /// The `borrow` is fast enough for reader. But if you want faster read that avoids hazard pointer cost,
    /// you can keep a local (per-thread) `Sdarc<T>`, then periodically sync the atomic pointer to local `Sdarc<T>`.
    /// If the atomic pointer doesn't change, the sync involves almost no cost.
    pub fn sync_to(&self, target: &mut Option<Sdarc<T>>) {
        let target_curr_ptr = Sdarc::nullable_get_raw_ptr(target);
        let curr_atomic_ptr = self.inner_ptr.load(Ordering::Relaxed);
        if target_curr_ptr != curr_atomic_ptr {
            *target = self.load_owned();
        }
    }
}

impl<T> Drop for AtomicNullableSdarc<T> {
    fn drop(&mut self) {
        self.store(None);
    }
}

/// The atomic `Sdarc` pointer.
///
/// It provides shallow interior mutability.
/// The pointer can be changed using immutable borrow.
pub struct AtomicSdarc<T>(AtomicNullableSdarc<T>);

impl<T: Send + Sync> AtomicSdarc<T> {
    pub fn new(value: T) -> Self {
        Self(AtomicNullableSdarc::new_with_value(value))
    }

    /// Load the atomic pointer and give owned `Sdarc<T>`.
    ///
    /// If you just want to do short-term read, it's recommended to use [`AtomicSdarc::borrow`] which
    /// can be faster (it uses hazard pointer, requiring no atomic read-modify-write operation in happy path).
    ///
    /// About implementation: [`PerThreadReaderCriticalSectionFlag`]
    pub fn load_owned(&self) -> Sdarc<T> {
        self.0.load_owned().unwrap()
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
    /// It's free of ABA problem because `if_matches` is live.
    #[allow(clippy::result_unit_err)]
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

    /// Borrow from the atomic pointer.
    /// The inner object will be kept alive using hazard pointer mechanism.
    /// The borrow stays valid as long as the guard object is live.
    /// After the atomic pointer changes, the existing guard still borrows the previous pointee.
    ///
    /// The guard can keep the Sdarc pointee alive even when its reference count sum reach zero.
    ///
    /// About implementation: [`try_borrow_from_atomic_ptr_using_hazard_pointer`]
    #[allow(clippy::needless_lifetimes)]
    pub fn borrow<'a>(&'a self) -> AtomicSdarcBorrowGuard<'a, T> {
        self.0.borrow().unwrap()
    }

    /// The `borrow` is fast enough for reader. But if you want faster read that avoids hazard pointer cost,
    /// you can keep a local (per-thread) `Sdarc<T>`, then periodically sync the atomic pointer to local `Sdarc<T>`.
    /// If the atomic pointer doesn't change, the sync involves almost no cost.
    pub fn sync_to(&self, target: &mut Sdarc<T>) {
        let target_curr_ptr = target.inner_ptr.as_ptr();
        let curr_atomic_ptr = self.0.inner_ptr.load(Ordering::Relaxed);
        if target_curr_ptr != curr_atomic_ptr {
            *target = self.load_owned();
        }
    }
}

/// Why 15: the [`PerThreadSharedHazardData`] contains hazard pointer slots and two `AtomicBool`s.
/// And it's held in `CachePadded` which is 128 align in mainstream platform.
/// If we use 8, then it wastes space. Using 15 will make padding space less than a pointer width.
/// The added scanning is cheap because they are in same cache line.
const HZ_PTR_SLOT_COUNT: usize = 15;

#[derive(Copy, Clone, Debug)]
struct HzSlotIndex(u8);

pub(crate) struct PerThreadReaderCriticalSectionFlag(AtomicBool);

impl PerThreadReaderCriticalSectionFlag {
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
            self.0.store(false, Ordering::Release);
        });

        func()
    }

    fn get_relaxed(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Spin until observing that flag being false once.
    fn collector_poll_until_false(&self) {
        loop {
            let r = self.0.load(Ordering::Acquire);
            if r.not() {
                return;
            }

            hint::spin_loop();
        }
    }
}

/// It will only be written by local thread.
/// The collector thread only reads it.
/// This makes it much simpler than arc-swap's hazard pointer implementation. Also, there is no "debt paying".
pub(crate) struct PerThreadSharedHazardData {
    pub hazard_ptr_slots: [AtomicPtr<u8>; HZ_PTR_SLOT_COUNT],
    pub is_used: AtomicBool,

    pub reader_critical_section_flag: PerThreadReaderCriticalSectionFlag,
}

impl PerThreadSharedHazardData {
    fn load_relaxed(&self, index: HzSlotIndex) -> *mut u8 {
        self.hazard_ptr_slots[index.0 as usize].load(Ordering::Relaxed)
    }

    fn load_acquire(&self, index: HzSlotIndex) -> *mut u8 {
        self.hazard_ptr_slots[index.0 as usize].load(Ordering::Acquire)
    }

    fn store_relaxed(&self, index: HzSlotIndex, ptr: *mut u8) {
        self.hazard_ptr_slots[index.0 as usize].store(ptr, Ordering::Relaxed)
    }

    fn store_release(&self, index: HzSlotIndex, ptr: *mut u8) {
        self.hazard_ptr_slots[index.0 as usize].store(ptr, Ordering::Release)
    }
}

impl HzSlotIndex {
    fn all() -> impl Iterator<Item = HzSlotIndex> {
        (0..HZ_PTR_SLOT_COUNT).map(|n| HzSlotIndex(n as u8))
    }
}

// The AppendOnlyVec is simpler than manually making a lock-free linked list.
// It's append-only, no freeing, avoid race condition of freeing it.
// Its size is as large as the max amount of threads that use atomic sdarc at the same time.
static SHARED_HAZARD_DATA: AppendOnlyVec<CachePadded<PerThreadSharedHazardData>> =
    AppendOnlyVec::new();

fn obtain_per_thread_shared_hazard_data() -> &'static PerThreadSharedHazardData {
    for e in SHARED_HAZARD_DATA.iter() {
        let old_is_used = e.is_used.swap(true, Ordering::Acquire);
        if !old_is_used {
            // claimed by current thread
            return e.deref();
        }
    }

    let r = PerThreadSharedHazardData {
        hazard_ptr_slots: array::from_fn(|_| AtomicPtr::new(null_mut())),
        is_used: AtomicBool::new(true),
        reader_critical_section_flag: PerThreadReaderCriticalSectionFlag::new(),
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
            .all(|ptr| ptr.load(Ordering::Relaxed).is_null()),
        "During thread-local destruction, all hazard pointer borrowing should finish. \
        You should NOT make thread-local own an AtomicSdarcBorrowGuard."
    );
    assert!(r.reader_critical_section_flag.get_relaxed().not());

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
            let ptr = data.load_acquire(index);
            if let Some(ptr) = NonNull::new(ptr) {
                hazard_ptr_set.insert(ptr);
            }
        }

        data.reader_critical_section_flag
            .collector_poll_until_false();
    });

    HazardPointerSet(hazard_ptr_set)
}

/// This guard should be held in local variable. It should not be owned by thread local.
pub(crate) struct HazardPointerGuard<'a, T> {
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

/// This guard should be held in local variable. It should not be owned by thread local.
/// Because its dropping may use other thread locals.
pub struct AtomicSdarcBorrowGuard<'a, T>(AtomicSdarcBorrowGuardInner<'a, T>);

// use a separate type because Rust doesn't allow public enum to have private variant
pub(crate) enum AtomicSdarcBorrowGuardInner<'a, T> {
    UsingHazardPtr(HazardPointerGuard<'a, T>),
    Owned(Sdarc<T>),
}

impl<'a, T> Deref for AtomicSdarcBorrowGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        match &self.0 {
            AtomicSdarcBorrowGuardInner::UsingHazardPtr(g) => g.deref(),
            AtomicSdarcBorrowGuardInner::Owned(sdarc) => sdarc.deref(),
        }
    }
}

#[allow(clippy::needless_lifetimes, clippy::manual_map)]
pub(crate) fn borrow_from_atomic_ptr<'a, T>(
    atomic_ptr: &'a AtomicPtr<SdarcInner<T>>,
) -> Option<AtomicSdarcBorrowGuard<'a, T>> {
    match try_borrow_from_atomic_ptr_using_hazard_pointer(atomic_ptr) {
        Ok(g) => match g {
            None => None,
            Some(g) => Some(AtomicSdarcBorrowGuard(
                AtomicSdarcBorrowGuardInner::UsingHazardPtr(g),
            )),
        },
        Err(_) => {
            let sdarc_opt = load_atomic_ptr_owned(atomic_ptr);
            match sdarc_opt {
                None => None,
                Some(sdarc) => Some(AtomicSdarcBorrowGuard(AtomicSdarcBorrowGuardInner::Owned(
                    sdarc,
                ))),
            }
        }
    }
}

pub(crate) enum HazardBorrowErr {
    SlotFull,
    PointerChanged,
}

#[allow(clippy::needless_lifetimes)]
pub(crate) fn try_borrow_from_atomic_ptr_using_hazard_pointer<'a, T>(
    atomic_ptr: &'a AtomicPtr<SdarcInner<T>>,
) -> Result<Option<HazardPointerGuard<'a, T>>, HazardBorrowErr> {
    let pre_loaded_ptr = atomic_ptr.load(Ordering::Relaxed);

    let pre_loaded_ptr_nn = match NonNull::new(pre_loaded_ptr) {
        None => {
            // If it's null, directly finish, no need to touch hazard pointer
            return Ok(None);
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
    let hazard_slot_index_opt = try_publish_maybe_dangling_hazard_pointer(pre_loaded_ptr_nn);

    let Some(hazard_slot_index) = hazard_slot_index_opt else {
        return Err(HazardBorrowErr::SlotFull);
    };

    // About safety, see function doc.
    membarrier2::light();

    // Why Acquire (instead of Relaxed): avoid reading uninitialized data from new pointer
    let re_loaded_ptr = atomic_ptr.load(Ordering::Acquire);

    // if pointer equals, everything is fine. pointer is not dangling. borrow succeeded.
    // it's possible that object pointed by pre_loaded_ptr was freed, then another object is allocated in same address coincidentally.
    // it's fine because the type system ensures type is same. we just borrow the new object.
    // (in that case, the re_loaded_ptr has different provenance to pre_loaded_ptr, so it uses re_loaded_ptr)
    if re_loaded_ptr == pre_loaded_ptr {
        let re_loaded_ptr = NonNull::new(re_loaded_ptr).unwrap();
        Ok(Some(HazardPointerGuard {
            hazard_slot_index,
            loaded_ptr: ManuallyDrop::new(unsafe { Sdarc::from_raw_ptr(re_loaded_ptr) }),
            _limit_lifetime: PhantomData,
            _no_send_sync: PhantomData,
        }))
    } else {
        // this should be very rare, unless atomic pointer is changing frequently
        let shared = curr_thread_shared_hazard_data();
        shared.store_relaxed(hazard_slot_index, null_mut());
        Err(HazardBorrowErr::PointerChanged)
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

fn try_publish_maybe_dangling_hazard_pointer<T>(
    ptr: NonNull<SdarcInner<T>>,
) -> Option<HzSlotIndex> {
    let shared = curr_thread_shared_hazard_data();

    // linear scan, should be fast
    for index in HzSlotIndex::all() {
        let loaded_ptr = shared.load_relaxed(index);
        if loaded_ptr.is_null() {
            shared.store_relaxed(index, ptr.as_ptr() as *mut u8);
            return Some(index);
        }
    }

    None
}

fn unpublish_hazard_pointer_on_borrow_finish<T>(
    index: HzSlotIndex,
    _borrowed_ptr: NonNull<SdarcInner<T>>,
) {
    let shared = curr_thread_shared_hazard_data();

    debug_assert!(shared.load_relaxed(index).is_null().not());

    shared.store_release(index, null_mut());
}

/// See also [`PerThreadReaderCriticalSectionFlag`]
pub(crate) fn load_atomic_ptr_owned<T>(atomic_ptr: &AtomicPtr<SdarcInner<T>>) -> Option<Sdarc<T>> {
    let shared = curr_thread_shared_hazard_data();

    shared
        .reader_critical_section_flag
        .reader_critical_section(|| {
            // Use Acquire ordering (not Relaxed) to avoid reading uninitialized data (caught by miri).
            let ptr = atomic_ptr.load(Ordering::Acquire);

            match NonNull::new(ptr) {
                None => None,
                Some(ptr) => {
                    let r = unsafe { ptr.as_ref() };

                    r.counters
                        .at_curr_shard()
                        .increment_ref_count_relaxed();

                    Some(unsafe { Sdarc::from_raw_ptr(ptr) })
                }
            }
        })
}
