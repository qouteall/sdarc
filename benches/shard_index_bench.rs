//! Benchmark for `curr_shard_index`, which gets the current CPU index
//! (`sched_getcpu` on Linux, `GetCurrentProcessorNumber` on Windows,
//! thread-id hash elsewhere) and maps it to a shard index.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use sdarc::curr_shard_index;

fn bench_curr_shard_index(c: &mut Criterion) {
    c.bench_function("curr_shard_index", |b| {
        b.iter(|| black_box(curr_shard_index()))
    });
}

criterion_group!(benches, bench_curr_shard_index);
criterion_main!(benches);
