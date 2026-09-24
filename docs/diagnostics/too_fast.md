# `too_fast`

**Default severity:** warning

## What it means

Each sample finished within a few timer ticks, so timer resolution dominates.
The evidence cites the host's `timer_resolution_ns`.

## Why it matters

When a sample is a handful of ticks long, rounding in the clock is a large
fraction of the measurement.

## Typical causes

- A non-Tier-1 row measures a tiny operation once per sample.

## How to fix

Batch more logical work per sample, or move hot-path timing to Tier 1
(which calibrates micro timing).

Before:

```rust
#[stress(tier = 2)]
fn lookup(ctx: &mut StressContext) {
    ctx.measure("lookup", || black_box(map.get(&black_box(7))));
}
```

After:

```rust
#[stress(tier = 2)]
fn lookup(ctx: &mut StressContext) {
    ctx.benchmark("lookup")
        .operations_per_sample(10_000)
        .measure(|| black_box(map.get(&black_box(7))));
}
```

Or `#[stress(tier = 1)]` for a hot-path micro benchmark.

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code too_fast
STRESS_ALLOW_CODES=too_fast cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("too_fast")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain too_fast`.
