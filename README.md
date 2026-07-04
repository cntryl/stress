# cntryl-stress

[![crates.io](https://img.shields.io/crates/v/cntryl-stress.svg)](https://crates.io/crates/cntryl-stress)
[![docs.rs](https://docs.rs/cntryl-stress/badge.svg)](https://docs.rs/cntryl-stress)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)

Raw-sample-first benchmarking for Tier 1 hot paths through Tier 6 workload and system performance work.

## Quick Start

```toml
[dev-dependencies]
cntryl-stress = "0.3"

[[bench]]
name = "storage_stress"
path = "benches/storage_stress.rs"
harness = false
```

```rust
use cntryl_stress::{black_box, stress_main, stress_test, StressContext};

cntryl_stress::stress_allocator!();

#[stress_test(tier = 1, max_allocs_per_op = 0, max_bytes_per_op = 0)]
fn parse_route_hot_path(ctx: &mut StressContext) {
    let route = b"tenant-a.queue.primary";
    ctx.measure_micro(|| black_box(route.iter().position(|byte| *byte == b'.')));
}

#[stress_test(tier = 2, metadata(component = "storage"))]
fn write_batch(ctx: &mut StressContext) {
    let batch = vec![0_u8; 4096];

    ctx.parameter("payload_size", batch.len());
    ctx.measure(|| {
        std::fs::write("target/stress-write.tmp", &batch).unwrap();
    });

    std::fs::remove_file("target/stress-write.tmp").ok();
}

stress_main!();
```

```bash
cargo bench --bench storage_stress
cargo bench --bench storage_stress -- --workload '*fanout*'
```

The optional `cargo stress` wrapper is feature-gated so ordinary benchmark builds do not compile its CLI dependency graph:

```bash
cargo install cntryl-stress --features cli
cargo stress
cargo stress --baseline target/stress/storage_stress/latest.json
```

## Model

- `tier = 1..6` describes benchmark scope:
  - Tier 1: hot path
  - Tier 2: subsystem
  - Tier 3: system
  - Tier 4: integration
  - Tier 5: saturation/scaling
  - Tier 6: soak/endurance
- Use `smoke` for a quick correctness-focused diagnostic run.
- The `default` profile is the normal day-to-day run: useful per-tier signal without paying the exhaustive lab cost.
- Use `lab` for deeper exploratory runs with more samples and longer sample windows.
- Use `release` for the trustworthy release-quality gate: quality enforcement and regression enforcement when a baseline is supplied.
- JSON artifacts use `schema_version: "cntryl-stress.v1"`.
- Raw `Sample` rows are authoritative; summaries, quality, and comparisons are derived from measured samples only.
- Warmup and cooldown samples are retained in JSON and excluded from summary statistics and baseline comparison.
- Tier drives benchmark mode: Tier 1 uses `micro`, Tier 2 uses `fixed_operations`, and Tiers 3-6 use `fixed_duration`.
- `mode = "..."` is optional compatibility syntax and must match the tier-derived mode.
- Console rows include a `wall` column with total wall-clock time spent running that benchmark method across warmup, measured, and cooldown samples.

## Tier Recipes

Pick the tier first, then use the matching timing shape. The detailed copy-paste guide is in [docs/bench-recipes.md](docs/bench-recipes.md).

| Tier | Scope | Recipe |
|------|-------|--------|
| 1 | Hot path | `ctx.measure_micro(|| hot_path())` |
| 2 | Subsystem operation | `ctx.measure(|| one_operation())` or `ctx.measure_counted(|| completed_batch())` |
| 3 | System throughput | `#[stress_test(tier = 3)]` plus `ctx.measure_batch(n, || batch())` |
| 4 | Integration throughput | Fixed-duration `ctx.measure_batch(n, ...)` or `ctx.record_external(duration, n)` |
| 5 | Saturation/scaling | Fixed-duration `ctx.measure_batch(n, ...)` with scale parameters |
| 6 | Soak/endurance | Fixed-duration `ctx.measure_batch(n, ...)` or `ctx.record_external(duration, n)` over the soak window |

## Benchmark API

The common case stays small:

```rust
#[stress_test(tier = 1)]
fn parse_document_header(ctx: &mut StressContext) {
    let document = load_document();
    ctx.parameter("payload_size", document.len());
    ctx.measure_micro(|| parse_header(&document));
}

#[stress_test(tier = 2)]
fn write_batch(ctx: &mut StressContext) {
    let batch = build_batch();
    ctx.parameter("payload_size", batch.len());
    ctx.measure(|| write(&batch));
}
```

Use `measure_counted` when one Tier 2 subsystem call returns logical work completed:

```rust
#[stress_test(tier = 2)]
fn flush_ready_entries(ctx: &mut StressContext) {
    let _completed = ctx.measure_counted(|| flush_ready_entries_once());
}
```

Use `measure_batch` when each framework iteration performs many logical operations:

```rust
#[stress_test(tier = 3)]
fn fanout(ctx: &mut StressContext) {
    let clients = 16_u64;
    ctx.parameter("client_count", clients);

    let _completed = ctx.measure_batch(clients, || {
        for client in 0..clients {
            send_one_request(client);
        }
    });
}
```

Use `record_external` when another harness owns timing:

```rust
#[stress_test(tier = 4)]
fn external_round_trip(ctx: &mut StressContext) {
    let report = run_external_harness();
    ctx.record_external(report.duration, report.completed_operations);
}
```

Useful context methods:

```rust
ctx.parameter("client_count", 16);
ctx.metadata("scenario", "fanout");
ctx.record_latency(duration);
ctx.correctness().attempted(n).completed(n).failures(0);
ctx.operations(n);
ctx.measure_micro(|| work());
ctx.measure(|| work());
ctx.measure_counted(|| work_count());
ctx.measure_batch(n, || work());
ctx.measure_workload(|| work());
ctx.record_external(duration, n);
```

## Attributes

```rust
#[stress_test]
#[stress_test(tier = 1)]
#[stress_test(tier = 1, mode = "micro")]
#[stress_test(tier = 2, mode = "fixed_operations")]
#[stress_test(tier = 4)]
#[stress_test(tier = 1, max_ns_per_op = 250, max_regression_pct = 5)]
#[stress_test(max_allocs_per_op = 0, max_bytes_per_op = 0, max_rsd_pct = 10)]
#[stress_test(name = "custom_name", ignore)]
#[stress_test(metadata(component = "queue", scenario = "fanout"))]
```

Tiers are defined as 1 through 6. The macro rejects `tier = 0`, `tier > 6`, and any explicit `mode` that does not match the tier.

## Run Policy

| Profile | Default Samples | Gate Behavior |
|---------|-----------------|---------------|
| `default` | 5 measured, 1 warmup | Fails correctness; reports noisy rows without failing quality |
| `smoke` | 1 measured, 0 warmup | Explicit diagnostic override; correctness-focused, no quality/regression failure |
| `lab` | 30 measured, 2 warmup, 1 cooldown | Exhaustive exploration; fails correctness and reports quality findings |
| `release` | 10 measured, 1 warmup | Fails correctness, quality below acceptable, and meaningful regressions |

Quality classes:

- `authoritative`: at least 10 measured samples and RSD <= 5%
- `acceptable`: at least 5 measured samples and RSD <= 10%
- `noisy`: correctness passed but sample count or variance is weak
- `untrustworthy`: too few samples, zero completed ops, invalid timing, or correctness failure

Baseline regressions are meaningful only when the primary metric moves past threshold and 95% confidence intervals do not overlap.
Benchmark budgets fail the run when exceeded. Micro rows below 5 ns/op are marked `suspicious_micro` unless the benchmark metadata includes `validated_micro = "true"`.

## Configuration

Command-line arguments override `STRESS_*` environment variables, which override the trustworthy defaults.

| Variable | Description |
|----------|-------------|
| `STRESS_PROFILE` | Optional profile override: `default`, `smoke`, `lab`, or `release` |
| `STRESS_SAMPLES` | Measured samples per benchmark |
| `STRESS_WARMUP_SAMPLES` | Warmup samples |
| `STRESS_COOLDOWN_SAMPLES` | Cooldown samples |
| `STRESS_FILTER` | Benchmark name/module filter |
| `STRESS_TIER` | Exact tier filter, 1 through 6 |
| `STRESS_OUTPUT_DIR` | Artifact output directory |
| `STRESS_CONSOLE` | Console mode: `default`, `verbose`, `quiet`, `json`, or `markdown` |
| `STRESS_INCLUDE_IGNORED` | Include ignored benchmarks |
| `STRESS_BASELINE` | Baseline stress artifact |
| `STRESS_THRESHOLD` | Regression threshold |
| `STRESS_GIT_SHA` | Git SHA override |
| `STRESS_SAMPLE_DURATION_MS` | Fixed-duration sample budget |
| `STRESS_OPERATIONS_PER_SAMPLE` | Fixed-operations sample size |
| `STRESS_MICRO_SAMPLE_DURATION_MS` | Micro sample target duration |

Harness options:

```bash
cargo bench --bench storage_stress -- --tier 3 --workload '*fanout*'
cargo bench --bench storage_stress -- --samples 10 --warmup-samples 1
cargo bench --bench storage_stress -- --baseline target/stress/storage_stress/latest.json
cargo bench --bench storage_stress -- --print-config
```

Console modes:

```bash
cargo bench --bench storage_stress -- --console default
cargo bench --bench storage_stress -- --console verbose
cargo bench --bench storage_stress -- --console quiet
cargo bench --bench storage_stress -- --console json
cargo bench --bench storage_stress -- --console markdown
```

The default console output is a compact decision surface: grouped benchmark rows, `ns/op` for Tier 1 micro rows, optional allocation and overhead columns, wall-clock time per benchmark, quality labels, baseline deltas, summary counts, and a needs-attention block. Throughput percentile columns are sample-throughput percentiles, not operation latency percentiles.

## Artifacts

Files are written under `target/stress/{suite}/`:

- `{timestamp}.json` and `latest.json`
- `{timestamp}.txt` and `latest.txt`
- `{timestamp}.md` and `latest.md`

The JSON artifact contains tool version, run profile, environment, benchmark specs, raw samples, summaries, quality, and comparisons. Unknown environment fields are explicit `"unknown"` or `null`.

## Programmatic Runner

```rust
use cntryl_stress::{StressRunner, StressRunnerConfig};

let config = StressRunnerConfig::new().filter("write");

let mut runner = StressRunner::with_config("storage", config);
runner.run("write_batch", |ctx| {
    let batch = vec![0_u8; 4096];
    ctx.parameter("payload_size", batch.len());
    ctx.measure(|| write_batch(&batch));
});
let run = runner.finish();
```

## Development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::pedantic
cargo test --workspace --all-features
cargo doc --workspace --all-features --no-deps
```

## License

Apache License 2.0. See [LICENSE](LICENSE).
