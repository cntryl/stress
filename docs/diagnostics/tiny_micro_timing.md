# `tiny_micro_timing`

**Default severity:** warning

## What it means

A Tier 1 row measured between 5 and 15 ns/op without the reviewed-micro
opt-in. (Below 5 ns/op it is `likely_optimized_away` instead.)

## Why it matters

At a few nanoseconds, timer resolution and loop overhead are a large share of
the number, so small regressions are hard to separate from noise.

## Typical causes

- The operation is genuinely tiny (one comparison, one hash round).

## How to fix

Measure more logical work per call, or declare the row a diagnostic after
validating its shape.

Before:

```rust
#[stress(tier = 1)]
fn byte_eq(ctx: &mut StressContext) {
    ctx.measure("eq", || black_box(black_box(b'a') == black_box(b'b')));
}
```

After:

```rust
#[stress(tier = 1)]
fn slice_eq(ctx: &mut StressContext) {
    let left = [7_u8; 256];
    let right = [7_u8; 256];
    ctx.parameter("len", left.len());
    ctx.measure("eq 256 bytes", || black_box(black_box(&left[..]) == black_box(&right[..])));
}
```

Or keep the tiny row but out of the gate set:
`#[stress(tier = 1, role = "diagnostic")]`. After an anti-DCE review,
`metadata(validated_micro = "true")` also clears it.

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code tiny_micro_timing
STRESS_ALLOW_CODES=tiny_micro_timing cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("tiny_micro_timing")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain tiny_micro_timing`.
