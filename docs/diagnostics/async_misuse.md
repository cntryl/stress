# `async_misuse`

**Default severity:** info

## What it means

The row was measured with an async method (`measure_async`,
`measure_result_async`, ...), but every sample's wall-clock time was within
100 ns of its measured time: no scheduling or await overhead was observed.

## Why it matters

An async row that never actually suspends usually measures synchronous work or
only the cost of *spawning* a task, not the async operation you care about.

## Typical causes

- The future completes synchronously (for example, it only wraps a
  computation in `async {}`).
- The closure spawns detached work (`tokio::spawn`) and returns without
  awaiting it.

## How to fix

Await the real operation inside the measured future.

Before:

```rust
#[stress(tier = 2)]
async fn fetch(ctx: &mut StressContext) {
    ctx.measure_async("fetch", || async {
        // Detached: the measured future returns immediately.
        tokio::spawn(fetch_remote());
    })
    .await;
}
```

After:

```rust
#[stress(tier = 2)]
async fn fetch(ctx: &mut StressContext) {
    ctx.measure_async("fetch", || async { black_box(fetch_remote().await) })
        .await;
}
```

If the work is genuinely synchronous, use `ctx.measure` instead.

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics warning --allow-code async_misuse
STRESS_ALLOW_CODES=async_misuse cargo stress --deny-diagnostics warning
```

Programmatic runners use `StressRunnerConfig::new().allow_code("async_misuse")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain async_misuse`.
