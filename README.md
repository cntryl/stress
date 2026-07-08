# cntryl-stress

[![crates.io](https://img.shields.io/crates/v/cntryl-stress.svg)](https://crates.io/crates/cntryl-stress)
[![docs.rs](https://docs.rs/cntryl-stress/badge.svg)](https://docs.rs/cntryl-stress)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)

Performance benchmarks for engineers who need trustworthy artifacts, not just
timing numbers.

`cntryl-stress` is an opinionated Rust benchmarking framework for performance
engineering loops. It keeps benchmark authoring low ceremony while producing
structured artifacts, diagnostics, and gates that can support real optimization
decisions.

The core question is simple: **can this benchmark row be trusted?**

`cntryl-stress` helps answer that by recording raw samples, deriving summaries
from measured samples only, preserving correctness counters, and calling out
common benchmark-shape mistakes such as uncounted batch work, invalid timing,
high variance, setup-dominated measurements, and missing allocation tracking.

## What It Optimizes For

- Small benchmark bodies that read like tests.
- Deterministic fixtures with setup outside measured work.
- Named measurements and stable row identifiers.
- Logical operation counts for batch and throughput work.
- Machine-readable JSON artifacts under `target/stress`.
- Human output that prioritizes value, variance, allocations, and fixes.
- Release gates based on correctness, budgets, diagnostics, quality, and
  baseline comparisons.

## When To Use It

Use `cntryl-stress` when benchmark output needs to feed an engineering
workflow:

- Day-to-day optimization loops.
- CI performance gates.
- Baseline refresh decisions.
- Release-quality benchmark reports.
- Subsystem, integration, throughput, saturation, or soak-style workloads.
- Allocation-aware hot-path and parser/constructor benchmarks.

`cntryl-stress` is public tooling, but it is intentionally not neutral. It has
opinions about benchmark shape because those opinions make performance work
easier to repeat and harder to misread.

## When Not To Use It

Do not reach for `cntryl-stress` first when you only need a quick one-off
timing, rich statistical plotting for an isolated function, or a fully custom
benchmarking policy. It works best when you want conventions, JSON artifacts,
and actionable diagnostics more than a blank-slate benchmark harness.

## Benchmark Model

`cntryl-stress` organizes benchmarks by intent rather than by raw size:

- Tier 1: Hot paths and microbenchmarks.
- Tier 2: Subsystem operations.
- Tier 3: System behavior.
- Tier 4: Integration workloads.
- Tier 5: Saturation and scaling scenarios.
- Tier 6: Soak and endurance runs.

Each tier uses the same authoring API and reporting model, but the default
timing shape changes to match the expected workload: micro timing for Tier 1,
fixed operations for Tier 2, and fixed-duration windows for Tiers 3 through 6.

## Dependency Posture

`cntryl-stress` keeps the dependencies that directly support the benchmark
authoring and reporting experience.

- `serde` and `serde_json` are core dependencies because JSON artifacts, baselines, schema validation, and machine-readable output are part of the stable workflow.
- `linkme` keeps `#[stress]` benchmarks automatically registered without asking users to maintain manual benchmark lists.
- `cntryl-stress-macros` and its proc-macro stack power the macro-first API, including async benchmark support and benchmark metadata.
- `clap` and `anyhow` are limited to the optional `cli` feature used by the `cargo stress` wrapper, so ordinary benchmark builds do not compile that CLI dependency graph.

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
use cntryl_stress::{black_box, stress, stress_main, StressContext};

cntryl_stress::stress_allocator!();

#[stress(tier = 1, max_allocs_per_op = 0, max_bytes_per_op = 0)]
fn parse_route_hot_path(ctx: &mut StressContext) {
    let route = b"tenant-a.queue.primary";
    ctx.measure("dot lookup", || black_box(route.iter().position(|byte| *byte == b'.')));
}

#[stress(tier = 2, metadata(component = "storage"))]
fn write_batch(ctx: &mut StressContext) {
    let batch = vec![0_u8; 4096];

    ctx.parameter("payload_size", batch.len());
    ctx.measure("write temp file", || {
        std::fs::write("target/stress-write.tmp", &batch).unwrap();
    });

    std::fs::remove_file("target/stress-write.tmp").ok();
}

stress_main!();
```

```bash
cargo bench --bench storage_stress
cargo bench --bench storage_stress -- --workload 'parse_route_hot_path'
```

The optional `cargo stress` wrapper is feature-gated so ordinary benchmark builds do not compile its CLI dependency graph:

```bash
cargo install cntryl-stress --features cli
cargo stress
cargo stress --baseline target/stress/storage_stress/latest.json
```

## Selection

`--workload` filters the registered benchmark set before execution. It matches
the display name, Rust function name, module path, `module_path::function_name`,
and `module_path::display_name`.

```bash
cargo bench --bench storage_stress -- --workload 'parse_route_hot_path'
cargo bench --bench storage_stress -- --workload 'queue::writer::*'
```

When no row matches, stress prints close registered candidates instead of
failing with an empty selection message.

## Run Semantics

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
- JSON artifacts use `schema_version: "cntryl-stress.v2"`.
- `STRESS_RUN_ID` is copied into run metadata when present; `cargo stress`
  creates one shared run id for all child benchmark binaries unless the caller
  already supplied one.
- Raw `Sample` rows are the source of truth; summaries, diagnostics, quality,
  and comparisons are derived from measured samples only.
- Warmup and cooldown samples are retained in JSON and excluded from summary statistics and baseline comparison.
- Tier drives benchmark mode: Tier 1 uses micro timing, Tier 2 uses fixed operations, and Tiers 3-6 use fixed duration.
- `mode = "..."` is not public API; choose `#[stress(tier = 1..6)]`.
- Human console output is one table per suite. Bench-shape, result, and diagnostic issues are grouped after the table with concrete fixes.
- Tier 1 batch rows mark `ns_per_op_basis = logical_completed_operation` so
  baseline tools can treat changed ns/op semantics as a baseline refresh event,
  not as a performance regression.
- `metadata(row_class = "construction" | "parsing" | "allocation")` marks
  allocation-oriented rows so allocation diagnostics are advisory unless an
  explicit allocation budget fails.

## Tier Recipes

Pick the tier first, then use the matching timing shape. The detailed copy-paste guide is in [docs/bench-recipes.md](docs/bench-recipes.md).

| Tier | Scope | Recipe |
|------|-------|--------|
| 1 | Hot path | `ctx.measure("parse", || hot_path())` |
| 2 | Subsystem operation | `ctx.measure("write", || one_operation())` or `ctx.measure_batch("flush", n, || batch())` |
| 3 | System throughput | `#[stress(tier = 3)]` plus `ctx.measure_batch("fanout", n, || batch())` |
| 4 | Integration throughput | Fixed-duration `ctx.measure_batch("round trip", n, ...)` or `ctx.record_external("round trip", duration, n)` |
| 5 | Saturation/scaling | Fixed-duration `ctx.measure_batch("fanout", n, ...)` with scale parameters |
| 6 | Soak/endurance | Fixed-duration `ctx.measure_batch("churn", n, ...)` or `ctx.record_external("soak", duration, n)` over the soak window |

## Benchmark API

The common case stays small:

```rust
#[stress(tier = 1)]
fn parse_document_header(ctx: &mut StressContext) {
    let document = load_document();
    ctx.parameter("payload_size", document.len());
    ctx.measure("parse header", || parse_header(&document));
}

#[stress(tier = 2)]
fn write_batch(ctx: &mut StressContext) {
    let batch = build_batch();
    ctx.parameter("payload_size", batch.len());
    ctx.measure("write batch", || write(&batch));
}
```

Use `measure_batch` when one measured call completes multiple logical operations:

```rust
#[stress(tier = 2)]
fn flush_ready_entries(ctx: &mut StressContext) {
    let ready = ready_entry_count();
    let _completed = ctx.measure_batch("flush ready entries", ready, || flush_ready_entries_once());
}
```

Use `measure_batch` when each framework iteration performs many logical operations:

```rust
#[stress(tier = 3)]
fn fanout(ctx: &mut StressContext) {
    let clients = 16_u64;
    ctx.parameter("client_count", clients);

    let _completed = ctx.measure_batch("client fanout", clients, || {
        for client in 0..clients {
            send_one_request(client);
        }
    });
}
```

Use `record_external` when another harness owns timing:

```rust
#[stress(tier = 4)]
fn external_round_trip(ctx: &mut StressContext) {
    let report = run_external_harness();
    ctx.record_external("round trip", report.duration, report.completed_operations);
}
```

Use `measure_async` inside an async benchmark function when the measured work is a future:

```rust
#[stress(tier = 2)]
async fn async_lookup(ctx: &mut StressContext) {
    ctx.measure_async("lookup", || async { lookup_once().await }).await;
}
```

Use the builder path when one row needs local run-shape overrides:

```rust
ctx.benchmark("large fanout")
    .samples(20)
    .warmup(2)
    .measure_batch(client_count, || run_fanout_once());
```

Useful context methods:

```rust
ctx.parameter("client_count", 16);
ctx.metadata("scenario", "fanout");
ctx.record_latency(duration);
ctx.correctness().attempted(n).completed(n).failures(0);
ctx.operations(n);
ctx.measure("name", || work());
ctx.measure_batch("name", n, || work());
ctx.measure_async("name", || async { work().await }).await;
ctx.measure_threaded("name", || work());
ctx.measure_pipeline("name", || work());
ctx.measure_io("name", || work());
ctx.record_external("name", duration, n);
```

## Attributes

```rust
#[stress]
#[stress(tier = 1)]
#[stress(tier = 4)]
#[stress(tier = 1, max_ns_per_op = 250, max_regression_pct = 5)]
#[stress(max_allocs_per_op = 0, max_bytes_per_op = 0, max_rsd_pct = 10)]
#[stress(name = "custom_name", ignore)]
#[stress(metadata(component = "queue", scenario = "fanout"))]
#[stress(metadata(row_class = "parsing"))]
```

Tiers are defined as 1 through 6. The macro rejects `tier = 0`, `tier > 6`, and any explicit `mode`.

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
Benchmark budgets fail the run when exceeded. Diagnostics are structured on each summary with `code`, `severity`, `reason`, `evidence`, and `suggestions`.

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
| `STRESS_JSON` | Emit machine-readable JSON to stdout instead of the console table |
| `STRESS_INCLUDE_IGNORED` | Include ignored benchmarks |
| `STRESS_BASELINE` | Baseline stress artifact |
| `STRESS_BASELINE_DIR` | Baseline directory for `latest` and `--save-baseline` conventions |
| `STRESS_SAVE_BASELINE` | Save a passed run under the baseline directory |
| `STRESS_THRESHOLD` | Regression threshold |
| `STRESS_GIT_SHA` | Git SHA override |
| `STRESS_SAMPLE_DURATION_MS` | Fixed-duration sample budget |
| `STRESS_OPERATIONS_PER_SAMPLE` | Fixed-operations sample size |
| `STRESS_MICRO_SAMPLE_DURATION_MS` | Micro sample target duration |
| `STRESS_RUN_ID` | Run generation identity copied into artifact metadata |
| `STRESS_FAIL_ON_ISSUES` | Fail on warning-or-error diagnostics |
| `STRESS_DENY_DIAGNOSTICS` | Fail on diagnostics at `info`, `warning`, or `error` |
| `STRESS_CONSOLE_NAMES` | Human console name mode: `compact` or `full` |
| `STRESS_PROGRESS` | Enable or disable stderr progress for human output |

Harness options:

```bash
cargo bench --bench storage_stress -- --tier 3 --workload '*fanout*'
cargo bench --bench storage_stress -- --samples 10 --warmup-samples 1
cargo bench --bench storage_stress -- --baseline target/stress/storage_stress/latest.json
cargo bench --bench storage_stress -- --print-config
```

Console output:

```bash
cargo bench --bench storage_stress
cargo bench --bench storage_stress -- --json
```

`cargo bench --bench ...` uses one console format: one simple benchmark table per suite with `benchmark`, `value`, `p50`, `p95`, `p99`, `rsd`, `alloc/op`, and `B/op` columns. Suite-local `issues` appear directly after a table only when a row needs attention, and the run ends with one `result:` line. Use `--json` only for machine-readable stdout.

## Artifacts

Files are written under `target/stress/{suite}/`:

- `{timestamp}.json` and `latest.json`
- `{timestamp}.txt` and `latest.txt`
- `{timestamp}.md` and `latest.md`

The JSON artifact contains tool version, run profile, environment, benchmark specs, raw samples, summaries, diagnostics, quality, and comparisons. Unknown environment fields are explicit `"unknown"` or `null`.

Freshness-sensitive report tooling should group artifacts by `metadata.run_id`
when present. Older artifacts without run ids can still be consumed, but mixed
`latest.json` files from widely separated runs should be treated as stale.

## Public API Layout

Common benchmark files do not change: keep using root imports such as `stress`, `stress_main`, `black_box`, `StressContext`, `StressRunner`, `StressRunnerConfig`, `StressRunnerOptions`, and `RunProfile`.

Advanced imports moved out of the crate root. Result and schema types are under `cntryl_stress::artifact`, reporters and console formatting helpers are under `cntryl_stress::reporting`, and run gate helpers are under `cntryl_stress::runner`.

## Programmatic Runner

```rust
use cntryl_stress::{StressRunner, StressRunnerConfig};

let config = StressRunnerConfig::new().filter("write");

let mut runner = StressRunner::with_config("storage", config);
runner.run("write_batch", |ctx| {
    let batch = vec![0_u8; 4096];
    ctx.parameter("payload_size", batch.len());
    ctx.measure("write batch", || write_batch(&batch));
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
