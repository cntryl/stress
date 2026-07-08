# Bench Recipes

Pick the tier first, then use the matching timing recipe. A good stress
benchmark makes the measured work, logical operation count, and benchmark row
identity obvious from the body.

These recipes are intentionally prescriptive. They keep benchmark rows
comparable across runs and make diagnostics actionable when a row is noisy,
untrustworthy, or measuring the wrong unit of work.

| Tier | Scope | Default Recipe |
|------|-------|----------------|
| 1 | Hot path | `ctx.measure("parse", || hot_path())` |
| 2 | Subsystem operation | `ctx.measure("write", || one_operation())` or `ctx.measure_batch("flush", n, || batch())` |
| 3 | System throughput | `#[stress(tier = 3)]` plus `ctx.measure_batch("project", n, || batch())` |
| 4 | Integration throughput | Fixed-duration `ctx.measure_batch("round trip", n, ...)` or `ctx.record_external("round trip", duration, n)` |
| 5 | Saturation/scaling | Fixed-duration `ctx.measure_batch("fanout", n, ...)` with scale parameters such as client or shard count |
| 6 | Soak/endurance | Fixed-duration `ctx.measure_batch("churn", n, ...)` or `ctx.record_external("soak", duration, n)` over the soak window |

## Tier 1: Hot Path

```rust
use cntryl_stress::{black_box, stress, StressContext};

#[stress(tier = 1, max_allocs_per_op = 0, max_bytes_per_op = 0)]
fn parse_header_hot_path(ctx: &mut StressContext) {
    let header = b"content-type:application/json";
    ctx.parameter("header_len", header.len());

    ctx.measure("separator lookup", || black_box(header.iter().position(|byte| *byte == b':')));
}
```

Tier 1 uses calibrated micro timing and reports `ns/op` as the primary metric.
Keep the closure small and deterministic. If the row intentionally measures
parsing, construction, or allocation behavior, add explicit row context so
allocation diagnostics are interpreted correctly:

```rust
use cntryl_stress::{black_box, stress, StressContext};

#[stress(tier = 1, metadata(row_class = "parsing"))]
fn parse_header(ctx: &mut StressContext) {
    let header = b"content-type:application/json";
    ctx.measure("parse header", || {
        black_box(header.split(|byte| *byte == b':').count())
    });
}
```

## Tier 2: Single Operation

```rust
use cntryl_stress::{stress, StressContext};

#[stress(tier = 2)]
fn write_one_batch(ctx: &mut StressContext) {
    let batch = build_batch();
    ctx.parameter("payload_size", batch.len());

    ctx.measure("write batch", || write_batch(&batch));
}
```

If one measured subsystem call returns logical work completed by a batch, count
that logical work explicitly:

```rust
#[stress(tier = 2)]
fn flush_ready_entries(ctx: &mut StressContext) {
    let ready = ready_entry_count();
    let _completed = ctx.measure_batch("flush ready entries", ready, || flush_ready_entries_once());
}
```

`measure_batch` returns the completed logical operation count. For Tier 1 batch
rows, stress records `ns_per_op_basis = logical_completed_operation` in summary
metadata so baseline tools do not compare old iteration-based rows to new
logical-operation rows as if they were the same measurement.

## Tiers 3-6: Batched Throughput

```rust
use cntryl_stress::{black_box, stress, StressContext};

#[stress(tier = 3)]
fn project_events(ctx: &mut StressContext) {
    let events = load_events();
    ctx.parameter("event_count", events.len());

    let completed = ctx.measure_batch("project events", events.len() as u64, || {
        for event in &events {
            black_box(project(event));
        }
    });
    black_box(completed);
}
```

## Externally Timed Workload

Use this when another runtime or harness owns the timing window. Allocation
counters remain unavailable because `cntryl-stress` did not bracket the work.

```rust
use cntryl_stress::{stress, StressContext};
use std::time::Duration;

#[stress(tier = 4)]
fn remote_round_trip(ctx: &mut StressContext) {
    let report = run_external_harness();

    ctx.record_external(
        "remote round trip",
        Duration::from_nanos(report.elapsed_ns),
        report.completed_operations,
    );
}
```

Use `record_external` only when the external harness already knows both elapsed
time and completed logical operations. If either value is approximate, record
that context as metadata and avoid using the row as a release gate.

## Async Work

Async measurements do not require a Tokio dependency in `cntryl-stress`; the
benchmark function may be async and the measured future is awaited by
`ctx.measure_async`.

```rust
use cntryl_stress::{stress, StressContext};

#[stress(tier = 2)]
async fn read_from_cache(ctx: &mut StressContext) {
    ctx.measure_async("cache read", || async { read_once().await })
        .await;
}
```

## Selecting Rows

`--workload` can match the display name, Rust function name, module path,
`module_path::function_name`, or `module_path::display_name`.

```bash
cargo bench --bench storage_stress -- --workload 'read_from_cache'
cargo bench --bench storage_stress -- --workload 'storage::cache::*'
```

If nothing matches, stress prints close registered candidates so a typo does not
look like an empty benchmark suite.

## Run Freshness

Set `STRESS_RUN_ID` when a larger script or CI job runs multiple benchmark
binaries and wants their artifacts treated as one generation. The optional
`cargo stress` wrapper does this automatically for child binaries.

```bash
STRESS_RUN_ID=ci-run-20260706-001 cargo bench --bench storage_stress
```

## Anti-Patterns

- Setup inside the measured closure: build deterministic fixtures before `ctx.measure*`.
- Uncounted batch work: use `ctx.measure_batch("name", n, ...)` or `ctx.record_external("name", duration, n)`.
- Random inputs without a fixed seed: use deterministic fixtures for release-quality rows.
- Comparing unlike tiers: compare Tier 1 hot paths to Tier 1 hot paths, not to Tier 4 integration rows.
- One-sample release gates: use `release` or enough measured samples for the quality class you need.
- Allocation budgets without `cntryl_stress::stress_allocator!()`: install the allocator in the benchmark crate.
- Treating construction, parsing, or allocation rows as accidental allocation
  regressions: add `metadata(row_class = "...")`, then use explicit allocation
  budgets when allocation count is a gate.
- Reusing stale `latest.json` artifacts from different runs: group multi-suite
  reports by `STRESS_RUN_ID` or refresh the full suite before comparing.
