(This repo is work-in-progress)

# Sharded Deferred Atomic Reference Counting (sdarc)

`Arc` is commonly used in Rust. But when many threads increment/decrement same atomic counter, cache contention may hurt performance.

Examples:

- [The Concurrency Trap: How An Atomic Counter Stalled A Pipeline](https://www.conviva.ai/resource/the-concurrency-trap-how-an-atomic-counter-stalled-a-pipeline/)
- [How a Single Line of Code Made a 24-core Server Slower Than a Laptop](https://pkolaczk.github.io/server-slower-than-a-laptop/)

This library provides sharded-deferred-atomic-reference-counting (`Sdarc`). A thread increment/decrement the counter shard according to thread id hash. One counter shard can become negative. There is a background thread periodically observing the counters and do freeing. This reduces contention of incrementing/decrementing counter.

It uses tagged counter and multiphase collection to solve race condition.

Different counter shards of one `Sdarc` are in different cache lines. But the different `Sdarc`'s counters in same shard can be put together. This saves memory. This library provides general sharded allocation functionality that allows allocating 8 bytes per shard. This library also supports sharded RwLock (reader acquire one sharded lock, writer acquire all locks, readers have low contention with each other).

Its weak reference behavior is different to std `Arc`. Because that reclamation is deferred, upgrade from weak ref to strong ref can happen when strong counter sum is 0. The `Sdarc` can be "resurrected". After resurrection, the upgrading from weak ref to strong ref may fail or not fail.

This library doesn't suit these use ases:

- If `Arc` atomic counter contention is low (there won't be many threads increment/decrement same counter in parallel), don't use this library.
- If you want it to drop content immediately when strong reference count goes 0. (Collecting a deep structure drops layer-by-layer, so it may take long time to fully drop.)
- For millions of small object, don't use `Sdarc`. It's recommended to put them into an arena. The arena can be held in `Sdarc`.
- This library doesn't support no_std.

