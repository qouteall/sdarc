#!/usr/bin/env bash
set -euo pipefail

# ---------------------------------------------------------------------------
# Run the Sdarc integrated stress test under 6 different configurations.
#
# Each config runs as a separate `cargo test` process so the per-process
# environment variables (RUST_SDARC_SHARD_COUNT, RUST_SDARC_COLLECTOR_INTERVAL_MS)
# take effect in a freshly-initialized crate.
#
# Usage:
#   ./scripts/run_stress_tests.sh [cargo-test-extra-args...]
#   ./scripts/run_stress_tests.sh --release
#
# The configs cover the same matrix the old rusty_fork_test! block used:
#   (1,   0)   — 1 shard,  fastest collector (tightest race window)
#   (8,   0)   — 8 shards, fastest collector
#   (128, 0)   — many shards, fastest collector
#   (1,   200) — 1 shard,  normal collector
#   (16,  200) — medium,   normal collector
#   (256, 500) — max shards, slow collector
# ---------------------------------------------------------------------------

CONFIGS=(
  "1   0"
  "8   0"
  "128 0"
  "1   200"
  "16  200"
  "256 500"
)

EXTRA_ARGS="${@}"

for config in "${CONFIGS[@]}"; do
  read -r SHARD_COUNT INTERVAL_MS <<< "$config"
  echo ""
  echo "=================================================="
  echo "Running stress test: SHARD_COUNT=${SHARD_COUNT}, INTERVAL_MS=${INTERVAL_MS}"
  echo "=================================================="

  if [[ "${INTERVAL_MS}" == "0" ]]; then
    RUST_SDARC_SHARD_COUNT="${SHARD_COUNT}" \
    RUST_SDARC_COLLECTOR_INTERVAL_MS="${INTERVAL_MS}" \
    RUST_SDARC_TEST_DISABLE_SHARDED_ALLOC_MAINTENANCE=1 \
      cargo test integrated_stress ${EXTRA_ARGS} -- --nocapture --test-threads=1
  else
    RUST_SDARC_SHARD_COUNT="${SHARD_COUNT}" \
    RUST_SDARC_COLLECTOR_INTERVAL_MS="${INTERVAL_MS}" \
      cargo test integrated_stress ${EXTRA_ARGS} -- --nocapture --test-threads=1
  fi
done

echo ""
echo "=================================================="
echo "All stress test configurations passed."
echo "=================================================="
