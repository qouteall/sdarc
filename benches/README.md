
Note: the benchmarks are simple tight loops, may be different to performance in real-world workloads.

---

On Intel(R) Xeon(R) 6982P-C, VM in aliyun, 32 cores, 64 threads, Ubuntu 26.04 LTS, rustc 1.97.1 (8bab26f4f 2026-07-14), x86_64-unknown-linux-gnu

```
AtomicSdarc load_owned  time:   [2.1295 ms 2.1370 ms 2.1439 ms]
AtomicSdarc borrow      time:   [1.2476 ms 1.2491 ms 1.2504 ms]
ArcSwap load            time:   [2.0191 ms 2.0239 ms 2.0289 ms]
clone_drop_multi_thread Sdarc
                        time:   [2.1496 ms 2.1557 ms 2.1618 ms]
clone_drop_multi_thread Arc
                        time:   [79.395 ms 79.472 ms 79.547 ms]
clone_drop_single_thread Sdarc
                        time:   [783.90 µs 783.99 µs 784.06 µs]
clone_drop_single_thread Arc
                        time:   [601.54 µs 601.56 µs 601.58 µs]
curr_shard_index        time:   [2.2380 ns 2.2382 ns 2.2384 ns]
```

---

On Neoverse-N2, VM in aliyun, 32 cores, 32 threads, Ubuntu 26.04 LTS, rustc 1.97.1 (8bab26f4f 2026-07-14), aarch64-unknown-linux-gnu

```
AtomicSdarc load_owned  time:   [2.7601 ms 2.7688 ms 2.7777 ms]
AtomicSdarc borrow      time:   [1.7147 ms 1.7178 ms 1.7210 ms]
ArcSwap load            time:   [2.3913 ms 2.4008 ms 2.4099 ms]
clone_drop_multi_thread Sdarc
                        time:   [2.5341 ms 2.5399 ms 2.5455 ms]
clone_drop_multi_thread Arc
                        time:   [37.164 ms 37.181 ms 37.198 ms]
clone_drop_single_thread Sdarc
                        time:   [927.51 µs 927.58 µs 927.65 µs]
clone_drop_single_thread Arc
                        time:   [571.72 µs 571.76 µs 571.81 µs]
curr_shard_index        time:   [4.1541 ns 4.1555 ns 4.1567 ns]
```

---

Same as the above, but uses musl instead of glibc, aarch64-unknown-linux-musl

(in musl aarch64, `sched_getcpu` uses syscall, so shard index is based on thread id hash. there will be more contention)

```
AtomicSdarc load_owned  time:   [9.8068 ms 9.8782 ms 9.9552 ms]
AtomicSdarc borrow      time:   [1.9748 ms 1.9784 ms 1.9818 ms]
ArcSwap load            time:   [2.6258 ms 2.6330 ms 2.6402 ms]
clone_drop_multi_thread Sdarc
                        time:   [8.6571 ms 8.7253 ms 8.8006 ms]
clone_drop_multi_thread Arc
                        time:   [35.177 ms 35.281 ms 35.395 ms]
clone_drop_single_thread Sdarc
                        time:   [781.66 µs 781.84 µs 782.00 µs]
clone_drop_single_thread Arc
                        time:   [573.80 µs 573.81 µs 573.82 µs]
curr_shard_index        time:   [1.7994 ns 1.7995 ns 1.7995 ns]
```

---

On AMD Ryzen 9 9900X, 12 cores, 24 threads, Windows 11 rustc 1.97.1 (8bab26f4f 2026-07-14), x86_64-pc-windows-msvc:

```
AtomicSdarc load_owned  time:   [1.5430 ms 1.5834 ms 1.6305 ms]
AtomicSdarc borrow      time:   [1.1899 ms 1.1930 ms 1.1974 ms]
ArcSwap load            time:   [1.3696 ms 1.3721 ms 1.3752 ms]
clone_drop_multi_thread Sdarc
                        time:   [1.4424 ms 1.4453 ms 1.4483 ms]
clone_drop_multi_thread Arc
                        time:   [22.894 ms 22.904 ms 22.914 ms]
clone_drop_single_thread Sdarc
                        time:   [425.29 µs 426.82 µs 428.32 µs]
clone_drop_single_thread Arc
                        time:   [369.16 µs 370.74 µs 372.31 µs]
curr_shard_index        time:   [1.5292 ns 1.5349 ns 1.5405 ns]
```