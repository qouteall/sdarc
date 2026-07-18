use crate::shard_index::{MAX_SHARD_COUNT, ShardCount};
use std::env;
use std::time::Duration;

static ENV_COLLECTOR_INTERVAL_MS: &str = "RUST_SDARC_COLLECTOR_INTERVAL_MS";
static ENV_SHARD_COUNT: &str = "RUST_SDARC_SHARD_COUNT";

#[derive(Debug, Clone)]
pub(crate) struct CollectorParams {
    pub interval: Duration,
}

impl CollectorParams {
    const DEFAULT_COLLECTOR_INTERVAL: Duration = Duration::from_millis(200);

    pub fn new_from_env_var() -> Self {
        let interval: Duration = if let Ok(s) = env::var(ENV_COLLECTOR_INTERVAL_MS) {
            match s.parse::<u64>() {
                Ok(num) => Duration::from_millis(num),
                Err(err) => {
                    panic!(
                        "Env var {ENV_COLLECTOR_INTERVAL_MS} cannot be parsed as unsigned integer. {s} {err:?}"
                    );
                }
            }
        } else {
            Self::DEFAULT_COLLECTOR_INTERVAL
        };

        Self { interval }
    }
}

pub(crate) fn shard_count_from_env_var() -> Option<ShardCount> {
    if let Ok(s) = env::var(ENV_SHARD_COUNT) {
        match s.parse::<usize>() {
            Ok(num) => {
                if num > MAX_SHARD_COUNT {
                    panic!(
                        "Shard count specified by env var {ENV_SHARD_COUNT} is too large: {num}. The max is {MAX_SHARD_COUNT}"
                    );
                } else {
                    Some(ShardCount(num as u16))
                }
            }
            Err(err) => {
                panic!(
                    "Env var {ENV_SHARD_COUNT} cannot be parsed as unsigned integer. {s} {err:?}"
                );
            }
        }
    } else {
        None
    }
}
