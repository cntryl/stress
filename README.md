# cntryl-stress

[![crates.io](https://img.shields.io/crates/v/cntryl-stress.svg)](https://crates.io/crates/cntryl-stress)
[![docs.rs](https://docs.rs/cntryl-stress/badge.svg)](https://docs.rs/cntryl-stress)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://github.com/cntryl/stress/blob/main/LICENSE)

Performance benchmarks for engineers who need trustworthy artifacts, not just
timing numbers.

`cntryl-stress` is an opinionated Rust benchmarking framework for performance
engineering loops. It keeps benchmark authoring low ceremony while producing
structured artifacts, diagnostics, and gates that can support real optimization
decisions.

The core question is simple: **can this benchmark row be trusted?**

> [!IMPORTANT]
> This `main` branch documents the unreleased 0.3 API. Until 0.3 is published,
> crates.io and docs.rs serve 0.2.x, whose authoring API is different. Use the
> Git dependency below to try the current API, or follow the
> [published 0.2.x documentation](https://docs.rs/cntryl-stress) when consuming
> the registry release.

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
- `fs2` serializes same-suite artifact publishers; durable transaction state lets the next publisher recover an interrupted generation before writing.
- `cntryl-stress-macros` and its proc-macro stack power the macro-first API, including async benchmark support and benchmark metadata.
- `clap`, `anyhow`, `syn`, and `toml` are limited to the optional `cli` feature used by the `cargo stress` wrapper, so ordinary benchmark builds do not compile that discovery and configuration-receipt graph.

## Quick Start

```toml
[dev-dependencies]
# Unreleased 0.3 API documented on this branch:
cntryl-stress = { git = "https://github.com/cntryl/stress" }
# After 0.3 is published, replace the Git dependency with:
# cntryl-stress = "0.3"

[[bench]]
name = "storage_stress"
path = "benches/storage_stress.rs"
harness = false
```

```rust
use cntryl_stress::{black_box, stress, stress_main, StressContext};
use std::collections::BTreeMap;

cntryl_stress::stress_allocator!();

#[stress(tier = 1, max_allocs_per_op = 0, max_bytes_per_op = 0)]
fn parse_route_hot_path(ctx: &mut StressContext) {
    let route = b"tenant-a.queue.primary.region-east-1.message-handler-v2.delivery-attempt-000042.customer-enterprise";
    ctx.measure("route hash", || {
        let mut hash = 5381_u64;
        for byte in black_box(route) {
            hash = hash.wrapping_mul(33).wrapping_add(u64::from(*byte));
        }
        black_box(hash)
    });
}

#[stress(tier = 2, metadata(component = "index"))]
fn insert_index_entry(ctx: &mut StressContext) {
    let initial_entries = 256_u64;
    ctx.parameter("initial_entries", initial_entries);

    ctx.benchmark("index insert")
        .operations_per_sample(256)
        .measure_with_setup(
            || {
                (0..initial_entries)
                    .map(|key| (key, key.rotate_left(5)))
                    .collect::<BTreeMap<_, _>>()
            },
            |mut index| {
                index.insert(initial_entries, initial_entries.rotate_left(5));
                black_box(index)
            },
        );
}

stress_main!();
```

Allocation tracking uses process-wide allocator counters. Keep unrelated
background work quiescent while enforcing per-operation allocation budgets;
allocations performed by workload-owned threads are intentionally included.

```bash
cargo bench --bench storage_stress
cargo bench --bench storage_stress -- --workload 'parse_route_hot_path'
```

The optional `cargo stress` wrapper is feature-gated so ordinary benchmark builds do not compile its CLI dependency graph:

```bash
cargo install --git https://github.com/cntryl/stress cntryl-stress --features cli
cargo stress
cargo stress --bench storage_stress --profile release --save-baseline
cargo stress --bench storage_stress --baseline latest
```

## Selection

`--workload` filters the registered benchmark set before execution. It matches
the display name, Rust function name, module path, `module_path::function_name`,
and `module_path::display_name`.

```bash
cargo bench --bench storage_stress -- --list
cargo bench --bench storage_stress -- --workload 'parse_route_hot_path'
cargo bench --bench storage_stress -- --workload 'queue::writer::*'
```

Selection is strict. When no row matches, stress exits unsuccessfully and
prints close registered candidates. A misspelled filter can never produce a
successful empty run. In workspace runs, `cargo stress` probes every selected
bench target, skips only targets with no local match, and runs every target that
does match; selection fails only when the pattern matches nowhere.

Wrapper discovery parses each declared Cargo bench source and attributes its
entrypoint to that package's declared `cntryl-stress` dependency. Qualified
calls, dependency renames, and direct, renamed, grouped, or glob imports of
`stress_main!` are supported. The entrypoint must be an unconditional top-level
invocation; nested, conditional, generated, or unattributed lookalikes are
rejected with the inspected source and supported forms instead of being run as
an unrelated executable.

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
- Tier 1 logical-outcome rows mark `ns_per_op_basis = logical_completed_operation` so
  baseline tools can treat changed ns/op semantics as a baseline refresh event,
  not as a performance regression.
- `metadata(row_class = "construction" | "parsing" | "allocation")` marks
  allocation-oriented rows so allocation diagnostics are advisory unless an
  explicit allocation budget fails.

Tier 1 rows below 5 ns/op are invalid by default because dead-code elimination
or measurement overhead can dominate them; rows below 15 ns/op receive a
warning. Vary inputs and accumulate an observable output first. Only after an
explicit anti-DCE review, opt in with the exact attribute syntax:

```rust
#[stress(tier = 1, metadata(validated_micro = "true"))]
```

## Tier Recipes

Pick the tier first, then use the matching timing shape. The detailed copy-paste guide is in the [benchmark recipes](https://github.com/cntryl/stress/blob/main/docs/bench-recipes.md).

| Tier | Scope | Recipe |
|------|-------|--------|
| 1 | Hot path | `measure`, or `measure_with_setup` for consumed input |
| 2 | Subsystem operation | Single-operation timing; `measure_outcome` for batches |
| 3 | System throughput | `measure_outcome` plus `LogicalUnit::new("record")` |
| 4 | Integration throughput | Observed `measure_outcome` or `record_external_outcome` |
| 5 | Saturation/scaling | Observed outcomes across a real scale or load sweep |
| 6 | Soak/endurance | Observed outcomes across the declared soak duration |

Tier 5 and Tier 6 describe evidence, not merely code shape. A real Tier 5 suite
must exercise a scale/load sweep long enough to locate sustained saturation. A
real Tier 6 suite must run a representative workload across its declared soak
window and observe correctness and resource drift. Mark short synthetic shape
examples with `role = "diagnostic"`; the repository demos do this explicitly.

## Benchmark API

Use `measure` for repeatable, non-destructive single operations:

```rust
#[stress(tier = 1)]
fn parse_document_header(ctx: &mut StressContext) {
    let document = load_document();
    ctx.parameter("payload_size", document.len());
    ctx.measure("parse header", || parse_header(&document));
}
```

Use `measure_with_setup` when an operation consumes or mutates input. The setup
closure creates fresh input outside the timed interval, and the measured output
is dropped outside it:

```rust
#[stress(tier = 2)]
fn sort_records(ctx: &mut StressContext) {
    let input = (0_u64..1024).rev().collect::<Vec<_>>();
    ctx.parameter("record_count", input.len());

    ctx.measure_with_setup("sort records", || input.clone(), |mut records| {
        records.sort_unstable();
        black_box(records)
    });
}
```

Benchmark functions may return `Result<(), E>` where `E: Display`, or
`StressResult`. Prefer `?` to panicking when fixture or benchmark-level setup
can fail:

```rust
#[stress(tier = 2)]
fn parse_checked_counter(ctx: &mut StressContext) -> Result<(), std::num::ParseIntError> {
    let value = ctx.measure_result("parse counter", || "42".parse::<u64>())?;
    black_box(value);
    Ok(())
}
```

`measure_result` and `measure_result_with_setup` stop on the first `Err` and
record the calls actually attempted, completed, and failed. The general
`measure` and `measure_with_setup` methods are for infallible work: repeated
modes retain only the final closure value, so passing a `Result` to them can
hide an earlier error.

For gate-worthy batch or throughput work, name the logical operation and return
the counters actually observed by the workload:

```rust
use cntryl_stress::{LogicalUnit, OperationOutcome};

#[stress(tier = 3)]
fn project_records(ctx: &mut StressContext) {
    let records = (0_u64..512).collect::<Vec<_>>();
    let outcome = ctx.measure_outcome(
        "project records",
        LogicalUnit::new("record"),
        || {
            let mut completed = 0_u64;
            for record in &records {
                black_box(record.rotate_left(7));
                completed += 1;
            }
            OperationOutcome::new(records.len() as u64, completed)
        },
    );
    black_box(outcome);
}
```

Use `record_external_outcome` when another harness owns both timing and
correctness observation:

```rust
#[stress(tier = 4)]
fn external_round_trip(ctx: &mut StressContext) {
    let report = run_external_harness();
    let outcome = OperationOutcome::new(report.attempted, report.completed)
        .failures(report.failures)
        .timeouts(report.timeouts);
    ctx.record_external_outcome(
        "round trip",
        report.duration,
        LogicalUnit::new("request"),
        outcome,
    );
}
```

Async benchmarks can also be fallible:

```rust
#[stress(tier = 2)]
async fn async_lookup(ctx: &mut StressContext) -> Result<(), &'static str> {
    let value = ctx
        .measure_result_async("lookup", || async {
            Ok::<_, &'static str>(black_box(42_u64))
        })
        .await?;
    black_box(value);
    Ok(())
}
```

`measure_batch("name", n, ...)` is a legacy convenience that infers all `n`
operations succeeded. It is unsuitable when partial failure, timeout, drop,
duplicate, or validation failure is possible. The same caveat applies to
`record_external("name", duration, n)`. Gate-worthy batch and external rows
must use `LogicalUnit` with `measure_outcome` or `record_external_outcome`.

Use the builder path when one row needs local run-shape overrides:

```rust
ctx.benchmark("large fanout")
    .samples(20)
    .warmup(2)
    .parameter("client_count", client_count)
    .measure_outcome(LogicalUnit::new("request"), || run_fanout_once());
```

Every named row emitted by one `#[stress]` function must use the same
`samples`, `warmup`, and `cooldown` overrides. Split differently shaped rows
into separate functions; otherwise extra invocations would create work that
cannot be represented honestly in every row.

Useful context methods:

```rust
ctx.parameter("client_count", 16);
ctx.metadata("scenario", "fanout");
ctx.record_latency(duration);
ctx.measure("name", || work());
ctx.measure_result("name", || fallible_work())?;
ctx.measure_with_setup("name", setup, |input| work(input));
ctx.measure_result_with_setup("name", setup, |input| fallible_work(input))?;
ctx.measure_outcome("name", LogicalUnit::new("request"), || observed_work());
ctx.measure_outcome_with_setup("name", LogicalUnit::new("request"), setup, |input| observed_work(input));
ctx.measure_async("name", || async { work().await }).await;
ctx.measure_result_async("name", || async { fallible_work().await }).await?;
ctx.measure_result_async_with_setup("name", setup, |input| async move { fallible_work(input).await }).await?;
ctx.measure_threaded("name", || work());
ctx.measure_pipeline("name", || work());
ctx.measure_io("name", || work());
ctx.record_external_outcome("name", duration, LogicalUnit::new("request"), outcome);
```

For a fast Tier 2 operation, use
`ctx.benchmark("name").operations_per_sample(n)` to batch enough independent
operations for a stable sample while retaining per-operation metrics.
Fallible builder rows use the matching explicit method, for example
`.operations_per_sample(n).measure_result(...)` or
`.measure_result_with_setup(...)`; they stop before a later success can hide
the first error.

## Attributes

```rust
#[stress]
#[stress(tier = 1)]
#[stress(tier = 4)]
#[stress(tier = 1, max_ns_per_op = 250, max_regression_pct = 5)]
#[stress(max_allocs_per_op = 0, max_bytes_per_op = 0, max_rsd_pct = 10)]
#[stress(name = "custom_name", ignore)]
#[stress(tier = 5, role = "diagnostic")]
#[stress(metadata(component = "queue", scenario = "fanout"))]
#[stress(metadata(row_class = "parsing"))]
```

Tiers are defined as 1 through 6. `role = "gate"` is the default. Use
`role = "diagnostic"` or `role = "experimental"` for rows that should not
create authoritative suite obligations. The macro rejects invalid tiers,
roles, attributes, and function signatures; benchmark functions take exactly
one `&mut StressContext` and return `()`, `Result<(), E>`, or `StressResult`.

Release and explicit quality/regression policies still require the *selected*
row set to contain at least one `gate` row. A filter that selects only
diagnostic or experimental rows therefore fails that policy rather than
vacuously passing a release gate; use `default`, `smoke`, or `lab` for a
diagnostic-only selection.

## Run Policy

| Profile | Default Samples | Gate Behavior |
|---------|-----------------|---------------|
| `default` | 5 measured, 1 warmup | Fails correctness, budgets, and invalid evidence; reports merely noisy rows |
| `smoke` | 1 measured, 0 warmup | Explicit diagnostic override; correctness-focused, no quality/regression failure |
| `lab` | 30 measured, 2 warmup, 1 cooldown | Exhaustive exploration; fails correctness, budgets, and invalid evidence |
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
| `STRESS_FILTER` | Benchmark name/module glob; an unmatched selection is fatal |
| `STRESS_TIER` | Exact tier filter, 1 through 6 |
| `STRESS_TIMEOUT_SECS` | Positive per-benchmark deadline in seconds |
| `STRESS_OUTPUT_DIR` | Artifact output directory |
| `STRESS_JSON` | Emit machine-readable JSON to stdout instead of the console table |
| `STRESS_INCLUDE_IGNORED` | Include ignored benchmarks |
| `STRESS_BASELINE` | Baseline stress artifact |
| `STRESS_BASELINE_DIR` | Baseline directory for `latest` and `--save-baseline` conventions |
| `STRESS_SAVE_BASELINE` | Save a passed run under the baseline directory |
| `STRESS_THRESHOLD` | Regression threshold as a fraction (`0.05` means 5%) |
| `STRESS_GIT_SHA` | Git SHA override |
| `STRESS_SAMPLE_DURATION_MS` | Fixed-duration sample budget |
| `STRESS_OPERATIONS_PER_SAMPLE` | Fixed-operations sample size |
| `STRESS_MICRO_SAMPLE_DURATION_MS` | Micro sample target duration |
| `STRESS_RUN_ID` | Run generation identity copied into artifact metadata |
| `STRESS_BUILD_INPUT_IDENTITY` | Advanced direct-run identity for non-default feature/target builds; the wrapper sets this automatically |
| `STRESS_FAIL_ON_ISSUES` | Fail on warning-or-error diagnostics |
| `STRESS_DENY_DIAGNOSTICS` | Fail on diagnostics at `info`, `warning`, or `error` |
| `STRESS_CONSOLE_NAMES` | Human console name mode: `compact` or `full` |
| `STRESS_PROGRESS` | Enable or disable stderr progress for human output |

Harness options:

```bash
cargo bench --bench storage_stress -- --tier 3 --workload '*fanout*'
cargo bench --bench storage_stress -- --samples 10 --warmup-samples 1
cargo bench --bench storage_stress -- --timeout-secs 300
cargo bench --bench storage_stress -- --operations-per-sample 64 --sample-duration-ms 500
cargo bench --bench storage_stress -- --profile release --save-baseline
cargo bench --bench storage_stress -- --baseline latest --threshold 0.05
cargo bench --bench storage_stress -- --print-config
```

Prefer the Cargo wrapper's explicit percentage-points spelling:

```bash
cargo stress --bench storage_stress --timeout-secs 300
cargo stress --bench storage_stress --operations-per-sample 64 --sample-duration-ms 500
cargo stress --baseline latest --threshold-percent 5
```

An explicit baseline file applies to exactly one benchmark target, so select it
with `--bench` (and `--package` when needed). Use `--baseline latest` for a
multi-target or workspace run: the wrapper resolves each suite independently.
Direct runs resolve and save `latest` under
`{baseline_dir}/{latest|timestamp}/{suite}.json`; wrapper runs add the package
namespace under
`{baseline_dir}/{package}/{latest|timestamp}/{suite}.json` to prevent packages
with the same bench name from overwriting each other.

Do not use the current output artifact
`{output_dir}/{suite}/latest.json` as an explicit baseline: that path is
overwritten by the run being evaluated and is rejected before measurement.
Keep accepted evidence under `--baseline-dir`, create it with
`--save-baseline`, and select it with `--baseline latest`.
Saving is strict: the run gate must pass, at least one intended gate row must be
present, and every intended gate must retain gate trust with acceptable-or-better
quality. Smoke, noisy, invalid, and diagnostic-only runs are rejected before
either the timestamped or `latest` baseline is changed.

The main artifacts are published first, followed by a requested baseline. Only
then does the run emit its final human result or single JSON stdout receipt. A
publication failure is recorded in `metadata.reporter_errors`, evaluates as
`ArtifactFailed`, and exits unsuccessfully. An earlier run-gate failure skips
baseline publication and remains the reported gate.

Baseline comparisons require known, matching CPU, logical core count, operating
system and architecture, allocator, build profile/input identity, Rust compiler, and tool
version. A missing or changed identity rejects the comparison with a baseline
refresh explanation instead of treating unlike environments as evidence.

The wrapper also accepts Cargo-native `--features`, `--all-features`,
`--no-default-features`, `--target <TRIPLE>`, and `--target-dir`. Target
selection is forwarded to metadata, build, and execution, so Cargo's
`target.<triple>.runner` configuration is honored. Repeatable
`--cargo-arg <ARG>` accepts only Cargo's non-resolution-changing controls:
locked/offline/frozen mode, jobs, verbosity/quiet, color, timings, and
ignore-rust-version. Each value is one exact argument and is never shell-split;
scope, profile, feature, target, config, unstable, and positional escapes are
rejected.

Build profile, feature mode/list, target, and conservative Rust/Cargo build-input
receipts are recorded as compatibility identity so unlike binaries cannot
silently share a baseline. The ambient receipt covers Rust flags, active DEV or
BENCH/RELEASE profile overrides, compiler and wrapper selection, build target
and incremental controls, plus target-specific Rust flags, linker, and runner.
The config-input receipt covers the workspace-root manifest's `[profile]`
section and relevant `build`, `target`, `profile`, `unstable`, and material
`env` inputs from Cargo config files consulted from the working-directory
hierarchy and Cargo home, including recursively included configs. It is
intentionally conservative: a tracked source change can require a baseline
refresh even when another Cargo source overrides it. Source roles, variable
names, and deterministic value fingerprints are recorded; absolute config
paths, raw values, secrets, unrelated sections/variables, and inactive ambient
profile overrides are not. The wrapper derives this identity automatically. If
you invoke `cargo bench` directly with non-default build inputs, set the same stable
`STRESS_BUILD_INPUT_IDENTITY` for baseline creation and comparison; for direct
artifacts intended for later wrapper comparison, prefer creating them through
`cargo stress` so the identity is canonical.
This is a fail-closed compatibility guard, not proof that two binaries are
semantically identical; requiring an extra baseline refresh is preferred to
comparing builds whose relevant inputs are unknown.

The benchmark binary parser is strict: unknown flags, missing values, malformed
CLI or `STRESS_*` values, invalid profiles, and unmatched workload selections
fail the command. This prevents a typo from weakening policy or producing a
plausible-looking successful run. Wrapper child output is consolidated after
each binary completes; it does not currently stream live progress.

Console output:

```bash
cargo bench --bench storage_stress
cargo bench --bench storage_stress -- --json
```

`cargo bench --bench ...` uses one console format: one simple benchmark table per suite with `benchmark`, `measurement`, `value`, `p50`, `p95`, `p99`, `rsd`, `trust`, and `mode` columns. Suite-local `issues` appear directly after a table only when a row needs attention, and the run ends with one `result:` line. Use `--json` only for machine-readable stdout.

## Artifacts

Direct `cargo bench` runs write under `target/stress/{suite}/`. The Cargo
wrapper keeps the same canonical suite and benchmark IDs, but avoids package
collisions by writing under `target/stress/{package}/{suite}/`:

- `{timestamp}.json` and `latest.json`
- `{timestamp}.txt` and `latest.txt`
- `{timestamp}.md` and `latest.md`

All six files are staged and synced before publication. A durable transaction
manifest distinguishes a commit in progress from a fully committed generation.
Detected failures roll back immediately, restoring the previous `latest` set
and removing the new timestamp set. If the process stops mid-publication, the
next same-suite publisher acquires the advisory lock and rolls back uncommitted
state before writing; committed cleanup debris is removed without discarding
the completed generation. A rollback failure remains fatal and preserves the
transaction state for diagnosis.

Same-suite publishers serialize through the persistent hidden
`.artifact-publication.lock`; its operating-system lock is released when the
process exits. Run history uses collision-resistant epoch/PID/sequence stems and
is create-only, so an unexpected timestamp collision fails instead of
overwriting evidence. The lock and transient `.artifact-transaction.*` or
`.artifact-committed.*` directories are coordination and recovery state, not
public artifacts.

Programmatic suite names are portable path components: ASCII letters, digits,
`.`, `-`, and `_` only. The wrapper also rejects Cargo targets such as
`same-name` and `same_name` when they would canonicalize to the same suite.

The JSON artifact contains tool version, run profile, environment, benchmark specs, raw samples, summaries, diagnostics, quality, and comparisons. Unknown environment fields are explicit `"unknown"` or `null`.
The checked-in [cntryl-stress.v2 JSON Schema](https://github.com/cntryl/stress/blob/main/core/schema/cntryl-stress.v2.schema.json)
is also available to Rust tooling as
`cntryl_stress::artifact::ARTIFACT_JSON_SCHEMA`.

Freshness-sensitive report tooling should group artifacts by `metadata.run_id`
when present. Older artifacts without run ids can still be consumed, but mixed
`latest.json` files from widely separated runs should be treated as stale.

## Public API Layout

Common benchmark files use root imports such as `stress`, `stress_main`,
`black_box`, `StressContext`, `LogicalUnit`, `OperationOutcome`, `StressError`,
`StressResult`, `BenchmarkRole`, `StressRunner`, `StressRunnerConfig`,
`StressRunnerOptions`, and `RunProfile`.

Advanced imports moved out of the crate root. Run artifacts and schema types are
under `cntryl_stress::artifact`, reporters and console formatting helpers are
under `cntryl_stress::reporting`, and run gate helpers are under
`cntryl_stress::runner`.

## Migrating from 0.2 to 0.3

- Rename `#[stress_test]` functions to `#[stress]`.
- Register each benchmark target with `harness = false` and end its source file
  with `cntryl_stress::stress_main!()`.
- Replace inferred-success batch/external gates with `LogicalUnit` plus
  `measure_outcome` or `record_external_outcome` and observed
  `OperationOutcome` counters.
- Replace fallible closures passed to `measure`, `measure_with_setup`, or
  `measure_async` with `measure_result`, `measure_result_with_setup`, or
  `measure_result_async` so repeated calls cannot hide an earlier error.
- Fix scripts that depended on ignored arguments or successful empty filters:
  0.3 rejects unknown or malformed flags and treats unmatched selections as
  failures.
- Refresh baselines. Current artifacts use schema v2 and current logical-unit,
  correctness, and summary semantics; old artifacts are not an apples-to-apples
  regression baseline.

## Programmatic Runner

```rust
use cntryl_stress::{black_box, StressRunner, StressRunnerConfig};

let config = StressRunnerConfig::new().filter("parse");

let mut runner = StressRunner::with_config("storage", config);
runner.run("parse_counter", |ctx| -> Result<(), std::num::ParseIntError> {
    let value = ctx.measure_result("parse counter", || "42".parse::<u64>())?;
    black_box(value);
    Ok(())
});
let run = runner.finish();
```

## Development

```bash
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings -D clippy::pedantic
cargo test --locked --workspace --all-targets --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps
```

## License

Apache License 2.0. See the repository [LICENSE](https://github.com/cntryl/stress/blob/main/LICENSE).
