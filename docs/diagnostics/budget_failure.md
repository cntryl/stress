# `budget_failure`

**Default severity:** error

## What it means

An explicit budget from `#[stress(max_ns_per_op = ..., max_allocs_per_op = ...,
max_bytes_per_op = ..., max_rsd_pct = ...)]` failed, or an allocation budget
was configured but allocation tracking was unavailable.

## Why it matters

Budgets are promises you wrote down. A failed budget fails the run in every
profile.

## Typical causes

- The measured cost really grew past the budget.
- An allocation budget is set but the bench crate does not install
  `cntryl_stress::stress_allocator!()`, so allocations cannot be counted.

## How to fix

Read the evidence on the diagnostic (it names the budget, limit, and actual
value). Then reduce the cost, or intentionally update the budget.

Before (allocation budget without the tracking allocator):

```rust
use cntryl_stress::{black_box, stress, stress_main, StressContext};

#[stress(tier = 1, max_allocs_per_op = 0)]
fn hash_key(ctx: &mut StressContext) {
    ctx.measure("hash", || black_box(hash(b"key")));
}

stress_main!();
```

After:

```rust
use cntryl_stress::{black_box, stress, stress_main, StressContext};

cntryl_stress::stress_allocator!();

#[stress(tier = 1, max_allocs_per_op = 0)]
fn hash_key(ctx: &mut StressContext) {
    ctx.measure("hash", || black_box(hash(b"key")));
}

stress_main!();
```

See [allocation budgets](../bench-recipes.md#allocation-budgets).

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code budget_failure
STRESS_ALLOW_CODES=budget_failure cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("budget_failure")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied. It does **not** bypass the
correctness, budget, quality, or regression gates, which fail the run
independently of diagnostic gating.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain budget_failure`.
