# cntryl-stress

[![crates.io](https://img.shields.io/crates/v/cntryl-stress.svg)](https://crates.io/crates/cntryl-stress)
[![docs.rs](https://docs.rs/cntryl-stress/badge.svg)](https://docs.rs/cntryl-stress)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)

A lightweight single-shot benchmark runner for system-level stress tests.

Unlike Criterion (which uses statistical sampling), this crate is designed for expensive operations where each iteration matters: disk I/O, network calls, database transactions, compaction, recovery, etc.

## Features

- **Single-shot measurements** — no statistical sampling overhead
- **Explicit timing control** — setup and teardown are not included in measurements
- **Throughput tracking** — report bytes/sec or ops/sec
- **Glob filtering** — run subsets with `--workload "pattern*"`
- **JSON + Text output** — machine-readable and human-readable results
- **Baseline comparison** — detect regressions against previous runs
- **Auto-discovery** — `#[stress_test]` attribute with `stress_main!()`
- **Manual runner** — full control with `BenchRunner` API

## Quick Start

### 1. Add dependency

Add to `Cargo.toml`:

```toml
[dependencies]
cntryl-stress = "0.1"

[[bench]]
name = "my_stress_test"
path = "benches/my_stress_test.rs"
harness = false
```

### 2. Create benchmark

Create `benches/my_stress_test.rs`:

```rust
use cntryl_stress::{stress_test, stress_main, StressContext};
use std::hint::black_box;

#[stress_test]
fn write_large_file(ctx: &mut StressContext) {
    let data = vec![0u8; 1024 * 1024];
    ctx.set_bytes(data.len() as u64);

    ctx.measure(|| {
        std::fs::write("/tmp/stress_test.bin", &data).unwrap();
    });

    std::fs::remove_file("/tmp/stress_test.bin").ok();
}

#[stress_test]
fn allocate_buffer(ctx: &mut StressContext) {
    ctx.measure(|| {
        let _buf = black_box(vec![0u32; 1_000_000]);
    });
}

#[stress_test(ignore)]
fn slow_test(ctx: &mut StressContext) {
    ctx.measure(|| {
        std::thread::sleep(std::time::Duration::from_secs(10));
    });
}

stress_main!();
```

### 3. Run with `cargo bench`

```bash
# Run all benchmarks in the bench
cargo bench --bench my_stress_test

# Filter by environment variable
BENCH_FILTER=write cargo bench --bench my_stress_test

# Multiple runs with warmup (reports median)
BENCH_RUNS=5 BENCH_WARMUP=1 cargo bench --bench my_stress_test

# Include ignored tests
BENCH_INCLUDE_IGNORED=1 cargo bench --bench my_stress_test
```

## Output

Each run creates timestamped files in `target/stress/{suite}/{timestamp}.{json,txt}`:

**Console Output (always shown):**
```
Benchmark Suite: my_stress_test
Runs: 5, Warmup: 1
---------------------------------------------------------------

  write_large_file                       15.32ms  (65.28 MB/s)
  allocate_buffer                        45.18ms
---------------------------------------------------------------
Completed 2 benchmarks in 60.50ms
---------------------------------------------------------------

  Results written to: target/stress/my_stress_test/1771376841729.json
  Latest results at: target/stress/my_stress_test/latest.json
```

**JSON Output** (`target/stress/my_stress_test/latest.json`):
```json
{
  "suite": "my_stress_test",
  "results": [
    {
      "name": "my_stress_test/write_large_file",
      "duration_ns": 15320000,
      "bytes": 1048576,
      "elements": null,
      "tags": {}
    }
  ],
  "total_duration_ns": 60500000,
  "started_at": "1771376841729",
  "runs": 5,
  "warmup_runs": 1,
  "git_sha": "36d2a432664734a8499a37e95686a6d378902b6a"
}
```

**Text Summary** (`target/stress/my_stress_test/latest.txt`):
```
===============================================================
Benchmark Suite: my_stress_test
===============================================================

Completed: 1771376841729
Git SHA:   36d2a432664734a8499a37e95686a6d378902b6a

Results:
---------------------------------------------------------------
  write_large_file                       15.32ms  (65.28 MB/s)
  allocate_buffer                        45.18ms
---------------------------------------------------------------
Total time: 60.50ms
Benchmarks: 2
===============================================================
```

## Configuration

All configuration is through environment variables:

| Variable           | Default         | Description                                 |
|-------------------|-----------------|---------------------------------------------|
| `BENCH_RUNS`      | `1`             | Number of measurement runs (reports median) |
| `BENCH_WARMUP`    | `0`             | Warmup runs (discarded)                     |
| `BENCH_VERBOSE`   | `true`          | Print results to console (always enabled)   |
| `BENCH_OUTPUT_DIR`| `target/stress` | Directory for JSON/TXT output               |
| `BENCH_FILTER`    | -               | Filter benchmarks by name (substring match) |
| `BENCH_GIT_SHA`   | auto-detected   | Git commit hash to include in results       |
| `BENCH_INCLUDE_IGNORED` | `false`   | Include benchmarks marked with `#[stress_test(ignore)]` |

Example:

```bash
# Run 5 times with 1 warmup, filter to "write" benchmarks
BENCH_RUNS=5 BENCH_WARMUP=1 BENCH_FILTER=write cargo bench --bench my_stress_test

# Custom output directory
BENCH_OUTPUT_DIR=./results cargo bench --bench my_stress_test
```

### Programmatic Configuration

Use `BenchRunnerConfig` for fine-grained control:

```rust
use cntryl_stress::{BenchRunner, BenchRunnerConfig};

let config = BenchRunnerConfig::new()
    .runs(5)
    .warmup(2)
    .verbose(true)
    .filter("write")
    .output_dir("target/custom");

let mut runner = BenchRunner::with_config("my_suite", config);

runner.run("benchmark_a", |ctx| {
    ctx.measure(|| {
        // Your code here
    });
});

runner.finish();
```

## Throughput Reporting

Report throughput by setting bytes or elements:

```rust
#[stress_test]
fn compress_data(ctx: &mut StressContext) {
    let data = vec![0u8; 1024 * 1024 * 10];  // 10 MB
    ctx.set_bytes(data.len() as u64);

    ctx.measure(|| {
        let _compressed = compress(&data);
    });
}
// Output: compress_data ... 1.23s (8.13 MB/s)
```

For element-based throughput:

```rust
#[stress_test]
fn hash_items(ctx: &mut StressContext) {
    let items = vec![1; 1_000_000];
    ctx.set_elements(items.len() as u64);

    ctx.measure(|| {
        let _hashed = items.iter().map(|x| hash(x)).collect::<Vec<_>>();
    });
}
// Output: hash_items ... 5.42ms (184.50M ops/s)
```

## Coexistence with Other Benchmarking Tools

cntryl-stress works great alongside **Criterion** or other benchmark frameworks in the same project.

### Example: Criterion + cntryl-stress

`Cargo.toml`:
```toml
[dev-dependencies]
cntryl-stress = "0.1"
criterion = "0.5"

[[bench]]
name = "statistical_benches"
harness = true  # Use criterion's harness

[[bench]]
name = "stress_benches"
path = "benches/stress_benches.rs"
harness = false  # Use cntryl-stress harness
```

`benches/criterion_benches.rs` (uses criterion):
```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn my_criterion_bench(c: &mut Criterion) {
    c.bench_function("fast_operation", |b| {
        b.iter(|| black_box(1 + 1))
    });
}

criterion_group!(benches, my_criterion_bench);
criterion_main!(benches);
```

`benches/stress_benches.rs` (uses cntryl-stress):
```rust
use cntryl_stress::{stress_test, stress_main, StressContext};

#[stress_test]
fn expensive_operation(ctx: &mut StressContext) {
    ctx.measure(|| {
        // Expensive I/O, network, etc.
    });
}

stress_main!();
```

Run both:
```bash
cargo bench
# Runs both stress_benches and criterion_benches independently
```

### Why Use Both?

- **Criterion**: Statistical analysis, micro-benchmarks, detecting small regressions
- **cntryl-stress**: System-level benchmarks, single expensive operations, throughput tracking
- **Together**: Complete picture of performance across different scales

### Key Differences

| Aspect | criterion | cntryl-stress |
|--------|-----------|---------------|
| **Best for** | Micro-benchmarks | Expensive operations |
| **Sampling** | Statistical (many runs) | Single-shot |
| **Output** | HTML reports | JSON + text |
| **Overhead** | Low (simple operations) | Minimal (setup excluded) |
| **I/O operations** | Not ideal | Perfect fit |
| **Network calls** | Not ideal | Perfect fit |

This workspace publishes two crates to crates.io using OIDC trusted publishing.

### Setup (One-time)

1. Go to https://crates.io/me
2. Click "Create a new token" → "Create token with OIDC"
3. Set `Repository: cntryl/stress`
4. Leave branch/tag patterns empty

### Publishing

From GitHub Actions:
1. Go to Actions → "Publish" workflow
2. Click "Run workflow"
3. Select crate: `macros` or `core`
4. Workflow validates, publishes, and creates release tag

**Order matters**: Publish `macros` first, then `core` (since core depends on macros).

## Contributing

We welcome contributions! See [CONTRIBUTING.md](CONTRIBUTING.md) for:

- Development setup
- Code quality standards (rustfmt, clippy, tests)
- Pull request process
- Commit message guidelines

Quick checklist:
- `cargo test --all` — all tests pass
- `cargo fmt --all` — code formatted
- `cargo clippy --all-targets -- -D warnings` — no warnings
- `cargo doc --all --no-deps` — documentation builds

## Security

Please report security vulnerabilities privately:

1. **Don't** open a public GitHub issue
2. Go to Security → Report a vulnerability
3. Or email the maintainers

See [SECURITY.md](SECURITY.md) for details.

## Attribute Reference

```rust
// Basic benchmark
#[stress_test]
fn my_bench(ctx: &mut StressContext) { ... }

// Skip (run with --include-ignored)
#[stress_test(ignore)]
fn slow_bench(ctx: &mut StressContext) { ... }

// Custom name (instead of function name)
#[stress_test(name = "custom_name")]
fn internal_name(ctx: &mut StressContext) { ... }
```

## API Reference

### StressContext

```rust
pub struct StressContext {
    pub fn measure<F>(&mut self, f: F)        // Time the operation
    pub fn set_bytes(&mut self, n: u64)       // Enable throughput (bytes/sec)
    pub fn set_elements(&mut self, n: u64)    // Enable throughput (ops/sec)
    pub fn tag(&mut self, key: &str, value: &str)  // Metadata
}
```

### BenchRunner

```rust
impl BenchRunner {
    pub fn new(suite: &str) -> Self                    // Default config
    pub fn with_config(suite: &str, config: BenchRunnerConfig) -> Self
    pub fn run<F>(&mut self, name: &str, f: F)        // Run benchmark
    pub fn group<F>(&mut self, name: &str, f: F)      // Group related benchmarks
    pub fn metadata(&mut self, key: &str, val: &str)  // Add metadata
    pub fn finish(self) -> Vec<BenchResult>           // Finish and report
}
```

### BenchResult

```rust
pub struct BenchResult {
    pub name: String,                       // Full name (suite/benchmark)
    pub duration: Duration,                 // Median duration
    pub bytes: Option<u64>,                 // Total bytes processed
    pub elements: Option<u64>,              // Total elements processed
    pub all_runs: Vec<Duration>,            // Individual run durations
    pub tags: HashMap<String, String>,      // Metadata
}
```

## Examples

See `demo/benches/` for full examples:

- `stress_demo.rs` — Basic benchmarks (I/O, memory, computation)
- `stress_demo1.rs` — Write operations, allocation, math
- `stress_demo2.rs` — Sorting, hashing, memory copy, recursion

Run them:

```bash
cargo bench --bench stress-demo
cargo bench --bench stress-demo1
cargo bench --bench stress-demo2

# With multiple runs
BENCH_RUNS=3 cargo bench --bench stress-demo1
```

## Troubleshooting

### Measurements seem inconsistent

This is normal for single-shot benchmarks. Use `BENCH_RUNS=5` to get the median across multiple runs.

### Throughput not showing

Ensure you set bytes or elements:
```rust
ctx.set_bytes(data.len() as u64);   // For bytes/sec
ctx.set_elements(count as u64);      // For ops/sec
```

### No output files generated

Check:
- `BENCH_OUTPUT_DIR` exists and is writable
- `BENCH_VERBOSE` is not explicitly set to `false`
- Benchmark actually ran (no filter excluded it)

### `stress_main!()` not found

Ensure:
1. You have the `[[bench]]` section in `Cargo.toml` with `harness = false`
2. You're importing: `use cntryl_stress::{stress_main, stress_test, StressContext};`
3. You have `stress_main!();` at the end of your benchmark file

### Results written to unexpected location

Check the `BENCH_OUTPUT_DIR` environment variable. The default is `target/stress/`. Results are organized as:
```
target/stress/
  {suite-name}/
    {timestamp}.json
    {timestamp}.txt
    latest.json
    latest.txt
```

### Console output formatting looks wrong

Console output uses `us` (microseconds) instead of `µs` for Windows console compatibility. All duration units are: `ns`, `us`, `ms`, `s`.

## License

Licensed under Apache License 2.0. See [LICENSE](LICENSE).

## Inspiration

- [Criterion.rs](https://github.com/bheisler/criterion.rs) — measurement framework
- [Flamegraph](https://www.brendangregg.com/flamegraphs.html) — profiling approach
- [Go testing](https://golang.org/pkg/testing/) — simple, built-in benchmarking
