# `single_op_throughput`

**Default severity:** warning

## What it means

A throughput-tier row (Tier 3-6) completed exactly one operation in every
sample.

## Why it matters

A throughput of one op per window is a latency measurement in disguise; the
tier promises a rate.

## Typical causes

- `ctx.measure` used in a Tier 3+ benchmark.

## How to fix

Report the real number of logical operations, or move the single-operation
row to Tier 2.

Before:

```rust
#[stress(tier = 3)]
fn project(ctx: &mut StressContext) {
    ctx.measure("project", || project_all(&records));
}
```

After:

```rust
use cntryl_stress::{LogicalUnit, OperationOutcome};

#[stress(tier = 3)]
fn project(ctx: &mut StressContext) {
    ctx.measure_outcome("project", LogicalUnit::new("record"), || {
        let done = project_all(&records);
        OperationOutcome::new(records.len() as u64, done)
    });
}
```

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code single_op_throughput
STRESS_ALLOW_CODES=single_op_throughput cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("single_op_throughput")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain single_op_throughput`.
