# `measurement_drift`

**Default severity:** info

## What it means

Measured samples trend steadily up or down over the run instead of scattering
around one level.

It is evaluated only when a row has at least 10 measured samples and at least
2 warmup samples. It fires when both hold:

- The Theil-Sen slope over measured samples in execution order has a Sen
  rank-based 99.9% confidence interval (from the Kendall tau variance
  `n(n-1)(2n+5)/18`) that excludes zero.
- The total drift over the run, `slope x (n - 1)`, exceeds 10% of the measured
  median.

The evidence records the slope (metric units per sample), its interval, and
the total drift in percent. The diagnostic never changes measurements.

## Why it matters

A trending row has no single steady value; the summary depends on how long the
run lasted.

## Typical causes

- State that accumulates across samples: growing collections, fragmentation,
  leaks, or caches that never reset.
- Thermal throttling or background load that changes during the run.

## How to fix

Reset per-sample state in setup so every sample does the same work:

Before:

```rust
let mut map = HashMap::new();
ctx.measure("insert", || map.insert(next_key(), 1));
```

After:

```rust
ctx.measure_with_setup("insert", HashMap::new, |mut map| map.insert(next_key(), 1));
```

If the state is intentional, check `environment.observations` for thermal or
load problems before trusting the row.

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics info --allow-code measurement_drift
STRESS_ALLOW_CODES=measurement_drift cargo stress --deny-diagnostics info
```

Programmatic runners use `StressRunnerConfig::new().allow_code("measurement_drift")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain measurement_drift`.
