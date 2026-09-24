# `batch_unit_ambiguous`

**Default severity:** warning

## What it means

A batch row (`measure_batch`) does not say what one logical operation is: it
has no `logical_unit` parameter, or its unit mentions `batch` without a
`*_per_logical_operation` parameter that normalizes it.

## Why it matters

Throughput and ns/op for a batch are only meaningful when the reader knows
whether an "op" is one record or one batch of records.

## Typical causes

- `measure_batch` used without a `logical_unit` parameter.
- `logical_unit = "batch"` without, e.g., `records_per_logical_operation`.

## How to fix

Name the logical unit and report observed outcomes with `measure_outcome`,
or add the normalization parameters.

Before:

```rust
#[stress(tier = 3)]
fn write_records(ctx: &mut StressContext) {
    let records = fixture_records(512);
    ctx.measure_batch("write", records.len() as u64, || write_all(&records));
}
```

After:

```rust
use cntryl_stress::{LogicalUnit, OperationOutcome};

#[stress(tier = 3)]
fn write_records(ctx: &mut StressContext) {
    let records = fixture_records(512);
    ctx.measure_outcome("write", LogicalUnit::new("record"), || {
        let written = write_all(&records);
        OperationOutcome::new(records.len() as u64, written)
    });
}
```

If a batch really is the unit, say how big it is:

```rust
ctx.parameter("logical_unit", "batch");
ctx.parameter("records_per_logical_operation", 512);
```

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code batch_unit_ambiguous
STRESS_ALLOW_CODES=batch_unit_ambiguous cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("batch_unit_ambiguous")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain batch_unit_ambiguous`.
