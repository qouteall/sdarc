//! When reading from atomic Sdarc pointer it firstly loads pointer then increment the reference count.
//!
//! But between loading pointer and incrementing ref count, the thread may be preempted, another thread could replace the pointer, then decrement original object's ref count so that sum becomes zero. If the first thread keeps not running for long time, the background collector could free the object. Then the first thread's incrementing of reference count will be use-after-free.
//!
//! So there is reader critical section. Before loading pointer, it increments critical section counter. After incrementing object reference count, it decrements critical section counter. The background thread will spin until all shards' critical section being 0 is observed once.
//!
//! It has some similarity to read-write lock, but with differences. The writer(background collector) can only spin until reader(critical section) count goes 0, but writer cannot acquire the lock. The reader never blocks.
//!
//! Mutating `AtomicSdarc` doesn't need to care about reader critical section. The collector cares about reader critical section.
//!
//! To understand it, there are 3 parties involved: reader, writer, collector.
//!
//! The reader calls [`AtomicNullableSdarc::load`], which:
//! 1. Increments critical section counter (one shard) in Acquire ordering
//! 2. Loads the atomic pointer in Acquire ordering
//! 3. Increments `Sdarc` reference count (one shard) by 1 in Relaxed ordering
//! 4. Decrements critical section counter (one shard) in Release ordering
//!
//! The writer calls [`AtomicNullableSdarc::swap`], which:
//! 1. Swaps the atomic pointer in Release ordering
//!
//! For the collector, in each iteration, it:
//! 1. Reads critical section counters using Acquire ordering.
//!    If it sees one non-zero, spin until observing that it's zero once.
//! 2. Reads reference counts using Acquire ordering
//!    (the Relaxed pre-check doesn't matter here because it doesn't affect ordering)
//! 3. If collector observes that reference count sum is zero, then reference count tags get cleared.
//!    If in the next collector iteration, no count tag gets set and ref count sum is 0,
//!    it frees memory.
//!
//!
//! It's possible that, right after collector finished checking critical section counter,
//! the reader starts `load`, then reader increment the critical section counter,
//! then load the pointer, then reader thread becomes un-scheduled for long time,
//! then writer mutates the pointer and drops the original `Sdarc`,
//! then collector sees ref counter sum being 0. However, it requires two iterations of collector
//! to free the memory. So memory is not freed at that time.
//! In the next collector interation,
//! collector will spin until observing each critical section counter becomes zero again.
//! If reader thread is still un-scheduled,
//! collector will wait until that reader thread is scheduled and finish reader critical section.
//! If collector sees reader critical section finishes (critical section counter being 0),
//! the collector must be able to see the reader's counter increment due to Release-Acquire ordering,
//! despite reader's ref count increment use Relaxed ordering.
//!
//! ---
//!
//! The weak reference upgrade [`WeakSdarc::upgrade`] also uses reader critical section.
//!
//! Assume that:
//! - A reader thread tries [`WeakSdarc::upgrade`]. It increments critical section counter,
//!   then loaded weak backref, which is not-null.
//! - The collector observed that strong ref count sum being 0 and stays same for two times.
//!   Collector clears the weak backref, found that the previous backref is non-null,
//!   then collector doesn't free it in current iteration. It will check in next iteration.
//! - In next iteration of collector, it waits until reader thread finish critical section.
//!   When reader thread finishes critical section, collector will observe strong count sum being non-zero.

use crate::shard_index::{shard_indexes, ShardIndex};
use crate::sharded_alloc::ShardedBox;
use log::warn;
use std::hint::spin_loop;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;

pub(crate) struct ReaderCriticalSection {
    counters: ShardedBox<AtomicU64>,
}

pub(crate) static READER_CRITICAL_SECTION: LazyLock<ReaderCriticalSection> =
    LazyLock::new(|| ReaderCriticalSection::new());

impl ReaderCriticalSection {
    pub fn new() -> Self {
        Self {
            counters: ShardedBox::<AtomicU64>::allocate_data_in_each_shard(|_| AtomicU64::new(0)),
        }
    }

    /// See module-level doc for details.
    ///
    /// The `func` should finish quickly and should not block.
    ///
    /// It's used by:
    /// - [`AtomicNullableSdarc::load`]
    /// - [`WeakSdarc::upgrade`]
    pub fn reader_critical_section<R>(&self, func: impl FnOnce() -> R) -> R {
        let counter: &AtomicU64 = self.counters.at_curr_thread_shard();

        /// Acquire ordering:
        /// - ensure that the loading of atomic pointer is after
        /// incrementing of critical section counter.
        ///
        /// It doesn't need Release ordering.
        /// Even if it uses Release, it's synchronize-with collector checking critical section counter is useless.
        /// As mentioned in outer doc, enter critical section right after collect finished checking
        /// critical section counter is fine, as freeing requires at least two stages.
        counter.fetch_add(1, Ordering::Acquire);

        let _guard = scopeguard::guard((), |()| {
            /// Release ordering:
            /// - ensure that if [`Self::spin_until_observing_non_critical_section_once_in_each_shard`]
            ///   observes critical counter being zero, it must observe incremented reference counter
            ///   and won't free wrongly
            counter.fetch_sub(1, Ordering::Release);
        });

        func()
    }

    /// Spin until observing that each shard is not in critical section once.
    ///
    /// It's just for ensuring that no thread stuck in critical section to continue collection.
    ///
    /// After this finishes, a reader thread could enter critical section in parallel with collection.
    /// But it's ok, because the collector will wait until counter sum goes 0 and keeps being same across one iteration.
    pub fn spin_until_observing_non_critical_section_once_in_each_shard(&self) {
        let mut shards_to_spin: Vec<ShardIndex> = Vec::new();

        for shard_index in shard_indexes() {
            let counter: &AtomicU64 = &self.counters[shard_index];

            /// Acquire ordering:
            /// Synchronize-with [`Self::reader_critical_section`] 's decrementing of critical section counter,
            /// ensure that if zero counter is observed, can observe the incremented reference count
            let counter_num = counter.load(Ordering::Acquire);
            if counter_num != 0 {
                shards_to_spin.push(shard_index);
            }
        }

        for shard_index in shards_to_spin {
            let counter: &AtomicU64 = &self.counters[shard_index];
            let mut spin_count: u64 = 0;

            // spin until it becomes zero
            'spin_loop: loop {
                // Why use Acquire ordering: same as the above
                let counter_num = counter.load(Ordering::Acquire);
                if counter_num == 0 {
                    break 'spin_loop;
                } else {
                    spin_loop();
                    spin_count += 1;

                    if spin_count == 100000 {
                        self.warn_about_too_long_spin(shard_index);
                    }
                }
            }
        }
    }

    fn warn_about_too_long_spin(&self, shard_index: ShardIndex) {
        let counters_for_logging: Vec<u64> = shard_indexes()
            .map(|shard_index| self.counters[shard_index].load(Ordering::Relaxed))
            .collect();

        warn!(
            "Critical section spins too much times on shard {:?}. Some possible causes: 1. a reader thread stuck too long time in critical section, 2. a reader thread was force-killed and didn't decrement counter, 3. other bugs. Current counters {:?}",
            shard_index, counters_for_logging
        );
    }
}
