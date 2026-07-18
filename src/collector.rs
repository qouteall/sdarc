use crate::reader_critical_section::READER_CRITICAL_SECTION;
use crate::sdarc::{ClearWeakBackRefResult, SdarcInnerFatPtr};
use crate::shard_index::{ShardsArr, shard_indexes};
use crate::sharded_alloc::FULL_SHARD_ALLOC;
use crossbeam::utils::CachePadded;
use log::{debug, error};
use parking_lot::Mutex;
use std::ops::{Deref, DerefMut};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use std::{env, mem, panic, thread};
use crate::env_params::CollectorParams;

pub(crate) struct CollectorShared {
    params: CollectorParams,
    thread_handle: JoinHandle<()>,
    /// Every time a new `Sdarc` is allocated, it's put into here.
    /// It's also sharded.
    ///
    /// Why not use [`sharded_alloc::ShardedBox`]: it can only hold 8 bytes per shard,
    /// but Vec is larger than that.
    pending_to_track: ShardsArr<CachePadded<Mutex<CollectorPendingDataShard>>>,

    collection_iteration_counter: AtomicU64,
}

pub(crate) struct CollectorPendingDataShard {
    new_counters_to_track: Vec<SdarcInnerFatPtr>,
}

impl CollectorPendingDataShard {
    pub fn new() -> CollectorPendingDataShard {
        Self {
            new_counters_to_track: Vec::new(),
        }
    }
}

impl CollectorShared {
    fn new(params: CollectorParams) -> Self {
        Self {
            params,
            thread_handle: thread::spawn(move || {
                let r = panic::catch_unwind(|| collector_thread_main());
                match r {
                    Ok(()) => {
                        error!("Collector main should not finish.")
                    }
                    Err(err) => {
                        error!("Collector panicked {err:?}");
                        eprintln!("Collector panicked {err:?}");
                    }
                }
            }),
            // The CachePadded ensure the rwlock and vec's outer 3 fields (ptr, length and capacity) are in unique cache lines.
            // The 8 ensures initial inner spaces are in unique cache lines.
            pending_to_track: ShardsArr::new(|_| {
                CachePadded::new(Mutex::new(CollectorPendingDataShard::new()))
            }),
            collection_iteration_counter: AtomicU64::new(0),
        }
    }

    fn on_new_sdarc_allocated(&self, fat_ptr: SdarcInnerFatPtr) {
        self.pending_to_track
            .at_curr_thread_shard()
            .lock()
            .new_counters_to_track
            .push(fat_ptr);
    }
}

pub(crate) fn on_new_sdarc_allocated(fat_ptr: SdarcInnerFatPtr) {
    get_collector().on_new_sdarc_allocated(fat_ptr);
}

static COLLECTOR: OnceLock<CollectorShared> = OnceLock::new();

fn get_collector() -> &'static CollectorShared {
    COLLECTOR.get_or_init(|| CollectorShared::new(CollectorParams::new_from_env_var()))
}

/// Interrupt the collector thread from parking.
///
/// Note that this function doesn't ensure early dropping of data when reference count sum goes 0.
pub fn collector_update_now() {
    get_collector().thread_handle.thread().unpark();
}

struct CollectorThreadState {
    collector: &'static CollectorShared,
    tracked_counters: Vec<TrackedCounter>,
}

struct TrackedCounter {
    sdarc_fat_ptr: SdarcInnerFatPtr,
    state: TrackedCounterState,
}

pub(crate) enum TrackedCounterState {
    DefaultState,
    RequiresReChecking,
    ReadyToFree,
}

impl TrackedCounter {
    fn new(sdarc_erased_info: SdarcInnerFatPtr) -> Self {
        Self {
            sdarc_fat_ptr: sdarc_erased_info,
            state: TrackedCounterState::DefaultState,
        }
    }

    fn update_state(&mut self) {
        match self.state {
            TrackedCounterState::DefaultState => {
                let relaxed_sum = read_ref_count_sum_relaxed(self.sdarc_fat_ptr);
                if relaxed_sum == 0 {
                    let sum = clear_tags_and_read_ref_count_sum_relaxed(self.sdarc_fat_ptr);
                    if sum == 0 {
                        self.state = TrackedCounterState::RequiresReChecking;
                    } else {
                        // The observed counter sum changed from 0 to nonzero.
                        // It's normal because there are race conditions.
                        // It stays in default state.
                        // There is side effect that counter tags are cleared. No need to re-set tags.
                        // Because after observing counter sum being 0 again tags will be re-cleared.
                    }
                }
            }
            TrackedCounterState::RequiresReChecking => {
                let opt_sum = read_ref_count_sum_if_all_tags_unset_acquire(self.sdarc_fat_ptr);
                match opt_sum {
                    None => {
                        // Observed that some tag is set. There is counter decrement in between.
                        // Go back to default state.
                        self.state = TrackedCounterState::DefaultState;
                    }
                    Some(sum) => {
                        if sum == 0 {
                            // At here we are confident that strong count sum reaches zero.
                            // However, weak reference upgrade may happen in parallel.
                            // so clear the weak backref so upgrade can no longer happen.
                            match self.sdarc_fat_ptr.clear_weak_back_ref() {
                                ClearWeakBackRefResult::WeakRefNotInvolved
                                | ClearWeakBackRefResult::WeakBackRefWasAlreadyNull => {
                                    // weak backref doesn't exist or was already cleared, ready to free
                                    self.state = TrackedCounterState::ReadyToFree;
                                }
                                ClearWeakBackRefResult::WeakBackRefCleared => {
                                    // Weak backref is cleared, upgrade can no longer happen,
                                    // but an upgrade may happen in parallel with clearing of weak backref
                                    // under reader critical section,
                                    // so re-check in next iteration of collection, which syncs
                                    // with reader critical section.
                                    self.state = TrackedCounterState::RequiresReChecking;
                                }
                            }
                        } else {
                            assert!(
                                sum > 0,
                                "In RequiresReChecking state, no tag is set, then counter sum should not be negative"
                            );

                            // No tag is set but counter sum is not zero
                            // Go back to default state.
                            self.state = TrackedCounterState::DefaultState;
                        }
                    }
                }
            }
            TrackedCounterState::ReadyToFree => {
                panic!("In state ReadyToFree, update_state should not be called")
            }
        }
    }
}

/// It uses Relaxed ordering to read counter sum.
/// Only when its result is 0 does counter sum be re-read using Acquire ordering.
///
/// In most cases, collector will see non-zero sum so this can improve collector performance.
fn read_ref_count_sum_relaxed(fat_ptr: SdarcInnerFatPtr) -> i64 {
    let mut sum: i64 = 0;

    let counters = unsafe { fat_ptr.get_counters().as_ref() };

    for shard_index in shard_indexes() {
        // Why use Relaxed ordering: it's just a pre-check
        let tagged_counter = counters[shard_index].load_relaxed();
        sum += tagged_counter.ref_count();
    }

    sum
}

fn clear_tags_and_read_ref_count_sum_relaxed(fat_ptr: SdarcInnerFatPtr) -> i64 {
    let mut sum: i64 = 0;

    let counters = unsafe { fat_ptr.get_counters().as_ref() };

    for shard_index in shard_indexes() {
        /// Why use Relaxed ordering: the [`read_ref_count_sum_if_all_tags_unset_acquire`]
        /// during re-check ensures correctness.
        let tagged_counter = counters[shard_index].fetch_and_clear_tag_relaxed();
        sum += tagged_counter.ref_count();
    }

    sum
}

/// If all tags are unset, returns Some containing counter sum
/// If one tag is set, return None
///
/// It uses Acquire ordering to read.
///
/// If there is decrement in parallel:
/// - If it observes decrement, then it observes tag being set, then collection will be delayed.
/// - If it doesn't observe decrement, it will observe counter sum higher than zero, so collection will still be delayed.
///
/// If there is increment in parallel:
/// - If the increment comes from existing strong reference, as increment happens-before decrement,
///   it cannot observe zero sum.
/// - If the increment comes from loading atomic pointer or weak ref upgrade,
///   reader critical section will ensure collector observes incremented counter.
fn read_ref_count_sum_if_all_tags_unset_acquire(fat_ptr: SdarcInnerFatPtr) -> Option<i64> {
    let mut sum: i64 = 0;

    let counters = unsafe { fat_ptr.get_counters().as_ref() };

    for shard_index in shard_indexes() {
        // Why use Acquire ordering: see function doc
        let tagged_counter = counters[shard_index].load_acquire();
        if tagged_counter.tag() {
            return None;
        }
        sum += tagged_counter.ref_count();
    }

    Some(sum)
}

impl CollectorThreadState {
    fn update(&mut self) {
        self.take_new_counters_to_track();

        // This is important
        READER_CRITICAL_SECTION.spin_until_observing_non_critical_section_once_in_each_shard();

        self.update_tracked_counters_and_collect();

        FULL_SHARD_ALLOC.do_maintenance_by_collector();

        if log::log_enabled!(log::Level::Trace) {
            FULL_SHARD_ALLOC.log_status_in_trace_level();
        }
    }

    fn update_tracked_counters_and_collect(&mut self) {
        for tracked_counter in &mut self.tracked_counters {
            tracked_counter.update_state();
        }

        let mut to_free: Vec<SdarcInnerFatPtr> = Vec::new();

        self.tracked_counters
            .retain(|tracked_counter| match &tracked_counter.state {
                TrackedCounterState::DefaultState => true,
                TrackedCounterState::RequiresReChecking => true,
                TrackedCounterState::ReadyToFree => {
                    to_free.push(tracked_counter.sdarc_fat_ptr);
                    false
                }
            });

        for fat_ptr in to_free {
            let res = panic::catch_unwind(move || {
                fat_ptr.free();
            });

            if let Err(err) = res {
                error!("Error dropping Sdarc content {:?} {:?}", fat_ptr, err);
            }
        }
    }

    /// It uses locking. The locking ensures that when it starts tracking a `SdarcInner`,
    /// so the collector won't observe uninitialized counter or uninitialized pointee.
    fn take_new_counters_to_track(&mut self) {
        for shard_index in shard_indexes() {
            // Use empty container to replace it, minimize time of taking lock
            let taken = {
                let mut guard = self.collector.pending_to_track[shard_index].deref().lock();
                mem::replace(guard.deref_mut(), CollectorPendingDataShard::new())
            };

            self.tracked_counters.extend(
                taken
                    .new_counters_to_track
                    .into_iter()
                    .map(|fat_ptr| TrackedCounter::new(fat_ptr)),
            );
        }
    }
}

fn collector_thread_main() {
    debug!("Collector thread started");

    let collector = get_collector();

    let mut state: CollectorThreadState = CollectorThreadState {
        collector,
        tracked_counters: Vec::new(),
    };

    loop {
        // This counter is just for logging, Relaxed ordering is fine
        let iteration_counter = collector
            .collection_iteration_counter
            .fetch_add(1, Ordering::Relaxed);

        let iteration_start_time = Instant::now();

        state.update();

        let elapsed_time = iteration_start_time.elapsed();

        debug!("Collection iteration {iteration_counter} took {elapsed_time:?}");

        let to_wait = collector.params.interval.saturating_sub(elapsed_time);

        debug!("Collector thread is going to wait {to_wait:?}");

        thread::park_timeout(to_wait);
    }
}
