# `flat_or_capped_throughput`

**Default severity:** warning

## What it means

Throughput is almost perfectly flat across samples while completed work is
effectively the same fixed amount each window.

## Why it matters

Real throughput under load wobbles. A perfectly flat rate usually means a
cap (rate limiter, fixed work per window, bounded queue) is being measured
instead of the system.

## Typical causes

- The workload does a fixed amount of work per sample regardless of the
  window.
- A rate limiter or capacity cap bounds completions.

## How to fix

Either make the row measure uncapped work, or declare it an intentional
capacity probe that is not a gate.

Before:

```rust
#[stress(tier = 3)]
fn publish(ctx: &mut StressContext) {
    ctx.measure_outcome("publish", LogicalUnit::new("message"), || {
        limiter.publish_up_to(1000) // always exactly 1000 per window
    });
}
```

After:

```rust
#[stress(tier = 3, role = "diagnostic", metadata(scenario = "rate_limited"))]
fn publish_capped(ctx: &mut StressContext) {
    ctx.measure_outcome("publish", LogicalUnit::new("message"), || {
        limiter.publish_up_to(1000)
    });
}
```

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code flat_or_capped_throughput
STRESS_ALLOW_CODES=flat_or_capped_throughput cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("flat_or_capped_throughput")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain flat_or_capped_throughput`.
