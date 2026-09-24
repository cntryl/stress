# `scaling_anomaly`

**Default severity:** info

## What it means

A parameter sweep's primary value changes measurably with the swept
parameter, or reverses direction across it.

Rows are grouped exactly as the report's sweep tables group them: the same
benchmark name with the swept value removed, the same metric and unit, and
equal values for every other parameter. A group is evaluated when it has at
least 3 points with a positive parameter value. The diagnostic fires when two
points differ by more than 5% and their 95% confidence intervals do not
overlap. It is attached to every row of the group, with this evidence:

- `exponent` and `r_squared`: the least-squares slope of `ln(value)` on
  `ln(parameter)` and its fit quality. An exponent near 1 is linear, near 2 is
  quadratic, near 0 is flat.
- `pattern`: `monotonic_increasing`, `monotonic_decreasing`, or
  `non_monotonic` when significant changes go both up and down.
- For a `threads` sweep: `available_parallelism`, the thread counts skipped
  because they exceed it (`skipped_above_parallelism`), and for throughput
  rows `thread_efficiency`, `T(n) / ((n / n0) * T(n0))` against the smallest
  thread count `n0`.
- `related_diagnostics`: `flat_or_capped_throughput`, `fixed_ops_throughput`,
  or scheduler-sensitive `high_variance` already reported on rows of the
  group.

The diagnostic never changes measurements and is computed only for the run
that produced it; baselines saved without it stay valid.

## Why it matters

The sweep table shows speedup and efficiency, but not whether the change is
larger than noise or which complexity it follows. A reversal usually points
at contention, a cache cliff, or an unstable measurement.

## Typical causes

- Work that grows with input size (the expected case for size sweeps).
- Lock contention, false sharing, or oversubscription in thread sweeps.
- Capacity cliffs where a working set leaves a cache level.

## How to fix

Compare the exponent with the expected complexity. For thread sweeps, low
efficiency at small thread counts points at contention; keep thread counts at
or below the available parallelism. Investigate `non_monotonic` groups before
trusting the sweep.

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics info --allow-code scaling_anomaly
STRESS_ALLOW_CODES=scaling_anomaly cargo stress --deny-diagnostics info
```

Programmatic runners use `StressRunnerConfig::new().allow_code("scaling_anomaly")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain scaling_anomaly`.
