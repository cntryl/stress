# `too_few_samples`

**Default severity:** warning

## What it means

The row has fewer than five measured samples.

## Why it matters

Confidence intervals and RSD from two or three samples are unreliable, so
the row cannot support a regression decision.

## Typical causes

- The `smoke` profile (1 sample) or a small `--samples` override.
- A per-row `.samples(n)` builder override below five.

## How to fix

Collect at least five measured samples for gate-quality rows; use the
`release` profile (10 samples) for gates.

Before:

```bash
cargo stress --samples 2 --baseline latest
```

After:

```bash
cargo stress --profile release --baseline latest
```

In code:

```rust
ctx.benchmark("merge").samples(10).measure(|| merge(&a, &b));
```

This code is expected in quick local loops (`--profile smoke`); allow it
there rather than raising sample counts.

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code too_few_samples
STRESS_ALLOW_CODES=too_few_samples cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("too_few_samples")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain too_few_samples`.
