#![doc = include_str!("../README.md")]
#![allow(unused_doc_comments)] // use doc comment within func body so that RustRover allows ctrl-click on links in it

pub mod shard_index;
pub mod sharded_alloc;
pub mod sharded_rwlock;
pub mod collector;
pub mod sdarc;
pub mod atomic_sdarc;
pub mod weak_sdarc;

pub(crate) mod tagged_counter;
pub(crate) mod env_params;

pub use sdarc::Sdarc;
pub use weak_sdarc::WeakSdarc;
pub use atomic_sdarc::{AtomicNullableSdarc, AtomicSdarc, AtomicSdarcBorrowGuard};
pub use collector::collector_update_now_and_wait;
pub use shard_index::{
    ShardCount, ShardIndex, curr_shard_index, get_shard_count
};
pub use sharded_alloc::ShardedBox;
pub use sharded_rwlock::{ShardedRwLock};

#[cfg(test)]
mod tests;
