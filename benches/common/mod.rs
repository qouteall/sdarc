//! Helpers shared by the criterion benchmarks.
//!
//! This lives in a subdirectory so that Cargo does not treat it as a bench target.

#![allow(dead_code)]

use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use criterion::Criterion;

pub fn parallelism() -> usize {
    thread::available_parallelism().unwrap().get()
}

pub const OPS_PER_THREAD: u64 = 50_000;

pub fn run_threads(num: usize, f: impl Fn() + Send + Sync + Clone + 'static) {
    let barrier = Arc::new(Barrier::new(num));
    let handles: Vec<_> = (0..num)
        .map(|_i| {
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

pub fn configure_criterion() -> Criterion {
    Criterion::default()
        .measurement_time(Duration::from_secs(30))
        .sample_size(50)
}
