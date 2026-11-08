//! Probe for a public-API reentrancy hazard:
//! `ShardedBox::allocate_data_in_each_shard` invokes the init callback while
//! holding the current shard's allocation mutex, so an init callback that
//! allocates another `ShardedBox` (or an `Sdarc`, whose counters use one) on
//! the same shard deadlocks.
//!
//! Run with one shard so the same mutex is certainly reused:
//! `RUST_SDARC_SHARD_COUNT=1 cargo test --test reentrancy -- --ignored`
//!
//! This test spawns the work on a separate thread and uses a timeout so that a
//! deadlock is reported as a failure instead of hanging the harness.

use std::sync::mpsc;
use std::time::Duration;

use crate::Sdarc;
use crate::sharded_alloc::ShardedBox;

const TIMEOUT: Duration = Duration::from_secs(5);

#[test]
#[ignore = "demonstrates a deadlock; run explicitly"]
fn nested_sharded_box_allocation_deadlocks() {
    let (tx, rx) = mpsc::channel::<()>();

    std::thread::spawn(move || {
        let outer = ShardedBox::<u64>::allocate_data_in_each_shard(|_| {
            let _inner = ShardedBox::<u64>::allocate_data_in_each_shard(|_| 0u64);
            1u64
        });
        let _ = tx.send(());
        drop(outer);
    });

    assert!(
        rx.recv_timeout(TIMEOUT).is_ok(),
        "nested ShardedBox allocation deadlocked on the shard mutex"
    );
}

#[test]
#[ignore = "demonstrates a deadlock; run explicitly"]
fn sdarc_new_inside_sharded_box_init_deadlocks() {
    let (tx, rx) = mpsc::channel::<()>();

    std::thread::spawn(move || {
        let outer = ShardedBox::<u64>::allocate_data_in_each_shard(|_| {
            // Allocating a `Sdarc` inside the callback also allocates a
            // `ShardedBox` (for the counters) on the current shard.
            let inner = Sdarc::new(7u64);
            let _ = *inner;
            1u64
        });
        let _ = tx.send(());
        drop(outer);
    });

    assert!(
        rx.recv_timeout(TIMEOUT).is_ok(),
        "allocating an Sdarc inside a ShardedBox init callback deadlocked"
    );
}
