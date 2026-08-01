# Sharded Deferred Atomic Reference Counting (sdarc)

`Arc` is commonly used in Rust. But when many threads increment/decrement same atomic counter, cache contention may hurt performance. Examples:

- [The Concurrency Trap: How An Atomic Counter Stalled A Pipeline](https://www.conviva.ai/resource/the-concurrency-trap-how-an-atomic-counter-stalled-a-pipeline/)
- [How a Single Line of Code Made a 24-core Server Slower Than a Laptop](https://pkolaczk.github.io/server-slower-than-a-laptop/)

This library provides sharded-deferred-atomic-reference-counting (`Sdarc`). It can be used similar to `Arc`. A thread increment/decrement one counter shard. The `Sdarc` can be freely sent and shared between threads, so one counter shard may become negative. This reduces cache contention of incrementing/decrementing counter. 

It doesn't check all counters after decrement. There is a background collector thread periodically checking the counters and do freeing (it uses tagged counter and two-stage collecting to solve race condition).

### Shard selection

In Linux, it selects shard by libc `sched_getcpu` in each operation. In most cases `sched_getcpu` involve no syscall, so it should be fast enough. In Windows, it uses `GetCurrentProcessorNumber`. Using current CPU index can avoid contention.

In non-Linux non-Windows, it uses thread id hash modulo shard count as shard index (cached in thread local). It will have more cache contention, because shard index is fixed per thread. Different threads may have same shard index.

### API differences to `Arc`

The API of `Sdarc` is similar to `Arc`. Some differences:

- Sdarc doesn't support [`get_mut`](https://doc.rust-lang.org/std/sync/struct.Arc.html#method.get_mut), which gives mutable borrow when reference count is 1. Because `Sdarc` ref count operations are lock-free, and there is no way to know whether counter sum is 1 immediately.
- Doesn't yet support unsized type.
- TODO

### Atomic pointers

This library also provides atomic pointers `AtomicSdarc` and `AtomicNullableSdarc`. They have functionality similar to `ArcSwap`. It uses hazard pointer and [asymmetric fence](https://crates.io/crates/membarrier2) to solve race condition.

Unlike std `Arc`, the `AtomicSdarc`(and `AtomicNullableSdarc`) allows borrowing content using hazard pointer, without incrementing reference count. This can allow a pointee with zero ref count sum to live for long time.

### About weak reference

The `WeakSdarc` is the weak reference version of `Sdarc`. Its weak reference behavior is different to std `Arc` `Weak`. Because that reclamation is deferred, and there is hazard pointer mechanism, upgrading from weak ref to strong ref can succeed when strong counter sum is 0. The `Sdarc` can be "resurrected". After resurrection, the upgrading from weak ref to strong ref may fail or not fail.

### About sharded alloc

Different counter shards of one `Sdarc` pointee are in different cache lines. But the different `Sdarc` pointees's counters in same shard can be put together to save memory usage. This library provides general sharded allocation functionality that allows allocating 8 bytes per shard (`ShardedBox`). 

This library also supports `ShardedRwLock`. Reader acquire one sharded lock, writer acquire all locks, readers have low contention with readers. It's similar to crossbeam `ShardedLock`, but uses parking_lot rwlock and uses this library's sharded alloc.

### Is it GC?

It has a background thread periodically do collection (the background thread will be launched after first usage of `Sdarc`). This is similar to GC. But it's not tracing GC. It doesn't traverse the object graph, and it doesn't do stop-the-world.

### When to not use this

This library doesn't suit these use cases:

- If `Arc` atomic counter contention is low (there won't be many threads increment/decrement same counter in parallel), then `Sdarc` has higher overhead.
- If you want it to drop content immediately when strong reference count goes 0. (Sdarc collector frees layer-by-layer by default. A deep structure may take long time to be fully freed.) You can call `collector_update_now_and_wait` to force immediate collection, but it has higher overhead.
- For millions of small objects, don't use `Sdarc`. It's recommended to put them into an arena. The arena can be held in `Sdarc`.
- This library doesn't support no_std.

### Appendix

#### Comparison with Linux percpu-refcount:

- Linux percpu-refcount has a special owner and has two stages. In the first stage, increment/decrement use the per-CPU ref count slot. When the original owner drops reference, it switches to one atomic counter (the switching is synced by RCU). In `Sdarc` there is no special owner (similar to `Arc`).
- Linux percpu-refcount's second stage frees immediately after count reach zero. There is no background thread scanning.  In `Sdarc`, the freeing is deferred, driven by a background collector thread.
- Linux precpu-refcount uses non-atomic operation to increment/decrement per-CPU ref count. It relies on the fact that kernel code cannot be preempted. The user program can get CPU index via `sched_getcpu` but the thread could be scheduled to another CPU core right after calling it (unless pinned), so the user program has to use atomic operation.
