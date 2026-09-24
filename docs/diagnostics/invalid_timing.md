# `invalid_timing`

**Default severity:** error

## What it means

At least one measured sample recorded zero or otherwise invalid timing.

## Why it matters

A zero-duration sample means nothing was timed, so every derived number
(ns/op, throughput, RSD) is meaningless. The row is untrustworthy.

## Typical causes

- The measured closure did no work.
- `record_external` was called with a zero `Duration`.

## How to fix

Measure exactly one non-empty workload for the row.

Before:

```rust
ctx.record_external("round trip", Duration::ZERO, 100);
```

After:

```rust
let report = run_external_harness();
ctx.record_external_outcome(
    "round trip",
    report.duration, // non-zero, measured by the harness
    LogicalUnit::new("request"),
    OperationOutcome::new(report.attempted, report.completed),
);
```

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code invalid_timing
STRESS_ALLOW_CODES=invalid_timing cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("invalid_timing")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied. It does **not** bypass the
correctness, budget, quality, or regression gates, which fail the run
independently of diagnostic gating.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain invalid_timing`.
