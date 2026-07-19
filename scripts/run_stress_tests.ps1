<#
.SYNOPSIS
  Run the Sdarc integrated stress test under 6 different configurations.

.DESCRIPTION
  Each config runs as a separate `cargo test` process so the per-process
  environment variables (RUST_SDARC_SHARD_COUNT, RUST_SDARC_COLLECTOR_INTERVAL_MS)
  take effect in a freshly-initialized crate.

  The configs cover the same matrix the old rusty_fork_test! block used:
    (1,   0)   — 1 shard,  fastest collector (tightest race window)
    (8,   0)   — 8 shards, fastest collector
    (128, 0)   — many shards, fastest collector
    (1,   200) — 1 shard,  normal collector
    (16,  200) — medium,   normal collector
    (256, 500) — max shards, slow collector

.PARAMETER ExtraArgs
  Additional arguments forwarded to `cargo test` (e.g. "--release").

.EXAMPLE
  .\scripts\run_stress_tests.ps1
  .\scripts\run_stress_tests.ps1 --release
#>

param(
    [string[]]$ExtraArgs = @()
)

$configs = @(
    @{ Shard = 1;   Interval = 0   },
    @{ Shard = 8;   Interval = 0   },
    @{ Shard = 128; Interval = 0   },
    @{ Shard = 1;   Interval = 200 },
    @{ Shard = 16;  Interval = 200 },
    @{ Shard = 256; Interval = 500 }
)

$failed = $false

foreach ($cfg in $configs) {
    Write-Host ""
    Write-Host "=================================================="
    Write-Host "Running stress test: SHARD_COUNT=$($cfg.Shard), INTERVAL_MS=$($cfg.Interval)"
    Write-Host "=================================================="

    $env:RUST_SDARC_SHARD_COUNT = [string]$cfg.Shard
    $env:RUST_SDARC_COLLECTOR_INTERVAL_MS = [string]$cfg.Interval

    if ($cfg.Interval -eq 0) {
        $env:RUST_SDARC_TEST_DISABLE_SHARDED_ALLOC_MAINTENANCE = "1"
    } else {
        Remove-Item Env:\RUST_SDARC_TEST_DISABLE_SHARDED_ALLOC_MAINTENANCE -ErrorAction SilentlyContinue
    }

    & cargo test integrated_stress @ExtraArgs -- --nocapture --test-threads=1

    if ($LASTEXITCODE -ne 0) {
        Write-Host ""
        Write-Host "Test FAILED for SHARD_COUNT=$($cfg.Shard), INTERVAL_MS=$($cfg.Interval)" -ForegroundColor Red
        $failed = $true
        break
    }
}

if (-not $failed) {
    Write-Host ""
    Write-Host "=================================================="
    Write-Host "All stress test configurations passed."
    Write-Host "=================================================="
} else {
    exit 1
}
