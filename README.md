# cntryl-stress

[![crates.io](https://img.shields.io/crates/v/cntryl-stress.svg)](https://crates.io/crates/cntryl-stress)
[![docs.rs](https://docs.rs/cntryl-stress/badge.svg)](https://docs.rs/cntryl-stress)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)

Raw-sample-first stress benchmarking for Tier 2 through Tier N performance work. Tier 1 microbenchmarks stay in Criterion.

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
use cntryl_stress::{stress_main, stress_test, StressContext};

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
cargo stress
cargo stress --baseline target/stress/storage_stress/latest.json
```

## Model

- `tier = 2..N` describes benchmark scope.
- The default run is the trustworthy release-quality gate: 10 measured samples, 1 warmup sample, quality enforcement, and regression enforcement when a baseline is supplied.
- `smoke` and `lab` remain explicit diagnostic overrides for callers that need a quick check or deeper exploration.
- JSON artifacts use `schema_version: "cntryl-stress.v2"`.
- Raw `Sample` rows are authoritative; summaries, quality, and comparisons are derived from measured samples only.
- Warmup and cooldown samples are retained in JSON and excluded from summary statistics and baseline comparison.
- Criterion remains the right tool for Tier 1 microbenchmarks.

## Benchmark API

The common case stays small:

```rust
#[stress_test(tier = 2)]
fn parse_document(ctx: &mut StressContext) {
    let document = load_document();
    ctx.parameter("payload_size", document.len());
    ctx.measure(|| parse(&document));
}
```

Use correctness counters when one sample represents many operations:

```rust
#[stress_test(tier = 3, mode = "fixed_duration")]
fn fanout(ctx: &mut StressContext) {
    ctx.parameter("client_count", 16);

    let completed = ctx.measure_workload(|| send_one_request());
    let _ = ctx.correctness().attempted(completed).completed(completed);
}
```

Useful context methods:

```rust
ctx.parameter("client_count", 16);
ctx.metadata("scenario", "fanout");
ctx.record_latency(duration);
ctx.correctness().attempted(n).completed(n).failures(0);
ctx.measure(|| work());
ctx.measure_for(duration, || work());
ctx.measure_workload(|| work());
```

## Attributes

```rust
#[stress_test]
#[stress_test(tier = 4)]
#[stress_test(tier = 3, mode = "fixed_duration")]
#[stress_test(name = "custom_name", ignore)]
#[stress_test(metadata(component = "queue", scenario = "fanout"))]
```

Tiers start at 2. The macro rejects `tier = 1`.

## Run Policy

| Profile | Default Samples | Gate Behavior |
|---------|-----------------|---------------|
| `release` (default) | 10 measured, 1 warmup | Fails correctness, quality below acceptable, and meaningful regressions |
| `smoke` | 1 measured, 0 warmup | Explicit diagnostic override; correctness-focused, no quality/regression failure |
| `lab` | 30 measured, 2 warmup, 1 cooldown | Fails correctness; reports noisy rows without failing quality |

Quality classes:

- `authoritative`: at least 10 measured samples and RSD <= 5%
- `acceptable`: at least 5 measured samples and RSD <= 10%
- `noisy`: correctness passed but sample count or variance is weak
- `untrustworthy`: too few samples, zero completed ops, invalid timing, or correctness failure

Baseline regressions are meaningful only when the primary metric moves past threshold and 95% confidence intervals do not overlap.

## Configuration

Command-line arguments override `STRESS_*` environment variables, which override the trustworthy defaults.

| Variable | Description |
|----------|-------------|
| `STRESS_PROFILE` | Optional profile override: `release`, `smoke`, or `lab` |
| `STRESS_SAMPLES` | Measured samples per benchmark |
| `STRESS_WARMUP_SAMPLES` | Warmup samples |
| `STRESS_COOLDOWN_SAMPLES` | Cooldown samples |
| `STRESS_FILTER` | Benchmark name/module filter |
| `STRESS_TIER` | Exact tier filter |
| `STRESS_OUTPUT_DIR` | Artifact output directory |
| `STRESS_VERBOSE` | Console output |
| `STRESS_INCLUDE_IGNORED` | Include ignored benchmarks |
| `STRESS_BASELINE` | v2 baseline artifact |
| `STRESS_THRESHOLD` | Regression threshold |
| `STRESS_GIT_SHA` | Git SHA override |
| `STRESS_SAMPLE_DURATION_MS` | Fixed-duration sample budget |
| `STRESS_OPERATIONS_PER_SAMPLE` | Fixed-operations sample size |

Harness options:

```bash
cargo bench --bench storage_stress -- --tier 3 --workload '*fanout*'
cargo bench --bench storage_stress -- --samples 10 --warmup-samples 1
cargo bench --bench storage_stress -- --baseline target/stress/storage_stress/latest.json
cargo bench --bench storage_stress -- --print-config
```

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
