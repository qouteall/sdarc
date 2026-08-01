//! Benchmark comparing single-threaded clone / drop: `Sdarc` vs
//! `std::sync::Arc` (no contention).

use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use sdarc::sdarc::Sdarc;

mod common;

use common::{OPS_PER_THREAD, configure_criterion};

fn bench_clone_drop_single_thread(c: &mut Criterion) {
    // ---------- Sdarc ----------
    {
        let shared = Sdarc::new(42i64);

        c.bench_function("clone_drop_single/Sdarc", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let s = shared.clone();
                    let start = Instant::now();
                    for _ in 0..OPS_PER_THREAD {
                        let c = s.clone();
                        black_box(&c);
                        drop(c);
                    }
                    total += start.elapsed();
                }
                total
            });
        });
    }

    // ---------- Arc ----------
    {
        let shared = Arc::new(42i64);

        c.bench_function("clone_drop_single/Arc", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let s = Arc::clone(&shared);
                    let start = Instant::now();
                    for _ in 0..OPS_PER_THREAD {
                        let c = Arc::clone(&s);
                        black_box(&c);
                        drop(c);
                    }
                    total += start.elapsed();
                }
                total
            });
        });
    }

    // In the X86 machine the single-threaded Sdarc clone/drop is only 15% slower than Arc
    // But in an ARM VM server Sdarc is much slower than Arc
    // cargo flamegraph shows el0t_64_sync in sched_getcpu so it's making syscall.
    // TODO find a better way to get cpu index
    //
    // clone_drop_single/Sdarc time:   [12.264 ms 12.269 ms 12.277 ms]
    // Found 12 outliers among 50 measurements (24.00%)
    //   12 (24.00%) high severe
    //
    // clone_drop_single/Arc   time:   [571.85 µs 571.92 µs 571.98 µs]
    // Found 1 outliers among 50 measurements (2.00%)
    //   1 (2.00%) low mild
}

criterion_group! {
    name = benches;
    config = configure_criterion();
    targets = bench_clone_drop_single_thread
}
criterion_main!(benches);
