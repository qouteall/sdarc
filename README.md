(This repo is work-in-progress)

# Sharded Deferred Atomic Reference Counting (sdarc)

`Arc` is commonly used in Rust. But when many threads increment/decrement same atomic counter, cache contention may hurt performance.

Examples:

- [The Concurrency Trap: How An Atomic Counter Stalled A Pipeline](https://www.conviva.ai/resource/the-concurrency-trap-how-an-atomic-counter-stalled-a-pipeline/)
- [How a Single Line of Code Made a 24-core Server Slower Than a Laptop](https://pkolaczk.github.io/server-slower-than-a-laptop/)

This library provides sharded-deferred-atomic-reference-counting (`Sdarc`). It can be used similar to `Arc`. A thread increment/decrement one counter shard according to thread id hash. The `Sdarc` can be freely sent and shared between threads, so one counter shard may become negative. There is a background thread periodically observing the counters and do freeing (it uses tagged counter and two-stage collecting to solve race condition). This reduces cache contention of incrementing/decrementing counter.

Different counter shards of one `Sdarc` are in different cache lines. But the different `Sdarc`'s counters in same shard can be put together. This saves memory. This library provides general sharded allocation functionality that allows allocating 8 bytes per shard. This library also supports sharded RwLock (reader acquire one sharded lock, writer acquire all locks, readers have low contention with each other).

This library also provides atomic pointers `AtomicSdarc` and `AtomicNullableSdarc` (has functionality similar to `ArcSwap`). It uses lock-free synchronization with the collector to solve race condition.

Its weak reference behavior is different to std `Arc`. Because that reclamation is deferred, upgrade from weak ref to strong ref can happen when strong counter sum is 0. The `Sdarc` can be "resurrected". After resurrection, the upgrading from weak ref to strong ref may fail or not fail.

Unlike `Arc` it doesn't support [`get_mut`](https://doc.rust-lang.org/std/sync/struct.Arc.html#method.get_mut), which gives mutable borrow when reference count is 1. Because there is no way to immediately know whether sharded counter sum is 1 without locking.

This library doesn't suit these use ases:

- If `Arc` atomic counter contention is low (there won't be many threads increment/decrement same counter in parallel), don't use this library.
- If you want it to drop content immediately when strong reference count goes 0.
- For millions of small object, don't use `Sdarc`. It's recommended to put them into an arena. The arena can be held in `Sdarc`.
- This library doesn't support no_std.

Compare it with Linux percpu-refcount TODO
