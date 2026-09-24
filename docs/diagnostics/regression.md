# `regression`

**Default severity:** error

## What it means

The row regressed against the selected baseline: the primary metric moved past
the regression threshold and the 95% confidence intervals do not overlap.

## Why it matters

This is the core signal of a performance gate. Whether it fails the run
depends on the profile (`release` fails on meaningful regressions) and on
per-row `max_regression_pct`.

## Typical causes

- A real slowdown in the code under test.
- Host noise on shared CI runners (a one-off slow run).
- A changed environment (the baseline compatibility check rejects clearly
  different hosts).

## How to fix

Reproduce locally on the same row:

```bash
cargo stress --workload 'my_bench' --baseline latest
cargo stress compare \
  target/stress/baselines/<package>/latest/<suite>.json \
  target/stress/<package>/<suite>/latest.json
```

If it is noise, absorb one-off slow runs instead of loosening thresholds:

```bash
cargo stress --baseline latest --baseline-runs 5 --confirm-regressions 2
```

If it is real, fix the code. If it is an accepted trade-off, refresh the
baseline from `main` after merging. A per-row threshold can be set explicitly:

Before:

```rust
#[stress(tier = 2)]
fn compact(ctx: &mut StressContext) { /* ... */ }
```

After:

```rust
#[stress(tier = 2, max_regression_pct = 10)]
fn compact(ctx: &mut StressContext) { /* ... */ }
```

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code regression
STRESS_ALLOW_CODES=regression cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("regression")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied. It does **not** bypass the
correctness, budget, quality, or regression gates, which fail the run
independently of diagnostic gating.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain regression`.
