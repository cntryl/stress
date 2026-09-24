# `scaling_anomaly`

**Default severity:** info

## What it means

A parameter sweep scales anomalously: its primary value reverses direction
across the swept parameter, changes in a way no single power law explains, or
(for a `threads` sweep) loses parallel efficiency. Healthy sweeps, such as a
clean O(n) or O(n²) size sweep or near-linear thread scaling, emit nothing.

Rows are grouped exactly as the report's sweep tables group them: the same
benchmark name with the swept value removed, the same metric and unit, and
equal values for every other parameter. A group is evaluated when it has at
least 3 points whose parameter and primary value are positive and finite.
Two points differ significantly when they are more than 5% apart and their
95% confidence intervals do not overlap. The diagnostic fires on any of these
triggers:

- `non_monotonic`: significant differences go both up and down.
- `poor_fit`: the group changes significantly in one direction, but the
  least-squares fit of `ln(value)` on `ln(parameter)` has r² below 0.9. A
  power law then leaves more than 10% of the log variance unexplained; clean
  polynomial sweeps fit above 0.95 even with a few percent of noise, so a lower
  fit points at a knee or cliff rather than at noise. Over a smaller span a
  few small steps dominate the fit's shape, so the span guard keeps nearly
  flat sweeps silent.
- `low_thread_efficiency`: for a `threads` sweep of throughput rows, the
  parallel efficiency `T(n) / ((n / n0) * T(n0))` against the smallest thread
  count `n0` is below 0.5 at some evaluated point, even when computed from the
  upper bound of `T(n)`'s interval and the lower bound of `T(n0)`'s. Each added
  thread then buys less than half its ideal speedup. A base whose interval
  reaches zero or below is too noisy to judge and never fires. This also fires on a flat
  throughput sweep, where threads add nothing.

It is attached to every evaluated row of the group; a row swept over several
parameters can carry one per parameter. Evidence:

- `exponent` and `r_squared`: the least-squares slope of `ln(value)` on
  `ln(parameter)` and its fit quality. An exponent near 1 is linear, near 2 is
  quadratic, near 0 is flat.
- `pattern`: `monotonic_increasing`, `monotonic_decreasing`, `flat`, or
  `non_monotonic` when significant changes go both up and down.
- `triggers`: the comma-separated triggers above that fired.
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

- Lock contention, false sharing, or oversubscription in thread sweeps.
- Capacity cliffs where a working set leaves a cache level.

## How to fix

For `poor_fit`, find the sweep point where the value jumps and check the
working set against cache sizes. For thread sweeps, low efficiency at small
thread counts points at contention; keep thread counts at or below the
available parallelism. Investigate `non_monotonic` groups before trusting the
sweep.

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
