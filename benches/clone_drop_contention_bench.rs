//! Benchmark comparing multi-threaded clone / drop contention: `Sdarc` vs
//! `std::sync::Arc`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use sdarc::sdarc::Sdarc;

mod common;

use common::{OPS_PER_THREAD, configure_criterion, parallelism, run_threads};

fn bench_clone_drop_contention(c: &mut Criterion) {
    let num_threads = parallelism();

    // ---------- Sdarc ----------
    {
        let shared = Sdarc::new(42i64);

        c.bench_function("clone_drop Sdarc", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let s = shared.clone();
                    let start = Instant::now();
                    run_threads(num_threads, move || {
                        for _ in 0..OPS_PER_THREAD {
                            let c = s.clone();
                            black_box(&c);
                            drop(c);
                        }
                    });
                    total += start.elapsed();
                }
                total
            });
        });
    }

    // ---------- Arc ----------
    {
        let shared = Arc::new(42i64);

        c.bench_function("clone_drop/Arc", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let s = Arc::clone(&shared);
                    let start = Instant::now();
                    run_threads(num_threads, move || {
                        for _ in 0..OPS_PER_THREAD {
                            let c = Arc::clone(&s);
                            black_box(&c);
                            drop(c);
                        }
                    });
                    total += start.elapsed();
                }
                total
            });
        });
    }
}

criterion_group! {
    name = benches;
    config = configure_criterion();
    targets = bench_clone_drop_contention
}
criterion_main!(benches);
