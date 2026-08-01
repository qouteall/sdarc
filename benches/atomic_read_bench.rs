//! Benchmark comparing atomic read throughput: `AtomicSdarc` vs
//! `arc_swap::ArcSwap`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use sdarc::atomic_sdarc::AtomicSdarc;
use sdarc::sdarc::Sdarc;
use sdarc::shard_index::{get_shard_count, DOES_SHARD_INDEX_USE_CPU_INDEX};

mod common;

use common::{OPS_PER_THREAD, configure_criterion, parallelism, run_threads};

fn bench_atomic_read_throughput(c: &mut Criterion) {
    println!("Shard count {:?}. Uses CPU Index {}", get_shard_count(), DOES_SHARD_INDEX_USE_CPU_INDEX);

    println!("asymmetric fence supported {}", membarrier2::is_supported());

    let num_readers = parallelism();
    let payload = vec![0i64; 64];

    {
        let shared = Arc::new(AtomicSdarc::new(payload.clone()));
        let stop = Arc::new(AtomicBool::new(false));

        // background writer — swaps a new Vec every 200 ms
        let w_shared = Arc::clone(&shared);
        let w_stop = Arc::clone(&stop);
        let writer = thread::spawn(move || {
            let mut generation = 0u64;
            while !w_stop.load(Ordering::Relaxed) {
                generation += 1;
                w_shared.swap(Sdarc::new(vec![generation as i64; 64]));
                thread::sleep(Duration::from_millis(200));
            }
        });

        c.bench_function("AtomicSdarc load_owned", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let s = Arc::clone(&shared);
                    let start = Instant::now();
                    run_threads(num_readers, move || {
                        for _ in 0..OPS_PER_THREAD {
                            let val = s.load_owned();
                            black_box(val[0]);
                        }
                    });
                    total += start.elapsed();
                }
                total
            });
        });

        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
    }

    {
        let shared = Arc::new(AtomicSdarc::new(payload.clone()));
        let stop = Arc::new(AtomicBool::new(false));

        // background writer — swaps a new Vec every 200 ms
        let w_shared = Arc::clone(&shared);
        let w_stop = Arc::clone(&stop);
        let writer = thread::spawn(move || {
            let mut generation = 0u64;
            while !w_stop.load(Ordering::Relaxed) {
                generation += 1;
                w_shared.swap(Sdarc::new(vec![generation as i64; 64]));
                thread::sleep(Duration::from_millis(200));
            }
        });

        c.bench_function("AtomicSdarc borrow", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let s = Arc::clone(&shared);
                    let start = Instant::now();
                    run_threads(num_readers, move || {
                        for _ in 0..OPS_PER_THREAD {
                            let val = s.borrow();
                            black_box(val[0]);
                        }
                    });
                    total += start.elapsed();
                }
                total
            });
        });

        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
    }

    // ---------- ArcSwap ----------
    {
        let shared = Arc::new(ArcSwap::from_pointee(payload));
        let stop = Arc::new(AtomicBool::new(false));

        let w_shared = Arc::clone(&shared);
        let w_stop = Arc::clone(&stop);
        let writer = thread::spawn(move || {
            let mut generation = 0u64;
            while !w_stop.load(Ordering::Relaxed) {
                generation += 1;
                w_shared.store(Arc::new(vec![generation as i64; 64]));
                thread::sleep(Duration::from_millis(200));
            }
        });

        c.bench_function("ArcSwap load", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let s = Arc::clone(&shared);
                    let start = Instant::now();
                    run_threads(num_readers, move || {
                        for _ in 0..OPS_PER_THREAD {
                            let val = s.load();
                            black_box(val[0]);
                        }
                    });
                    total += start.elapsed();
                }
                total
            });
        });

        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
    }

    // In a X86 machine the Sdarc borrow is roughtly as fast as ArcSwap
    // But in an ARM VM server the Sdarc borrow is faster than ArcSwap
    // The Sdarc hazard pointer uses asymmetric fence to replace SeqCst while ArcSwap uses SeqCst
    // The result in the ARM server
    //
    // AtomicSdarc borrow      time:   [1.7028 ms 1.7064 ms 1.7099 ms]
    // Found 1 outliers among 50 measurements (2.00%)
    //   1 (2.00%) high mild
    //
    // ArcSwap load            time:   [2.5046 ms 2.5126 ms 2.5216 ms]
    // Found 4 outliers among 50 measurements (8.00%)
    //   2 (4.00%) low mild
    //   1 (2.00%) high mild
    //   1 (2.00%) high severe
}

criterion_group! {
    name = benches;
    config = configure_criterion();
    targets = bench_atomic_read_throughput
}
criterion_main!(benches);

// cargo flamegraph --bench atomic_read_bench -- --bench "AtomicSdarc borrow"
