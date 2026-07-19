//! Benchmarks comparing `Sdarc` / `AtomicSdarc` against `std::sync::Arc` /
//! `arc_swap::ArcSwap`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use sdarc::atomic_sdarc::AtomicSdarc;
use sdarc::sdarc::{ Sdarc};
use sdarc::shard_index::{get_shard_count};
// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parallelism() -> usize {
    thread::available_parallelism().unwrap().get()
}

const OPS_PER_THREAD: u64 = 50_000;

fn run_threads(num: usize, f: impl Fn() + Send + Sync + Clone + 'static) {
    let barrier = Arc::new(Barrier::new(num));
    let handles: Vec<_> = (0..num)
        .map(|_| {
            let b = Arc::clone(&barrier);
            let f = f.clone();
            thread::spawn(move || {
                b.wait();
                f()
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}

// ===========================================================================
// Bench 1 — Atomic read throughput: AtomicSdarc vs ArcSwap
// ===========================================================================

fn bench_atomic_read_throughput(c: &mut Criterion) {
    println!("Shard count {:?}", get_shard_count());

    let num_readers = parallelism();
    let payload = vec![0i64; 64];

    // ---------- AtomicSdarc ----------
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

        c.bench_function("atomic_read/AtomicSdarc", |b| {
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

        c.bench_function("atomic_read/ArcSwap", |b| {
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
}

// ===========================================================================
// Bench 2 — Clone / drop contention: Sdarc vs Arc
// ===========================================================================

fn bench_clone_drop_contention(c: &mut Criterion) {
    let num_threads = parallelism();

    // ---------- Sdarc ----------
    {
        let shared = Sdarc::new(42i64);

        c.bench_function("clone_drop/Sdarc", |b| {
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

// ===========================================================================
// Bench 3 — Single-threaded clone / drop: Sdarc vs Arc (no contention)
// ===========================================================================

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
}

fn configure_criterion() -> Criterion {
    Criterion::default()
        .measurement_time(Duration::from_secs(30))
        .sample_size(50)
}

criterion_group! {
    name = benches;
    config = configure_criterion();
    targets = bench_atomic_read_throughput, bench_clone_drop_contention, bench_clone_drop_single_thread
}
criterion_main!(benches);
