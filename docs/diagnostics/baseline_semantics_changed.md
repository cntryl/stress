# `baseline_semantics_changed`

**Default severity:** warning

## What it means

The row exists in both the baseline and the current run, but it measures
something different: its tier, measurement mode, logical unit, parameters, or
measurement intent changed, so the numbers are not comparable.

## Why it matters

Comparing a row whose question changed would report a fake regression or
improvement. The row is left out of the comparison instead.

## Typical causes

- The benchmark moved to another tier or measurement method.
- A `LogicalUnit` was added or renamed.
- A `parameter` value (for example an input size) changed.

## How to fix

Confirm the change is intentional, then refresh the baseline from a clean run
of the new code:

```bash
cargo stress --profile release --save-baseline
```

If the change was accidental, restore the previous shape. For example, keep
the parameter that identifies the row stable:

Before:

```rust
ctx.parameter("record_count", 2048); // was 1024 in the baseline
```

After:

```rust
ctx.parameter("record_count", 1024);
```

To measure both sizes, add a separate row instead of changing the old one (see
the parameter sweep recipe in [bench-recipes.md](../bench-recipes.md)).

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code baseline_semantics_changed
STRESS_ALLOW_CODES=baseline_semantics_changed cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("baseline_semantics_changed")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain baseline_semantics_changed`.
