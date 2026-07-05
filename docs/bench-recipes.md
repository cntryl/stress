# Bench Recipes

Pick the tier first, then use the matching timing recipe. The tier should make
the benchmark body obvious.

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

## Anti-Patterns

- Setup inside the measured closure: build deterministic fixtures before `ctx.measure*`.
- Uncounted batch work: use `ctx.measure_batch("name", n, ...)` or `ctx.record_external("name", duration, n)`.
- Random inputs without a fixed seed: use deterministic fixtures for release-quality rows.
- Comparing unlike tiers: compare Tier 1 hot paths to Tier 1 hot paths, not to Tier 4 integration rows.
- One-sample release gates: use `release` or enough measured samples for the quality class you need.
- Allocation budgets without `cntryl_stress::stress_allocator!()`: install the allocator in the benchmark crate.
