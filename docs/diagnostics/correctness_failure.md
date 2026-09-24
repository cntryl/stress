# `correctness_failure`

**Default severity:** error

## What it means

The row recorded failures, timeouts, duplicates, dropped results, validation
errors, or fewer completed than attempted operations.

## Why it matters

A performance number for work that did not complete correctly is not a
performance number. The row is untrustworthy and the run fails.

## Typical causes

- The workload returned errors that `measure_result*` or `OperationOutcome`
  recorded.
- Timeouts or drops under load.
- A fixture bug makes validation fail.

## How to fix

Fix the workload first; the counters are in the row's evidence and the JSON
artifact. Report the outcome you actually observed rather than assuming
success.

Before (errors are hidden by an assumed-success helper):

```rust
ctx.measure_batch("send", 100, || client.send_many(&messages));
```

After:

```rust
use cntryl_stress::{LogicalUnit, OperationOutcome};

ctx.measure_outcome("send", LogicalUnit::new("message"), || {
    let report = client.send_many(&messages);
    OperationOutcome::new(report.attempted, report.completed)
        .failures(report.failures)
        .timeouts(report.timeouts)
});
```

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code correctness_failure
STRESS_ALLOW_CODES=correctness_failure cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("correctness_failure")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied. It does **not** bypass the
correctness, budget, quality, or regression gates, which fail the run
independently of diagnostic gating.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain correctness_failure`.
