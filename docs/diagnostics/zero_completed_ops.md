# `zero_completed_ops`

**Default severity:** error

## What it means

At least one measured sample completed zero logical operations.

## Why it matters

A sample with no completed work has no rate and no per-op cost; the row is
untrustworthy.

## Typical causes

- The workload returned early (empty input, closed connection).
- Completed work was not recorded (for example `OperationOutcome::new(n, 0)`
  from a counter that was never incremented).

## How to fix

Record the work that completed, and make sure the fixture yields work in
every sample.

Before:

```rust
ctx.measure_outcome("consume", LogicalUnit::new("message"), || {
    let consumed = consumer.poll(); // empty after the first sample
    OperationOutcome::new(100, consumed)
});
```

After:

```rust
ctx.measure_outcome_with_setup(
    "consume",
    LogicalUnit::new("message"),
    || filled_consumer(100),
    |mut consumer| {
        let consumed = consumer.poll();
        OperationOutcome::new(100, consumed)
    },
);
```

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code zero_completed_ops
STRESS_ALLOW_CODES=zero_completed_ops cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("zero_completed_ops")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied. It does **not** bypass the
correctness, budget, quality, or regression gates, which fail the run
independently of diagnostic gating.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain zero_completed_ops`.
