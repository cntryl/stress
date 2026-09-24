# `likely_optimized_away`

**Default severity:** warning

## What it means

A Tier 1 row measured below 5 ns/op without the reviewed-micro opt-in.

## Why it matters

At that speed the compiler has very likely removed the work (dead-code
elimination or constant folding), so the row measures an empty loop. Tier 1
rows this fast are treated as invalid evidence by default.

## Typical causes

- The input is a constant the compiler can see through.
- The result is unused, so the computation is dropped.

## How to fix

Pass inputs through `black_box`, vary them, and make the output observable.

Before:

```rust
#[stress(tier = 1)]
fn add(ctx: &mut StressContext) {
    ctx.measure("add", || 2_u64 + 2);
}
```

After:

```rust
use cntryl_stress::black_box;

#[stress(tier = 1)]
fn add(ctx: &mut StressContext) {
    let inputs = [3_u64, 5, 7, 11];
    let mut i = 0;
    ctx.measure("add", || {
        i = (i + 1) % inputs.len();
        black_box(black_box(inputs[i]) + black_box(2))
    });
}
```

Only after inspecting the optimized code and ruling out DCE, opt in with
exactly:

```rust
#[stress(tier = 1, metadata(validated_micro = "true"))]
```

See the [anti-DCE recipe](../bench-recipes.md#anti-dce-with-black_box).

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code likely_optimized_away
STRESS_ALLOW_CODES=likely_optimized_away cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("likely_optimized_away")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain likely_optimized_away`.
