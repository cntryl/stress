# cntryl-stress

[![crates.io](https://img.shields.io/crates/v/cntryl-stress.svg)](https://crates.io/crates/cntryl-stress) [![docs.rs](https://docs.rs/cntryl-stress/badge.svg)](https://docs.rs/cntryl-stress)

A lightweight single-shot benchmark runner for system-level stress tests.

Unlike Criterion (which uses statistical sampling), this crate is designed for expensive operations where each iteration matters: disk I/O, network calls, database transactions, compaction, recovery, etc.

## Features

- **`cargo stress` command** — run benchmarks like `cargo test`
- **`#[stress_test]` attribute** — auto-discovery of benchmark functions
- **Single-shot measurements** — no statistical sampling overhead
- **Explicit timing control** — setup and teardown are not included in measurements
- **Glob filtering** — run subsets with `--workload "pattern*"`
- **Pluggable reporters** — Console, JSON
- **Baseline comparison** — detect regressions against previous runs
- **Throughput tracking** — report bytes/sec or ops/sec

## Quick Start

### 1. Add the dependency

```toml
[dependencies]
cntryl-stress = "0.1"
```

### 2. Create a stress test file

Create `benches/my_test.rs`:

```rust
use cntryl_stress::{stress_test, StressContext};

#[stress_test]
fn write_1mb_file(ctx: &mut StressContext) {
    let data = vec![0u8; 1024 * 1024];
    ctx.set_bytes(data.len() as u64);

    ctx.measure(|| {
        std::fs::write("/tmp/stress_test", &data).unwrap();
    });

    std::fs::remove_file("/tmp/stress_test").ok();
}

#[stress_test(ignore)]
fn slow_network_test(ctx: &mut StressContext) {
    ctx.measure(|| {
        std::thread::sleep(std::time::Duration::from_secs(5));
    });
}
```

### 3. Run your benchmarks

```bash
# Run all stress tests
cargo stress

# Filter by glob pattern
cargo stress --workload "write*"
cargo stress --workload "*hash*"

# Multiple runs (reports median)
cargo stress --runs 5 --warmup 2

# Include ignored tests
cargo stress --include-ignored

# List available benchmarks
cargo stress --list

# Compare against baseline
cargo stress --baseline target/stress/baseline.json --threshold 0.10
```

## Command Line Reference

```
cargo stress [OPTIONS]

Options:
    --workload <PATTERN>    Filter benchmarks by glob pattern
    --runs <N>              Number of measurement runs [default: 1]
    --warmup <N>            Warmup runs (discarded) [default: 0]
    -v, --verbose           Verbose output
    -q, --quiet             Minimal output
    --include-ignored       Run benchmarks marked with #[stress_test(ignore)]
    --baseline <FILE>       Compare against baseline JSON
    --threshold <FLOAT>     Regression threshold (e.g., 0.05 for 5%) [default: 0.05]
    --list                  List benchmarks without running
    -h, --help              Print help
```

## Attribute Options

```rust
// Basic benchmark
#[stress_test]
fn my_bench(ctx: &mut StressContext) { ... }

// Ignored by default (opt-in with --include-ignored)
#[stress_test(ignore)]
fn slow_bench(ctx: &mut StressContext) { ... }

// Custom name (instead of function name)
#[stress_test(name = "custom_name")]
fn internal_name(ctx: &mut StressContext) { ... }
```

## Manual Runner Style

For more control over execution, use `BenchRunner` directly:

```rust
use cntryl_stress::{BenchRunner, StressContext};

let mut runner = BenchRunner::new("my_suite");

runner.run("expensive_operation", |ctx| {
    // Setup (not timed)
    let data = prepare_data();

    // Measure exactly one operation
    ctx.measure(|| {
        expensive_operation(&data);
    });

    // Teardown (not timed)
    cleanup(&data);
});

let results = runner.finish();
```

## Configuration

Configure via environment variables or the builder API:

| Variable           | Default         | Description                                 |
| ------------------ | --------------- | ------------------------------------------- |
| `BENCH_RUNS`       | `1`             | Number of measurement runs (reports median) |
| `BENCH_WARMUP`     | `0`             | Warmup runs (discarded)                     |
| `BENCH_VERBOSE`    | `true`          | Print results to stderr                     |
| `BENCH_OUTPUT_DIR` | `target/stress` | Directory for JSON results                  |
| `BENCH_FILTER`     | -               | Filter benchmarks by name substring         |
| `BENCH_GIT_SHA`    | auto-detected   | Git commit hash to include in results       |

Builder API:

```rust
use cntryl_stress::{BenchRunner, BenchRunnerConfig};

let config = BenchRunnerConfig::new()
    .runs(3)
    .warmup(1)
    .verbose(true)
    .filter("compaction");

let mut runner = BenchRunner::with_config("my_suite", config);
```

## Throughput Reporting

```rust
#[stress_test]
fn write_data(ctx: &mut StressContext) {
    let data = vec![0u8; 1024 * 1024];
    ctx.set_bytes(data.len() as u64);  // Enable bytes/sec reporting

    ctx.measure(|| {
        write_to_disk(&data);
    });
}
// Output: write_data ... 15.32ms (65.28 MB/s)
```

## Baseline Comparison

Detect performance regressions in CI:

```bash
# Save baseline
cargo stress --runs 5
cp target/stress/results.json baseline.json

# Compare against baseline (fails if >5% slower)
cargo stress --runs 5 --baseline baseline.json --threshold 0.05
```

## Project Structure

```
your-project/
├── src/           # Your library code
├── benches/       # Stress tests (just drop .rs files here)
│   └── my_stress.rs
└── Cargo.toml
```

That's it! No `Cargo.toml` changes needed. `cargo stress` auto-discovers files in `benches/` and builds them.

## Installation

Install the `cargo stress` command globally:

```bash
cargo install cntryl-stress
```

Or for development:

```bash
cargo install --path . --bin cargo-stress
```
