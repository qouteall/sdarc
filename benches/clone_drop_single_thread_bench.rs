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

        c.bench_function("clone_drop_single_thread Sdarc", |b| {
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

        c.bench_function("clone_drop_single_thread Arc", |b| {
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
}

criterion_group! {
    name = benches;
    config = configure_criterion();
    targets = bench_clone_drop_single_thread
}
criterion_main!(benches);
