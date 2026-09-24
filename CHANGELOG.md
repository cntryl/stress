# Changelog

All notable changes to `cntryl-stress` and `cntryl-stress-macros` are recorded here.
Both crates share a version number and are released together.

## [0.5.0] - 2026-09-24

0.5.0 is a breaking release. Artifacts written by 0.4 still load and compare.

### Breaking changes

- Public artifact and config types are now `#[non_exhaustive]`. Build them with
  their constructors and `with_*` builders instead of struct literals. This
  covers `StressRun`, `Sample`, `BenchmarkSummary`, `ProfileConfig`,
  `EnvironmentInfo`, `BenchmarkDiagnostic`, `ComparisonResult`,
  `BenchmarkBudgets`, `CorrectnessCounters`, `StressRunnerConfig` and the
  nested summary types (#12, #13).
- Public enums are `#[non_exhaustive]`, so a `match` on them needs a wildcard
  arm. This covers `RunGate`, `RunProfile`, `BenchmarkMode`, `QualityClass`,
  `TrustClass`, `ComparisonClass`, `DiagnosticSeverity` and similar enums
  (#11, #13). `ObservationDirection` is still exhaustive.
- `cntryl-stress` pins `cntryl-stress-macros` to exactly the same version (#12).
- Percentiles now use linear interpolation. p50, p95 and p99 values shift
  slightly compared with 0.4 artifacts. Baselines written by 0.4 are still
  validated with the math that wrote them (#11).
- The counting allocator counts only the growth from `realloc`, so
  bytes-per-op figures for workloads that grow collections are lower than in
  0.4 (#11).
- `STRESS_FILTER` matches the benchmark name, or the full `suite/bench` id when
  it contains `/`. It no longer matches the suite name alone (#11).
- If `STRESS_FAIL_ON_ISSUES` and `STRESS_DENY_DIAGNOSTICS` disagree, the
  stricter one wins and a notice is printed (#11).
- `STRESS_SAMPLES=0` and an out-of-range `STRESS_TIER` are now rejected when
  the environment is parsed (#11).

### Added

- **Diagnostics**
  - A diagnostic catalog, with a docs page for every code in
    `docs/diagnostics/` (#14, #19).
  - `--deny-code` and `--allow-code` (`STRESS_DENY_CODES`, `STRESS_ALLOW_CODES`)
    gate or exempt individual diagnostic codes (#14).
  - `cargo stress explain <code>` and `cargo stress explain --list` (#14).
  - New codes:
    - `non_finite_samples_dropped` (#14)
    - `insufficient_warmup` and `measurement_drift` (#22)
    - `peak_rss_exceeded` (#23)
    - `scaling_anomaly` (#25, #29)
  - Benchmark rows and diagnostics record the file and line of the benchmark
    that produced them (#15).
- **CI and gating**
  - Under GitHub Actions, annotations with file and line go to stderr, and the
    markdown report is appended to the step summary. It turns on when
    `GITHUB_ACTIONS` is set; `STRESS_GITHUB=0` turns it off (#16).
  - `cargo stress compare <baseline> <candidate>` prints text, markdown or JSON
    and exits 0, 1 or 2 (#17).
  - `--baseline-runs N` pools the samples of several saved baseline runs (#18).
  - `--confirm-regressions K` re-runs suspected regressions before failing the
    gate. Every attempt is recorded in the artifact (#18).
  - `--require-quiet-env` fails the run when an environment observation is
    adverse. The observations are CPU governor, turbo, load, cgroup quota and
    battery (#20).
  - `STRESS_FAIL_ON_REGRESSION`, `STRESS_FAIL_ON_QUALITY` and
    `STRESS_MIN_QUALITY` (#11).
- **Measurement**
  - An optional `tokio` feature, used with `#[stress(runtime = "tokio")]` or
    `#[stress(runtime = "tokio-multi")]`, and the re-export
    `cntryl_stress::tokio` (#21).
  - Per-benchmark peak RSS, with a `max_peak_rss_mb` budget that raises a
    diagnostic without failing the gate (#23).
  - p90, p99, p99.9 and max for recorded latencies and observations (#27).
  - The timer resolution is recorded and used as evidence for `too_fast` (#14).
  - Windows CPU model detection (#11).
- **Tooling and output**
  - `cargo stress init` sets up a bench target (#24).
  - `latest.csv` and a timestamped CSV are written with every artifact set (#26).
  - `cargo stress history`, with a guarded `--prune` (#28).
- **Docs**
  - Diagnostic cookbook, CI workflow template (`docs/ci.md`) and a guide to
    migrating from criterion (#19).

### Fixed

- **Failure handling**
  - A failing benchmark with sample overrides no longer panics or loses its
    error (#11).
  - A timeout or panic no longer throws away the results of the benchmarks that
    already completed (#11).
- **Measurement**
  - The counting allocator passes `alloc_zeroed` through to the system
    allocator (#11).
  - Fixed-duration and Micro samples that use a setup function now have a
    wall-clock limit (#11).
  - Micro timings below the timer tick are no longer zeroed (#11).
- **Reporting**
  - The p95 regression gate uses a p95 confidence interval (#11).
  - Sweep tables no longer mix unrelated benchmarks (#11).
  - Markdown cells are escaped (#11).
  - `latest.*` never points to an older run (#11).
  - Crash recovery handles artifact sets that contain only history files (#26).
- **CLI and macros**
  - Relative CLI paths resolve from the directory `cargo stress` is run in (#11).
  - Macro fixes for raw identifiers, `#[cfg]` and shadowed prelude names (#11).
- **Release workflow**
  - Tests run with read-only permissions, and actions are pinned by SHA (#11).

## [0.4.0]

Earlier releases are recorded in the git history.
