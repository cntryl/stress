# `high_allocations`

**Default severity:** warning

## What it means

The row allocated memory during measured work, and no allocation budget
says that is expected. Requires the `stress_allocator!()` tracking allocator.

## Why it matters

Hidden allocations are a common source of latency and variance. Rows
classed as allocation-oriented (`row_class = "construction"`, `"parsing"`,
`"allocation"`) get this as advisory evidence only.

## Typical causes

- A `Vec`, `String`, or map is created inside the measured closure.
- Input cloning happens inside `measure` rather than in setup.

## How to fix

Hoist reusable allocations out of the measured work, or make the expected
allocation explicit with a budget.

Before:

```rust
#[stress(tier = 1)]
fn encode(ctx: &mut StressContext) {
    ctx.measure("encode", || {
        let mut buf = Vec::with_capacity(256);
        encode_into(&mut buf, black_box(&MSG));
        black_box(buf.len())
    });
}
```

After:

```rust
#[stress(tier = 1, max_allocs_per_op = 0)]
fn encode(ctx: &mut StressContext) {
    let mut buf = Vec::with_capacity(256);
    ctx.measure("encode", || {
        buf.clear();
        encode_into(&mut buf, black_box(&MSG));
        black_box(buf.len())
    });
}
```

If allocating is the point of the row, mark it:
`#[stress(tier = 1, metadata(row_class = "allocation"))]`, or set a
non-zero `max_allocs_per_op`/`max_bytes_per_op` budget.

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code high_allocations
STRESS_ALLOW_CODES=high_allocations cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("high_allocations")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain high_allocations`.
