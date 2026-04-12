# cntryl-stress

[![crates.io](https://img.shields.io/crates/v/cntryl-stress.svg)](https://crates.io/crates/cntryl-stress)
[![docs.rs](https://docs.rs/cntryl-stress/badge.svg)](https://docs.rs/cntryl-stress)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)

A lightweight benchmark runner for expensive operations: disk I/O, network calls, database transactions, compaction, recovery.

Unlike Criterion (statistical sampling), cntryl-stress supports both single-shot measurements and duration-bounded throughput loops where each iteration matters.

## Quick Start

### 1. Add to `Cargo.toml`

```toml
[dev-dependencies]
cntryl-stress = "0.2"

[[bench]]
name = "my_stress"
path = "benches/my_stress.rs"
harness = false
```

### 2. Write benchmark (`benches/my_stress.rs`)

```rust
use cntryl_stress::{stress_test, stress_main, StressContext};

#[stress_test]
fn write_file(ctx: &mut StressContext) {
    let data = vec![0u8; 1024 * 1024];
    ctx.set_bytes(data.len() as u64);

    ctx.measure(|| {
        std::fs::write("/tmp/test", &data).unwrap();
    });
    std::fs::remove_file("/tmp/test").ok();
}

#[stress_test]
fn allocate_buffer(ctx: &mut StressContext) {
    ctx.measure(|| {
        let _buf = vec![0u32; 1_000_000];
    });
}

stress_main!();
```

### 3. Run

```bash
cargo bench --bench my_stress
cargo bench --bench my_stress -- --runs 5 --warmup 1
BENCH_RUNS=5 BENCH_WARMUP=1 cargo bench --bench my_stress
```

## Features

- **Single-shot measurements** — no statistical overhead
- **Duration-bounded measurements** — keep a benchmark running long enough for authoritative throughput numbers
- **Explicit timing** — setup/teardown excluded from measurements
- **Throughput tracking** — bytes/sec or ops/sec reporting
- **JSON + text output** — machine and human-readable formats
- **Filtering** — run benchmarks by name pattern
- **Warmup runs** — discard warmup iterations, report median
- **Auto-discovery** — `#[stress_test]` with `stress_main!()`
- **Manual API** — `BenchRunner` for full control

## Output

Benchmarks create timestamped results in `target/stress/{suite}/{timestamp}.{json,txt}`:

**Console:**
```
Benchmark Suite: my_stress
Runs: 5, Warmup: 1
---------------------------------------------------------------

  write_file                             15.32ms  (65.28 MB/s)
  allocate_buffer                        45.18ms
  hash_items                             3.01s  (25.05M ops/s)
---------------------------------------------------------------
Completed 2 benchmarks in 60.50ms
```

**JSON** (`target/stress/my_stress/latest.json`):
```json
{
  "suite": "my_stress",
  "results": [
    {
      "name": "my_stress/write_file",
      "duration_ns": 15320000,
      "bytes": 1048576
    }
  ],
  "total_duration_ns": 60500000,
  "started_at": "1771376841729",
  "runs": 5,
  "warmup_runs": 1,
  "git_sha": "36d2a432..."
}
```

**Text** (`target/stress/my_stress/latest.txt`):
```
===============================================================
Benchmark Suite: my_stress
Completed: 1771376841729
Git SHA:   36d2a432...

Results:
  write_file                             15.32ms  (65.28 MB/s)
  allocate_buffer                        45.18ms

Total time: 60.50ms
Benchmarks: 2
===============================================================
```

## Configuration

Configuration precedence for runtime settings is:

1. Explicit command-line arguments
2. Environment variables
3. Crate defaults

For run counts specifically: `--runs`/`--warmup` override `BENCH_RUNS`/`BENCH_WARMUP`,
and environment values fall back to defaults (`runs = 1`, `warmup = 0`) when unset.

Malformed environment values are warned about and ignored. For example, `BENCH_RUNS=abc`
emits a warning and falls back to the default resolution path.

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `BENCH_RUNS` | `1` | Measurement runs (reports median) |
| `BENCH_WARMUP` | `0` | Discarded warmup runs |
| `BENCH_FILTER` | - | Filter benchmarks by name substring |
| `BENCH_OUTPUT_DIR` | `target/stress` | Output directory for results |
| `BENCH_VERBOSE` | `true` | Print to console |
| `BENCH_INCLUDE_IGNORED` | `false` | Include `#[stress_test(ignore)]` |
| `BENCH_BASELINE` | - | Baseline JSON for regression comparison |
| `BENCH_THRESHOLD` | `0.05` | Regression threshold |
| `BENCH_GIT_SHA` | auto | Override git SHA in results |

```bash
BENCH_RUNS=5 BENCH_WARMUP=2 cargo bench --bench my_stress
```

### Command-Line Arguments

Pass arguments to the stress harness using `--` separator:

```bash
cargo bench --bench my_stress -- --runs 5 --warmup 2
cargo bench --bench 'stress-*' -- --runs 3 --workload write
cargo bench --bench my_stress -- --print-config
cargo bench -- --help
```

**Available options:**
- `--runs <N>` — Measurement runs (fallback: `BENCH_RUNS`, then `1`)
- `--warmup <N>` — Warmup runs (fallback: `BENCH_WARMUP`, then `0`)
- `--workload <PATTERN>` — Filter benchmarks by glob pattern
- `--verbose`, `-v` — Verbose output
- `--quiet`, `-q` — Quiet mode
- `--include-ignored` — Include ignored benchmarks
- `--list` — List benchmarks without running
- `--print-config` — Print resolved config and exit
- `--output-dir <PATH>` — Output directory
- `--baseline <PATH>` — Baseline JSON for regression comparison (fallback: `BENCH_BASELINE`)
- `--threshold <FLOAT>` — Regression threshold (fallback: `BENCH_THRESHOLD`, then `0.05`)

**Important:** The `--` is required to separate cargo flags from stress harness flags.

### Programmatic Configuration

```rust
use cntryl_stress::{BenchRunner, BenchRunnerConfig};

let config = BenchRunnerConfig::new()
    .runs(5)
    .warmup(2)
    .filter("write");

let mut runner = BenchRunner::with_config("my_suite", config);
runner.run("op", |ctx| ctx.measure(|| { /* ... */ }));
runner.finish();
```

### Env-Aware Entry Point

For integrations that want the same precedence behavior as `stress_main!()` without relying on the macro, use the explicit env-aware entrypoint:

```rust
fn main() {
  cntryl_stress::run_from_env_and_args();
}
```

This resolves config with `CLI > env > defaults`, prints warnings for malformed env vars, and supports `--print-config`.

## Throughput

Enable throughput reporting by measurement type:

```rust
// Bytes per second
#[stress_test]
fn compress(ctx: &mut StressContext) {
    let data = vec![0u8; 10 * 1024 * 1024];
    ctx.set_bytes(data.len() as u64);
    ctx.measure(|| { compress_data(&data) });
}
// Output: compress ... 1.23s (8.13 MB/s)

// Operations per second
#[stress_test]
fn hash_items(ctx: &mut StressContext) {
  use std::time::Duration;

  let item_count = 1_000_000usize;
  let iterations = ctx.measure_for(Duration::from_secs(3), || {
    let _ = (0..item_count)
      .map(|value| value.wrapping_mul(31))
      .collect::<Vec<_>>();
  });
  ctx.set_elements((item_count * iterations) as u64);
}
// Output: hash_items ... 3.01s (184.50M ops/s)
```

## Attributes

```rust
#[stress_test]                              // Basic benchmark
#[stress_test(ignore)]                      // Skip (use --include-ignored to run)
#[stress_test(name = "custom_name")]        // Custom name override
```

## API Reference

### StressContext

```rust
pub fn measure<F>(&mut self, f: F)           // Time one operation
pub fn measure_for<F>(&mut self, duration: Duration, f: F) -> usize
                                             // Time repeated work for a wall-clock budget
pub fn set_bytes(&mut self, n: u64)          // Enable bytes/sec throughput
pub fn set_elements(&mut self, n: u64)       // Enable ops/sec throughput
pub fn tag(&mut self, key: &str, val: &str)  // Add metadata
```

### BenchRunner

```rust
impl BenchRunner {
    pub fn new(suite: &str) -> Self
    pub fn with_config(suite: &str, config: BenchRunnerConfig) -> Self
    pub fn run<F>(&mut self, name: &str, f: F)
    pub fn group<F>(&mut self, name: &str, f: F)
    pub fn metadata(&mut self, key: &str, val: &str)
    pub fn finish(self) -> Vec<BenchResult>
}
```

## Examples

See `demo/benches/`:
- `stress_demo.rs` — I/O, memory, computation basics
- `stress_demo1.rs` — File writes, allocation, math
- `stress_demo2.rs` — Sorting, hashing, copying, recursion with both timing modes

```bash
cargo bench --bench stress-demo
cargo bench --bench stress-demo1 -- --runs 3
cargo bench --bench 'stress-*' -- --runs 5 --warmup 2
```

## Alongside Criterion

cntryl-stress works perfectly with Criterion in the same project:

```toml
[dev-dependencies]
cntryl-stress = "0.2"
criterion = "0.5"

[[bench]]
name = "fast"
harness = true

[[bench]]
name = "slow"
path = "benches/slow.rs"
harness = false
```

**Why both?**
- **Criterion**: Micro-benchmarks with statistical analysis
- **cntryl-stress**: System-level operations, single measurements
- **Together**: Complete performance picture

## Troubleshooting

**`unknown option: --runs`?**
Use `--` separator: `cargo bench -- --runs 5` not `cargo bench --runs 5`.

**Inconsistent measurements?**
Use `BENCH_RUNS=5` to get median across multiple runs.

**Throughput not showing?**
Call `ctx.set_bytes()` or `ctx.set_elements()` in your benchmark.

**No output files?**
Check that `BENCH_OUTPUT_DIR` is writable and `BENCH_VERBOSE` isn't `false`.

**`stress_main!()` not found?**
Ensure `[[bench]]` has `harness = false` and you're importing correctly:
```rust
use cntryl_stress::{stress_test, stress_main, StressContext};
```

**Output in wrong location?**
Files go to `target/stress/{suite}/{timestamp}.{json,txt}`. Override with `BENCH_OUTPUT_DIR=./custom`.

## Publishing

cntryl-stress publishes to crates.io using OIDC trusted publishing (no stored secrets).

### Setup (One-time)
1. Go to https://crates.io/me
2. Create token → "Create token with OIDC"
3. Set `Repository: cntryl/stress`

### Publish
1. Go to GitHub Actions → "Publish" workflow
2. Click "Run workflow"
3. Select crate: `macros` or `core`
4. Workflow publishes and creates release tag

**Note**: Publish `macros` before `core` (dependency order).

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) for:
- Setup and development workflow
- Code standards (rustfmt, clippy)
- Pull request process

Quick checks:
```bash
cargo test --all
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo doc --all --no-deps
```

## Security

Report vulnerabilities privately at GitHub Security → Report a vulnerability.
See [SECURITY.md](SECURITY.md) for details.

## License

Apache License 2.0 — See [LICENSE](LICENSE)

---

Inspired by [Criterion.rs](https://github.com/bheisler/criterion.rs), [Flamegraph](https://www.brendangregg.com/flamegraphs.html), and [Go testing](https://golang.org/pkg/testing/).
