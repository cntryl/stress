# `insufficient_warmup`

**Default severity:** info

## What it means

The last warmup samples and the first measured samples sit at different
levels, so measurement likely started before the benchmark reached steady
state.

It is evaluated only when a row has at least 10 measured samples and at least
2 warmup samples (for example the `lab` and `release` profiles, or explicit
`--samples`/`--warmup-samples`). It fires when all of these hold:

- The median of the warmup tail (last up to 5 warmup samples) and the median
  of the measured head (first `max(5, ceil(n/3))` measured samples) have
  non-overlapping distribution-free 95% confidence intervals (binomial
  order-statistic bounds).
- The shift between the two medians exceeds 4 robust standard deviations
  (1.4826 x MAD) of the measured samples and 1% of the head median.

The evidence records both medians and intervals, the shift in percent, and a
suggested warmup count: the current warmup plus the number of leading measured
samples outside a 3-sigma band around the second-half median, and at least
double the current warmup. The diagnostic never changes measurements.

## Why it matters

Early measured samples that are still settling bias the summary and inflate
variance.

## Typical causes

- Lazy initialization, cold caches, or page faults on first use.
- Allocator or collection growth during the first samples.
- CPU frequency ramping up.

## How to fix

Raise warmup to the suggested count:

```bash
cargo stress --warmup-samples 8
```

Or move one-time initialization into setup so it is not measured.

## Allowing it

When this diagnostic is expected for a row, it stays in the report but can be
exempted from severity gating:

```bash
cargo stress --deny-diagnostics info --allow-code insufficient_warmup
STRESS_ALLOW_CODES=insufficient_warmup cargo stress --deny-diagnostics info
```

Programmatic runners use `StressRunnerConfig::new().allow_code("insufficient_warmup")`.
`--allow-code` only exempts the code from `--deny-diagnostics`; a code that is
also passed to `--deny-code` stays denied.

See the [diagnostic codes section of the README](../../README.md#diagnostic-codes)
and `cargo stress explain insufficient_warmup`.
