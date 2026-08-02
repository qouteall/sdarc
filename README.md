# Sharded Deferred Atomic Reference Counting (sdarc)

`Arc` is commonly used in Rust. But when many threads increment/decrement same atomic counter, cache contention may hurt performance. Examples:

- [The Concurrency Trap: How An Atomic Counter Stalled A Pipeline](https://www.conviva.ai/resource/the-concurrency-trap-how-an-atomic-counter-stalled-a-pipeline/)
- [How a Single Line of Code Made a 24-core Server Slower Than a Laptop](https://pkolaczk.github.io/server-slower-than-a-laptop/)

This library provides sharded-deferred-atomic-reference-counting (`Sdarc`). It can be used similar to `Arc`. A thread increment/decrement one counter shard. The `Sdarc` can be freely sent and shared between threads, so one counter shard may become negative. This reduces cache contention of incrementing/decrementing counter. 

It doesn't check all counters after decrement. There is a background collector thread periodically checking the counters and do freeing (it uses tagged counter and two-stage collecting to solve race condition).

### Shard selection

When cloning/dropping `Sdarc` it needs to select a shard using `curr_shard_index()`. The shard selection mechanism: 

- In Linux, it selects shard by libc `sched_getcpu`. Normally `sched_getcpu` involves no syscall, so it's fast enough.
  - Exception: musl in aarch64 uses syscall to implement `sched_getcpu`. Syscall is slow, so in musl aarch64 it uses the fallback case below. (This exception doesn't apply to musl in X86-64. musl in X86-64 uses vdso to implement `sched_getcpu` which doesn't involve syscall. This exception also does not apply when using glibc.)
- In Windows, it selects shard by `GetCurrentProcessorNumber`. `GetCurrentProcessorNumber` also involves no syscall.
- Fallback case. uses thread id hash modulo shard count as shard index (cached in thread local). It will have more cache contention, because shard index is fixed per thread. Different threads may have same shard index.

If constant `DOES_SHARD_INDEX_USE_CPU_INDEX` is false, then it uses fallback case. If true, then it uses CPU index.

### API differences to `Arc`

The API of `Sdarc` is similar to `Arc`. Some differences:

- `Sdarc` doesn't support operations that require ref count to be exactly 1, including [`get_mut`](https://doc.rust-lang.org/std/sync/struct.Arc.html#method.get_mut). Because `Sdarc` ref count operations are lock-free, and there are race conditions when loading counters and compute sum, so there is no cheap way to immediately know whether counter shards sum as 1.
- Doesn't yet support unsized type.

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

### Env vars

- `RUST_SDARC_COLLECTOR_INTERVAL_MS`. By default, collector runs once every 500ms. If one `Sdarc` pointee ref count goes 0, it takes two collector iterations to free memory (if `WeakSdarc` is involved, 3 iterations). If you want faster collection, this can be specified to smaller than 500. But don't make it too small. Making it too small will make collector consume more CPU time. Also, each collector iteration uses heavy barrier which sends interrupt to all cores running current process's thread (the heavy barrier is for synchronization with atomic Sdarc). Calling `collector_update_now_and_wait` can make collector collect early, without needing to setting this env var.
- `RUST_SDARC_SHARD_COUNT`. By default, shard count is available parallelism, round up to power of 2. Setting this env var can control shard count. The actual shard count will be rounded up to power of 2.

### Appendix

#### Comparison with Linux percpu-refcount:

- Linux percpu-refcount has a special owner and has two stages. In the first stage, increment/decrement use the per-CPU ref count slot. When the original owner drops reference, it switches to one atomic counter (the switching is synced by RCU). In `Sdarc` there is no special owner (similar to `Arc`).
- Linux percpu-refcount's second stage frees immediately after count reach zero. There is no background thread scanning.  In `Sdarc`, the freeing is deferred, driven by a background collector thread.
- Linux precpu-refcount uses non-atomic operation to increment/decrement per-CPU ref count. It relies on the fact that kernel code cannot be preempted. The user program can get CPU index via `sched_getcpu` but the thread could be scheduled to another CPU core right after calling it (unless pinned), so the user program has to use atomic operation.
