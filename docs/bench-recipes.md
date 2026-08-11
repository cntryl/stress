# Bench Recipes

Pick the tier first, then make the measured work, logical operation, and
observed outcome obvious in the benchmark body. A row is **gate-worthy** when
it can affect a budget, release decision, or baseline comparison.

Three rules cover most authoring decisions:

1. Use `measure` for repeatable, non-destructive single operations.
2. Use `measure_with_setup` when an operation consumes or mutates its input.
3. Use the matching `measure_result*` method when an operation can return
   `Err`; it stops the repeated sample at the first error.
4. For gate-worthy batch, throughput, or externally timed work, name a
   `LogicalUnit` and report the observed `OperationOutcome`.

Rows default to `role = "gate"`. Mark shape checks and exploratory rows as
`role = "diagnostic"` or `role = "experimental"` so they do not create
authoritative suite obligations.

| Tier | Scope | Default recipe |
|------|-------|----------------|
| 1 | Hot path | `measure` or `measure_with_setup` for destructive inputs |
| 2 | Subsystem operation | Single-operation measurement; use `measure_outcome` for batches |
| 3 | System throughput | `measure_outcome("project", LogicalUnit::new("record"), ...)` |
| 4 | Integration throughput | `measure_outcome` or `record_external_outcome` with observed counters |
| 5 | Saturation/scaling | Observed outcomes across a real load or scale sweep |
| 6 | Soak/endurance | Observed outcomes across the declared, sustained soak window |

## Tier 1: Repeatable Hot Path

```rust
use cntryl_stress::{black_box, stress, StressContext};

#[stress(tier = 1, max_allocs_per_op = 0, max_bytes_per_op = 0)]
fn parse_header_hot_path(ctx: &mut StressContext) {
    let header = b"content-type:application/json";
    ctx.parameter("header_len", header.len());

    ctx.measure("separator lookup", || {
        black_box(header.iter().position(|byte| *byte == b':'))
    });
}
```

Tier 1 uses calibrated micro timing and reports `ns/op`. Keep the closure
small, deterministic, and observable. Rows below 5 ns/op are invalid by
default and rows below 15 ns/op receive a warning. Vary inputs and accumulate
observable outputs before using the reviewed-micro escape hatch:

```rust
#[stress(tier = 1, metadata(validated_micro = "true"))]
```

Use that exact metadata only after inspecting the optimized shape and ruling
out dead-code elimination. If the row intentionally measures
construction, parsing, or allocation, describe that intent explicitly:

```rust
#[stress(tier = 1, metadata(row_class = "parsing"))]
fn parse_header(ctx: &mut StressContext) {
    let header = b"content-type:application/json";
    ctx.measure("parse header", || {
        black_box(header.split(|byte| *byte == b':').count())
    });
}
```

## Destructive or State-Mutating Work

Do not reuse a progressively mutated fixture across measured iterations. Build
fresh input outside the timed interval with `measure_with_setup`; the returned
value is also dropped outside that interval.

```rust
use cntryl_stress::{black_box, stress, StressContext};
use std::collections::BTreeMap;

#[stress(tier = 2)]
fn insert_one_index_entry(ctx: &mut StressContext) {
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
```

Use this shape for inserts, sorts, parsers that consume buffers, request
builders, and any operation whose next invocation would otherwise see a
different state. The row-level `operations_per_sample` override batches a fast
Tier 2 operation into a stable timing sample without including fixture setup.

## Fallible Benchmark Functions

`#[stress]` functions may return `Result<(), E>` when `E: Display`, or the
crate's `StressResult`. Use `?` for fixture or benchmark-level failures instead
of `unwrap` or `expect`; the runner records the error as a structured,
untrustworthy benchmark row.

```rust
use cntryl_stress::{black_box, stress, StressContext};

#[stress(tier = 2)]
fn parse_checked_counter(ctx: &mut StressContext) -> Result<(), std::num::ParseIntError> {
    let value = ctx.measure_result("parse counter", || "42".parse::<u64>())?;
    black_box(value);
    Ok(())
}
```

A function-level `Result` describes whether the benchmark invocation itself
could run. `measure_result` and `measure_result_with_setup` stop at the first
operation error and record attempted, completed, and failed calls before
returning that error to the function. Their builder equivalents retain the
same behavior under `.operations_per_sample(n)`.

Do not pass a fallible closure to `measure`, `measure_with_setup`, or their
builder equivalents. Those methods are for infallible work and repeated modes
retain only the final return value. Use `OperationOutcome` instead when some
logical operations within one invocation may succeed while others fail.

## Gate-Worthy Batch and Throughput Work

Name the unit whose throughput is reported, then return counters observed while
the workload ran. Increment `completed` only after the operation actually
succeeds.

```rust
use cntryl_stress::{black_box, stress, LogicalUnit, OperationOutcome, StressContext};

#[stress(tier = 3)]
fn project_events(ctx: &mut StressContext) {
    let events = (0_u64..512).collect::<Vec<_>>();
    ctx.parameter("event_count", events.len());

    let outcome = ctx.measure_outcome(
        "project events",
        LogicalUnit::new("event"),
        || {
            let mut attempted = 0_u64;
            let mut completed = 0_u64;
            let mut validation_errors = 0_u64;
            let mut checksum = 0_u64;

            for event in &events {
                attempted += 1;
                if let Some(projected) = event.checked_mul(2) {
                    checksum ^= projected;
                    completed += 1;
                } else {
                    validation_errors += 1;
                }
            }

            black_box(checksum);
            OperationOutcome::new(attempted, completed)
                .validation_errors(validation_errors)
        },
    );
    black_box(outcome);
}
```

`OperationOutcome` can report `failures`, `timeouts`, `duplicates`, `dropped`,
and `validation_errors`. The framework aggregates the outcomes produced by all
measured invocations while excluding Tier 1 calibration calls.

The builder form keeps row-local overrides and facts together:

```rust
let outcome = ctx
    .benchmark("large fanout")
    .samples(20)
    .warmup(2)
    .parameter("client_count", client_count)
    .measure_outcome(LogicalUnit::new("request"), || run_fanout_once());
black_box(outcome);
```

Here `run_fanout_once()` returns the `OperationOutcome` it observed.

## Externally Timed Workload

When another runtime owns the clock, record both its duration and its observed
outcome. Allocation counters are unavailable because `cntryl-stress` did not
bracket the work.

```rust
use cntryl_stress::{stress, LogicalUnit, OperationOutcome, StressContext};
use std::time::Duration;

#[stress(tier = 4)]
fn remote_round_trip(ctx: &mut StressContext) {
    let report = run_external_harness();
    let outcome = OperationOutcome::new(report.attempted, report.completed)
        .failures(report.failures)
        .timeouts(report.timeouts);

    ctx.record_external_outcome(
        "remote round trip",
        Duration::from_nanos(report.elapsed_ns),
        LogicalUnit::new("request"),
        outcome,
    );
}
```

For a gate-worthy row, the external harness must observe the duration and every
counter. If a value is estimated, describe that limitation in metadata and keep
the row diagnostic.

## Async and Fallible Work

Async benchmarks need no Tokio dependency in `cntryl-stress`. The generated
entrypoint awaits the benchmark. Use `measure_async` for infallible futures and
`measure_result_async` for fallible futures.

```rust
use cntryl_stress::{black_box, stress, StressContext};

#[stress(tier = 2)]
async fn read_from_cache(ctx: &mut StressContext) -> Result<(), &'static str> {
    let value = ctx
        .measure_result_async("cache read", || async {
            Ok::<_, &'static str>(black_box(42_u64))
        })
        .await?;
    black_box(value);
    Ok(())
}
```

## Legacy Inferred-Success Helpers

`measure_batch("name", n, ...)` is a compatibility convenience. It infers that
all `n` logical operations completed successfully on every invocation. It
cannot observe partial failures, timeouts, drops, duplicates, or validation
errors. Likewise, `record_external("name", duration, n)` treats all `n`
operations as successful.

These helpers are acceptable for infallible diagnostic work. They are
unsuitable when partial failure is possible. Gate-worthy batch and external
rows must use `LogicalUnit` with `measure_outcome` or
`record_external_outcome`.

## Tier 5 and Tier 6 Evidence

A tier number does not create saturation or endurance evidence by itself.

- A real Tier 5 run sweeps a declared load or scale axis, holds other inputs
  stable, runs long enough to reach steady state, observes correctness and
  resource pressure, and identifies the throughput knee or saturation point.
- A real Tier 6 run sustains a representative workload for the declared soak
  duration and observes error rates, timeouts, resource drift, recovery, and
  throughput across the full wall-clock window.

Short synthetic loops may still document an API shape, but label them so they
cannot be mistaken for release evidence:

```rust
use cntryl_stress::{black_box, stress, LogicalUnit, OperationOutcome, StressContext};

#[stress(
    tier = 5,
    role = "diagnostic",
    metadata(evidence_scope = "short_synthetic")
)]
fn diagnostic_scale_shape(ctx: &mut StressContext) {
    let clients = 32_u64;
    ctx.parameter("client_count", clients);

    let outcome = ctx.measure_outcome(
        "client fanout shape",
        LogicalUnit::new("request"),
        || {
            let mut completed = 0_u64;
            for client in 0..clients {
                black_box(client.wrapping_mul(17));
                completed += 1;
            }
            OperationOutcome::new(clients, completed)
        },
    );
    black_box(outcome);
}
```

The repository demos use this role on their short Tier 5 and Tier 6 rows.

## Selecting Rows and Setting Run Policy

`--workload` is a glob over display names, Rust function names, module paths,
`module_path::function_name`, and `module_path::display_name`.

```bash
cargo bench --bench storage_stress -- --list
cargo bench --bench storage_stress -- --workload 'read_from_cache'
cargo bench --bench storage_stress -- --workload 'storage::cache::*'
cargo stress --bench storage_stress --workload '*cache*'
```

Selection is strict: an unmatched workload is a fatal error and prints nearby
registered candidates. Unknown flags, missing values, malformed values, and
invalid profiles are also rejected instead of being silently ignored. A
workspace wrapper run skips targets with no local match and executes every
target that does match, so target ordering cannot hide a valid selection.

Use a positive per-benchmark deadline to bound a stuck row:

```bash
cargo stress --bench storage_stress --timeout-secs 300
cargo bench --bench storage_stress -- --timeout-secs 300
```

For the Cargo subcommand, thresholds are percentage points. The direct harness
retains a fraction-valued compatibility flag:

```bash
cargo stress --baseline latest --threshold-percent 5
cargo bench --bench storage_stress -- --baseline latest --threshold 0.05
```

Create `latest` baselines only from passed runs with `--save-baseline`. Never
point an explicit baseline at the same output `latest.json` that the current run
will replace; the harness rejects that self-alias. Wrapper feature/target inputs
are part of environment compatibility. For a direct non-default Cargo build,
set a stable `STRESS_BUILD_INPUT_IDENTITY` consistently when saving and
comparing its baseline.

## Run Freshness

Set `STRESS_RUN_ID` when a larger script or CI job runs multiple benchmark
binaries and wants their artifacts treated as one generation. `cargo stress`
does this automatically for child binaries.

```bash
STRESS_RUN_ID=ci-run-20260706-001 cargo bench --bench storage_stress
```

## Anti-Patterns

- Mutating one fixture across iterations: use `measure_with_setup`.
- Setup inside the measured operation: return fresh input from the setup closure.
- Returning `Result` from `measure`, `measure_with_setup`, or `measure_async`:
  use the matching `measure_result*` API so the first error stops the sample.
- Inferred batch success where partial failure is possible: use
  `measure_outcome` with a `LogicalUnit` and observed counters.
- External timing with only a hoped-for operation count: use
  `record_external_outcome` with the harness's observed outcome.
- Random inputs without a fixed seed: use deterministic fixtures for
  release-quality rows.
- Treating a short loop as saturation or soak evidence: keep it diagnostic
  until the suite supplies the real scale or duration evidence.
- Comparing unlike tiers: compare Tier 1 hot paths to Tier 1 hot paths, not to
  Tier 4 integration rows.
- One-sample release gates: use `release` or enough measured samples for the
  quality class you need.
- Allocation budgets without `cntryl_stress::stress_allocator!()`: install the
  allocator in the benchmark crate.
- Allocation budgets while unrelated background threads are active: allocator
  counters are process-wide, so quiesce unrelated work or isolate the benchmark;
  allocations from workload-owned threads are intentionally part of the row.
- Reusing stale `latest.json` artifacts from different runs: group multi-suite
  reports by `STRESS_RUN_ID` or refresh the full suite before comparing.
