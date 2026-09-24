# `peak_rss_exceeded`

**Default severity:** warning

## What it means

The process peak resident set size recorded after this row ran
(`peak_rss_bytes`) is larger than the row's
`#[stress(max_peak_rss_mb = ...)]` budget.

## Why it matters

Peak memory limits matter for services and CI runners, but peak RSS is
process-wide and monotonic. It is the high-water mark of the whole
benchmark process, so an earlier row can trip a later row's budget. For
that reason this budget is diagnostic-class only. It never fails the budget
gate, and it is ignored by baseline validation.

Peak RSS is read from `getrusage(RUSAGE_SELF).ru_maxrss` on Unix and
normalized to bytes (macOS reports bytes, Linux reports KiB). On other
platforms it is unavailable, so this diagnostic never fires there.

## Typical causes

- The benchmark builds a large fixture or buffer.
- An earlier row in the same bench binary already raised the high-water mark.

## How to fix

Reduce peak memory in the benchmark, or raise the budget. To attribute peak
memory to one row, put memory-heavy rows in their own bench binary so the
high-water mark starts fresh.

Before:

```rust
#[stress(tier = 3, max_peak_rss_mb = 64)]
fn load_index(ctx: &mut StressContext) {
    ctx.measure("load", || black_box(vec![0_u8; 512 * 1024 * 1024]));
}
```

After:

```rust
#[stress(tier = 3, max_peak_rss_mb = 64)]
fn load_index(ctx: &mut StressContext) {
    ctx.measure("load", || black_box(stream_index_in_chunks()));
}
```

## Allowing it

It is a warning, so it only gates under `--deny-diagnostics warning` or
`--deny-code peak_rss_exceeded`. To keep it in the report but exempt it from
severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code peak_rss_exceeded
STRESS_ALLOW_CODES=peak_rss_exceeded cargo stress --deny-diagnostics warning
```

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain peak_rss_exceeded`.
