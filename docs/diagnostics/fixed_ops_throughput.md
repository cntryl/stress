# `fixed_ops_throughput`

**Default severity:** warning

## What it means

A throughput-tier row (Tier 3-6) ran a fixed operation count per sample
instead of a fixed-duration window.

## Why it matters

Throughput rows are meant to answer "how much work completes in a window".
Fixed-op samples of throughput work vary in length and mix setup and tail
effects into the rate.

## Typical causes

- A throughput row configured with a fixed `operations_per_sample`-style
  shape.
- A fixed-op probe placed in a throughput suite.

## How to fix

Let the tier pick duration-based sampling, and move deliberately fixed-op
probes to their own row or tier.

Before:

```rust
#[stress(tier = 3)]
fn ingest(ctx: &mut StressContext) {
    ctx.measure_batch("ingest 1000", 1000, || ingest_n(1000));
}
```

After:

```rust
use cntryl_stress::{LogicalUnit, OperationOutcome};

#[stress(tier = 3)]
fn ingest(ctx: &mut StressContext) {
    ctx.measure_outcome("ingest", LogicalUnit::new("record"), || {
        let done = ingest_n(1000);
        OperationOutcome::new(1000, done)
    });
}

// A deliberate fixed-op probe stays, but is not a gate.
#[stress(tier = 2, role = "diagnostic")]
fn ingest_fixed_probe(ctx: &mut StressContext) {
    ctx.measure("ingest 1000", || ingest_n(1000));
}
```

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code fixed_ops_throughput
STRESS_ALLOW_CODES=fixed_ops_throughput cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("fixed_ops_throughput")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain fixed_ops_throughput`.
