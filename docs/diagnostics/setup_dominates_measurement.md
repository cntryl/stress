# `setup_dominates_measurement`

**Default severity:** error

## What it means

Timing overhead or setup dominates the measured work in at least one sample.

## Why it matters

When the timer and loop cost as much as the work, the number reflects the
harness, not your code.

## Typical causes

- Setup (cloning, building fixtures) inside the measured closure.
- Each iteration does too little work for its measurement method.

## How to fix

Move setup into `measure_with_setup` and batch enough work per sample.

Before:

```rust
#[stress(tier = 2)]
fn sort(ctx: &mut StressContext) {
    let input = (0_u64..1024).rev().collect::<Vec<_>>();
    ctx.measure("sort", || {
        let mut v = input.clone(); // setup timed with the work
        v.sort_unstable();
        black_box(v)
    });
}
```

After:

```rust
#[stress(tier = 2)]
fn sort(ctx: &mut StressContext) {
    let input = (0_u64..1024).rev().collect::<Vec<_>>();
    ctx.measure_with_setup("sort", || input.clone(), |mut v| {
        v.sort_unstable();
        black_box(v)
    });
}
```

For a very fast Tier 2 operation, use
`ctx.benchmark("name").operations_per_sample(n)` to batch independent
operations per sample.

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code setup_dominates_measurement
STRESS_ALLOW_CODES=setup_dominates_measurement cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("setup_dominates_measurement")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied. It does **not** bypass the
correctness, budget, quality, or regression gates, which fail the run
independently of diagnostic gating.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain setup_dominates_measurement`.
