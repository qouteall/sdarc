//! Why the tagged counter is introduced: solving a race condition of collector reading counters.
//!
//! In Sdarc, each thread only increment/decrement counters in one shard.
//! One counter slot can go negative. It should be freed when counter sum goes 0.
//! However, there is no instruction to read all sharded counters at the same time atomically.
//! So the collector has to read counters one-by-one. Then there is chance of race condition.
//!
//! For example, assume there are two shards. Firstly counters are [0, 1]:
//! - Collector reads first counter, get 0
//! - A thread in first shard clones Sdarc, now counters are [1, 1]
//! - A thread in second shard drops Sdarc, now counters are [1, 0]
//! - Collector reads second counter, get 0
//! - Collector observed that the sum of counters is 0, but it's actually not zero. At that time, freeing is wrong.
//!
//! What if the collector reads the counters for two times? But the same thing can happen for two times, just with lower probability. Even if collector reads counters for one thousand times, it's still potentially unsafe. This interleave is valid even if all counter accesses use SeqCst memory ordering.
//!
//! It's solvable by making decrementing use locking. But locking may defeat the performance gain.
//!
//! This library solves it using tagged counter.
//!
//! A tagged counter is a 64-bit signed integer. The higher 63 bits is treated as reference count. The last bit is for tagging.
//! - Incrementing counter increments it by 2. (For the higher 63 bits, it increments by 1.)
//! - Decrementing counter decrements it by 2, and also set the last bit to 1. (For the higher 63 bits, it decrements by 1.) It happens atomically using compare_exchange_weak.
//!
//! When collector observes that ref counter sum is 0, it doesn't immediately free memory.
//! It atomically clears each counter's tag (set last bit to 0).
//! Then after some time it reads the counters again. If the reference count sum is still 0 and all tags are still 0, it means the counters haven't been decremented (given counter sum is still 0, it also haven't incremented).
//! Then it's safe to drop inner content.
//!
//! If the previously mentioned race condition happens,
//! one tag will be set, which can be observed by collector.
//!
//! The increment of counter uses Relaxed ordering. Because increment can only happen when an instance
//! of Sdarc is live, which means counter sum is at least 1. Collector delaying observing the increment is fine,
//! as long decrement is not visible before increment. Decrement uses Release which ensures that if the
//! decrement is observed, the previous increments in same thread is also observable. For cross-thread case (increment
//! in one thread, send to another thread to decrement), other synchronizations have established that incrementing counter inter-thread happens-before decrementing counter. (Even if increment is visible to collector before decrement, the collector's non-atomic way of reading counters can still cause race condition described above, which is solved by tagging.)
//!
//! The decrement and setting of bit use Release ordering. Collector reads counter using Acquire ordering.
//! (Collector firstly do pre-scan using Relaxed ordering, which is an optimization that doesn't affect ordering.)
//! If collector hasn't observed the decrement, then it's safe because reference count sum cannot be 0 before observing decrement. If the collector have observed the decrement, then collector can observe the tag being set then will delay collecting, so it's also safe.
//!
//! About overflow/underflow: the max reference count (higher 63 bit) is 2^62-1, min is -2^62. In ideal case, a fast uncontended atomic takes 3 cycles for 1 increment, given 4GHz frequency, overflowing/underflowing it takes about 110 years. If there is contention, incr/decr will be slower. So no need to care about overflow/underflow.
//!
//! Why not ensure that all counter shards are positive then have an extra counter tracking number of non-zero counters? Because the `Sdarc` can be sent across threads. One thread can increment one shard counter then send it to another thread then decrement another shard counter. It will naturally lead to negative counter shard. Trying to make every counter shard non-negative introduces new synchronization overhead that defeats the performance gain.

use std::fmt::{Debug, Formatter};
use std::sync::atomic::{AtomicI64, Ordering};

/// Higher 63 bits is a signed counter. The lowest 1 bit is tag.
#[derive(Copy, Clone, Eq, PartialEq)]
pub(crate) struct TaggedCounter(pub(crate) i64);

impl Debug for TaggedCounter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TaggedCounter({}, ref_count={}, tag={})",
            self.0,
            self.ref_count(),
            self.tag()
        )
    }
}

impl TaggedCounter {
    pub fn ref_count(self) -> i64 {
        // sign is preserved
        self.0 >> 1
    }

    pub fn tag(self) -> bool {
        self.0 & 1 != 0
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub(crate) struct AtomicTaggedCounter(pub(crate) AtomicI64);

impl AtomicTaggedCounter {
    pub fn new() -> AtomicTaggedCounter {
        // reference count 0, tag unset
        AtomicTaggedCounter(AtomicI64::new(0))
    }

    #[inline(always)]
    pub fn increment_ref_count_relaxed(&self) {
        self.0.fetch_add(2, Ordering::Relaxed);
    }

    const MASK_FOR_CLEARING_TAG: i64 = !1;

    pub fn decrement_ref_count_and_set_tag_release(&self) {
        // there is no one atomic instruction that does decrementing and logical AND at once,
        // so use compare_exchange loop.
        let mut value = self.0.load(Ordering::Relaxed);
        loop {
            // set the tag, minus reference count by 1
            let new_value = (value | 1) - 2;

            let r = self.0.compare_exchange_weak(
                value,
                new_value,
                Ordering::Release,
                Ordering::Relaxed,
            );

            match r {
                Ok(_) => {
                    return;
                }
                Err(v) => {
                    value = v;
                }
            }
        }
        // There is another design that allows decrementing using one instruction:
        // reserve 32 bits for reference count and 32 lower bits.
        // decrementing reference count decrements upper 32 bit by one but increments lower 32 bit by 1.
        // but 32 bit is easy to overflow. handling overflow is possible but more complex
        // (handling counter overflow needs to "even out" counters and reduce lower bits if too large)
    }

    pub fn fetch_and_clear_tag_relaxed(&self) -> TaggedCounter {
        // no need to use Release. decrementer use Release which won't sync with Release
        let v = self
            .0
            .fetch_and(Self::MASK_FOR_CLEARING_TAG, Ordering::Relaxed);
        TaggedCounter(v)
    }

    pub fn load_relaxed(&self) -> TaggedCounter {
        TaggedCounter(self.0.load(Ordering::Relaxed))
    }

    pub fn load_acquire(&self) -> TaggedCounter {
        TaggedCounter(self.0.load(Ordering::Acquire))
    }
}
