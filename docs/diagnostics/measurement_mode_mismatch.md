# `measurement_mode_mismatch`

**Default severity:** warning

## What it means

Sibling rows in the same workload family mix fixed-duration windows with
fixed operation counts.

## Why it matters

Rows in one family are read side by side; different sampling semantics make
that comparison misleading.

## Typical causes

- One row in a family was written with a fixed-op helper while its siblings
  use duration-based throughput.

## How to fix

Use one measurement shape per family, or split the odd row out as an
explicit diagnostic.

Before:

```rust
#[stress(tier = 3)]
fn queue_throughput(ctx: &mut StressContext) {
    ctx.measure_outcome("queue_push_1_producer", LogicalUnit::new("message"), push_1);
    ctx.measure_batch("queue_push_4_producer", 4000, push_4_fixed);
}
```

After:

```rust
#[stress(tier = 3)]
fn queue_throughput(ctx: &mut StressContext) {
    ctx.measure_outcome("queue_push_1_producer", LogicalUnit::new("message"), push_1);
    ctx.measure_outcome("queue_push_4_producer", LogicalUnit::new("message"), push_4);
}
```

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code measurement_mode_mismatch
STRESS_ALLOW_CODES=measurement_mode_mismatch cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("measurement_mode_mismatch")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain measurement_mode_mismatch`.
