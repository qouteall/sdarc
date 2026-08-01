
On AMD Ryzen 9 9900X (12 cores / 24 threads) Windows 11 rustc 1.97.1 (8bab26f4f 2026-07-14), x86_64-pc-windows-msvc:

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
```