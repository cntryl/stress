# `high_variance`

**Default severity:** warning

## What it means

Measured samples varied by more than 10% relative standard deviation (RSD).

## Why it matters

Noisy rows cannot support tight regression decisions: confidence intervals
widen and quality drops to `noisy`, which the `release` profile fails.

## Typical causes

- Sample windows that are too short.
- One-off setup (cold caches, lazy init) inside the first samples.
- I/O, background load, or scheduler contention (common on shared CI
  runners).

## How to fix

Make each sample do the same work on the same input and keep setup out of
the timed region. More samples help the estimate, not the noise.

Before:

```rust
#[stress(tier = 2)]
fn query(ctx: &mut StressContext) {
    ctx.measure("query", || {
        let db = open_db(); // setup inside the timed work
        black_box(db.query(random_key()))
    });
}
```

After:

```rust
#[stress(tier = 2)]
fn query(ctx: &mut StressContext) {
    let db = open_db();
    let key = 42_u64; // deterministic fixture
    ctx.benchmark("query")
        .warmup(2)
        .samples(20)
        .measure(|| black_box(db.query(black_box(key))));
}
```

On noisy hosts, prefer `--baseline-runs`/`--confirm-regressions` (see
[ci.md](../ci.md)) over widening thresholds, or set an explicit
`max_rsd_pct` budget for the row.

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code high_variance
STRESS_ALLOW_CODES=high_variance cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("high_variance")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain high_variance`.
