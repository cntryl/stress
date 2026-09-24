# `non_finite_samples_dropped`

**Default severity:** warning (error above 10% of samples)

## What it means

Some metric values were infinite or NaN and were dropped before statistics
were computed. It escalates to an error when more than 10% of samples are
affected.

## Why it matters

Dropped samples shrink the evidence behind the summary; many dropped samples
mean the summary no longer describes the run.

## Typical causes

- Zero-length timings producing an infinite rate.
- Overflowing counters.

## How to fix

Make every sample measure non-empty work so each metric is finite.

Before:

```rust
ctx.measure_outcome("drain", LogicalUnit::new("message"), || {
    let drained = queue.drain_ready(); // often 0 and instant
    OperationOutcome::success(drained)
});
```

After:

```rust
ctx.measure_outcome_with_setup(
    "drain",
    LogicalUnit::new("message"),
    || filled_queue(1024),
    |mut queue| {
        let drained = queue.drain_ready();
        OperationOutcome::new(1024, drained)
    },
);
```

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code non_finite_samples_dropped
STRESS_ALLOW_CODES=non_finite_samples_dropped cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("non_finite_samples_dropped")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain non_finite_samples_dropped`.
