# cntryl-stress

[![crates.io](https://img.shields.io/crates/v/cntryl-stress.svg)](https://crates.io/crates/cntryl-stress) [![docs.rs](https://docs.rs/cntryl-stress/badge.svg)](https://docs.rs/cntryl-stress)

A lightweight single-shot benchmark runner for system-level stress tests.

Unlike Criterion (which uses statistical sampling), this crate is designed for expensive operations where each iteration matters: disk I/O, network calls, database transactions, compaction, recovery, etc.

---

## Features

- **Single-shot measurements** — no statistical sampling overhead
- **Explicit timing control** — setup and teardown are not included in measurements
- **Pluggable reporters** — Console, JSON, GitHub Actions annotations
- **Baseline comparison** — detect regressions against previous runs
- **Throughput tracking** — report bytes/sec or ops/sec
- **HDR histograms** — optional latency percentiles (`hdr` feature)
- **Async support** — optional async benchmarks (`async` feature)

---

## Quick Start

```rust
use cntryl_stress::{BenchRunner, BenchContext};

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

---

## Configuration

Configure behavior via environment variables (or use the builder API):

| Variable | Default | Description |
|----------|---------|-------------|
| `BENCH_RUNS` | `1` | Number of measurement runs (reports median) |
| `BENCH_WARMUP` | `0` | Warmup runs (discarded) |
| `BENCH_VERBOSE` | `true` | Print results to stderr |
| `BENCH_OUTPUT_DIR` | `target/stress` | Directory where JSON results are written |
| `BENCH_FILTER` | - | Filter benchmarks by name substring |
| `BENCH_GIT_SHA` | auto-detected | Git commit hash to include in results |

Builder API example:

```rust
use cntryl_stress::{BenchRunner, BenchRunnerConfig};

let config = BenchRunnerConfig::new()
    .runs(3)
    .warmup(1)
    .verbose(true)
    .filter("compaction");

let mut runner = BenchRunner::with_config("my_suite", config);
```

---

## Throughput Reporting

```rust
runner.run("write_data", |ctx| {
    let data = vec![0u8; 1024 * 1024];
    ctx.set_bytes(data.len() as u64);

    ctx.measure(|| {
        write_to_disk(&data);
    });
});
// Example output: write_data ... 15.32ms (65.28 MB/s)
```

---

## Baseline Comparison

Compare results against a saved baseline JSON file and detect regressions beyond a threshold.

```rust
let (results, regressions) = runner.finish_with_baseline(
    "baseline.json",
    0.05,  // 5% threshold
);

if !regressions.is_empty() {
    for (bench, ratio) in &regressions {
        eprintln!("{} regressed by {:.1}%", bench.name, (ratio - 1.0) * 100.0);
    }
    std::process::exit(1);
}
```

---

## Notes & Tips

- Each benchmark closure **must call** `ctx.measure()` exactly once — the runner enforces this and will panic in tests if omitted. Document your setup/teardown so only the measured operation is timed.
- If a baseline file cannot be loaded, the runner currently skips regression checks (a warning is emitted when JSON writing fails).
- Consider using `BENCH_OUTPUT_DIR` to persist JSON results for CI or historical comparison.

---

## Contributing

Contributions welcome! Please open issues/PRs. Running `cargo clippy` and `cargo test` before submitting a PR helps keep the project tidy.

---

## Using `cargo stress`

This repository includes a small `cargo` subcommand binary named `cargo-stress` which enables running registered benchmarks like a normal Cargo subcommand.

Local development (no install):

```text
cargo run --bin cargo-stress -- run --suite my_suite --runs 3
```

Install globally (so you can run `cargo stress` anywhere):

```text
cargo install --path . --bin cargo-stress
# then
cargo stress run --suite my_suite --runs 3
```

The default registration file is `src/benches/mod.rs`. Update `register_benchmarks()` to add project-specific benchmarks.

---

## Changelog (selected)

- 2025-12-23 — README tuned for clarity; fixed a throughput formatting edge-case and applied minor lints and tests.

---

## License

MIT OR Apache-2.0
