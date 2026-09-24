# Migrating from Criterion

cntryl-stress is not a drop-in replacement for Criterion. Each benchmark
declares a tier, reports what it completed, and produces JSON artifacts that a
gate can compare. Most Criterion benchmarks translate mechanically.

## Mapping

| Criterion | cntryl-stress |
| --- | --- |
| `criterion_group!` + `criterion_main!` | `#[stress(...)]` on each function + one `stress_main!()` |
| `[[bench]] harness = false` | Same: `harness = false` |
| `c.bench_function("name", \|b\| b.iter(\|\| f()))` | `ctx.measure("name", \|\| f())` |
| `b.iter_batched(setup, routine, BatchSize::...)` | `ctx.measure_with_setup("name", setup, routine)` |
| `iter_batched_ref` (mutate input in place) | `measure_with_setup` taking the input by value (`\|mut input\| ...`) |
| `c.benchmark_group("g")` | A bench target is a suite; rows in one function share a name prefix; group with `metadata(component = "g")` and `--workload 'module::*'` |
| `BenchmarkId::new("f", n)` / `bench_with_input` | One row per value: `ctx.benchmark(format!("f/n={n}")).parameter("n", n)`; the text report renders a sweep table |
| `Throughput::Elements(n)` | Count real work: `measure_outcome("name", LogicalUnit::new("element"), \|\| OperationOutcome::new(n, done))`, or `measure_batch("name", n, f)` for infallible work |
| `Throughput::Bytes(n)` | Name the unit and record size as a parameter: `LogicalUnit::new("record")` plus `ctx.parameter("bytes_per_logical_operation", n)` |
| `group.sample_size(n)` | `ctx.benchmark("name").samples(n)`, `--samples n`, or a profile |
| `group.warm_up_time(..)` | `ctx.benchmark("name").warmup(n)` (warmup *samples*), `--warmup-samples n` |
| `group.measurement_time(..)` | `--sample-duration-ms` (Tiers 3-6) or `--micro-sample-duration-ms` (Tier 1) |
| `criterion::black_box` | `cntryl_stress::black_box` (re-export of `std::hint::black_box`) |
| `--save-baseline x` / `--baseline x` | `--save-baseline` / `--baseline latest` (or a file path) |
| `cargo bench -- <filter>` | `cargo bench -- --workload '<glob>'` or `cargo stress --workload '<glob>'` |
| HTML reports | `target/stress/<package>/<suite>/latest.{json,txt,md}` (`target/stress/<suite>/` for plain `cargo bench`), and `cargo stress compare` |

Choose the tier from what the benchmark measures: Tier 1 for hot-path
microbenchmarks (calibrated micro timing, ns/op), Tier 2 for a single
subsystem operation, Tier 3+ for throughput. See
[bench-recipes.md](bench-recipes.md).

## Example

Criterion:

```rust
use criterion::{black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};

fn bench(c: &mut Criterion) {
    c.bench_function("parse", |b| b.iter(|| parse(black_box(INPUT))));

    c.bench_function("sort", |b| {
        b.iter_batched(|| data(1024), |mut v| v.sort_unstable(), BatchSize::SmallInput)
    });

    let mut group = c.benchmark_group("sum");
    for n in [64_u64, 1024] {
        group.throughput(Throughput::Elements(n));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| (0..n).sum::<u64>())
        });
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
```

cntryl-stress:

```rust
use cntryl_stress::{black_box, stress, stress_main, LogicalUnit, OperationOutcome, StressContext};

#[stress(tier = 1)]
fn parse_input(ctx: &mut StressContext) {
    ctx.measure("parse", || parse(black_box(INPUT)));
}

#[stress(tier = 2)]
fn sort(ctx: &mut StressContext) {
    ctx.measure_with_setup("sort", || data(1024), |mut v| {
        v.sort_unstable();
        black_box(v)
    });
}

#[stress(tier = 3)]
fn sum(ctx: &mut StressContext) {
    for n in [64_u64, 1024] {
        ctx.benchmark(format!("sum/n={n}"))
            .parameter("n", n)
            .measure_outcome(LogicalUnit::new("element"), || {
                black_box((0..black_box(n)).sum::<u64>());
                OperationOutcome::new(n, n)
            });
    }
}

stress_main!();
```

## Differences to expect

- **Correctness is part of the row.** Report failures with
  `measure_result*` or `OperationOutcome`; a failing row is untrustworthy.
- **Diagnostics instead of silent numbers.** Rows that are too fast, noisy, or
  allocation-heavy get a stable diagnostic code with a fix; see the
  [diagnostic cookbook](diagnostics/).
- **Gates are explicit.** Budgets (`max_ns_per_op`, `max_allocs_per_op`, ...),
  `--deny-code`, and the `release` profile decide pass/fail; Criterion only
  reports.
- **No plots.** Use the markdown/text reports and
  `cargo stress compare --format md`.
