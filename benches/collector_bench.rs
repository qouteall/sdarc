use criterion::{Criterion, black_box, criterion_group, criterion_main};
use sdarc::collector::collector_update_now_and_wait;
use sdarc::sdarc::Sdarc;
use std::time::Instant;

fn bench_collector_cleanup(c: &mut Criterion) {
    for &count in &[10_000, 100_000, 1_000_000] {
        let name = format!("collector_cleanup_{}", count);
        c.bench_function(&name, |b| {
            b.iter_custom(|iters| {
                let mut total = std::time::Duration::ZERO;
                for _ in 0..iters {
                    // setup
                    let v: Vec<Sdarc<i32>> = (0..count).map(|i| Sdarc::new(i)).collect();
                    collector_update_now_and_wait();
                    black_box(v); // drop
                    // measure cleanup
                    let start = Instant::now();
                    collector_update_now_and_wait();
                    total += start.elapsed();
                }
                total
            });
        });
    }
}

criterion_group!(benches, bench_collector_cleanup);
criterion_main!(benches);
