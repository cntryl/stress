//! Stress runner that records raw samples and derives current artifacts.

use crate::config::StressRunnerConfig;
use crate::context::{MeasurementRecord, StressContext};
use crate::report::{ConsoleReporter, JsonReporter, JsonStdoutReporter, Reporter};
use crate::result::{
    attach_regression_diagnostics, compare_summaries, summarize_benchmark, BenchmarkModeKind,
    BenchmarkSpec, ComparisonClass, EnvironmentInfo, MeasurementIntent, Sample, SamplePhase,
    StressRun, MAX_TIER, SCHEMA_VERSION,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

/// Runner for Tier 1 through Tier 6 stress benchmarks.
pub struct StressRunner {
    suite: String,
    config: StressRunnerConfig,
    benchmark_specs: Vec<BenchmarkSpec>,
    samples: Vec<Sample>,
    summaries: Vec<crate::result::BenchmarkSummary>,
    suite_start: Instant,
    reporters: Vec<Box<dyn Reporter>>,
    metadata: BTreeMap<String, String>,
    environment: EnvironmentInfo,
}

impl StressRunner {
    /// Create a runner from `STRESS_*` environment configuration.
    ///
    /// # Panics
    ///
    /// Panics if the resolved config has zero measured samples or zero
    /// fixed-operations sample size.
    #[must_use]
    pub fn new(suite: &str) -> Self {
        Self::with_config(suite, StressRunnerConfig::from_env())
    }

    /// Create a runner with explicit config.
    ///
    /// # Panics
    ///
    /// Panics if the config has zero measured samples or zero fixed-operations
    /// sample size.
    #[must_use]
    pub fn with_config(suite: &str, config: StressRunnerConfig) -> Self {
        Self::with_config_and_metadata(suite, config, BTreeMap::new())
    }

    /// Create a runner with explicit config and run metadata.
    ///
    /// # Panics
    ///
    /// Panics if the config has zero measured samples or zero fixed-operations
    /// sample size.
    #[must_use]
    pub fn with_config_and_metadata(
        suite: &str,
        config: StressRunnerConfig,
        metadata: BTreeMap<String, String>,
    ) -> Self {
        let validation_errors = config.validation_errors();
        assert!(
            validation_errors.is_empty(),
            "invalid stress config: {}",
            validation_errors.join("; ")
        );

        let environment = capture_environment(&config);
        let mut reporters: Vec<Box<dyn Reporter>> = vec![Box::new(
            JsonReporter::new(config.output_dir.clone()).announce(false),
        )];
        if config.json_stdout {
            reporters.insert(0, Box::new(JsonStdoutReporter::new()));
        } else {
            reporters.insert(0, Box::new(ConsoleReporter::new()));
        }

        let runner = Self {
            suite: suite.to_string(),
            config,
            benchmark_specs: Vec::new(),
            samples: Vec::new(),
            summaries: Vec::new(),
            suite_start: Instant::now(),
            reporters,
            metadata,
            environment,
        };

        for reporter in &runner.reporters {
            reporter.suite_start(&runner.suite, &runner.config);
        }

        runner
    }

    /// Add run-level metadata.
    #[allow(clippy::needless_pass_by_value)]
    pub fn metadata(&mut self, key: impl Into<String>, value: impl ToString) -> &mut Self {
        self.metadata.insert(key.into(), value.to_string());
        self
    }

    /// Replace reporters.
    pub fn reporters(&mut self, reporters: Vec<Box<dyn Reporter>>) -> &mut Self {
        self.reporters = reporters;
        self
    }

    /// Add a reporter.
    pub fn add_reporter(&mut self, reporter: Box<dyn Reporter>) -> &mut Self {
        self.reporters.push(reporter);
        self
    }

    /// Run a Tier 2 fixed-operations benchmark with low ceremony.
    pub fn run<F>(&mut self, name: &str, f: F)
    where
        F: Fn(&mut StressContext),
    {
        let spec = BenchmarkSpec {
            id: format!("{}/{}", self.suite, name),
            name: name.to_string(),
            tier: 2,
            mode: self
                .config
                .mode_for_kind(BenchmarkModeKind::FixedOperations),
            intent: MeasurementIntent::General,
            budgets: crate::result::BenchmarkBudgets::default(),
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
        };
        self.run_spec(&spec, f);
    }

    /// Run a benchmark using a complete spec.
    ///
    /// # Panics
    ///
    /// Panics when `spec.tier` is outside the defined range of 1 through 6.
    pub fn run_spec<F>(&mut self, spec: &BenchmarkSpec, f: F)
    where
        F: Fn(&mut StressContext),
    {
        assert!(
            (1..=MAX_TIER).contains(&spec.tier),
            "Benchmark '{}' has invalid tier {}; tiers are 1 through {MAX_TIER}",
            spec.name,
            spec.tier
        );
        if let Err(error) = spec.mode.kind().validate_for_tier(spec.tier) {
            panic!(
                "Benchmark '{}' has invalid tier/mode combination: {error}",
                spec.name
            );
        }
        if !self.should_run(spec) {
            return;
        }

        let start_sample = self.samples.len();
        let mut derived_specs = BTreeMap::<String, BenchmarkSpec>::new();
        let mut spec_order = Vec::<String>::new();
        self.record_phase_samples(
            spec,
            SamplePhase::Warmup,
            self.config.warmup_samples,
            &f,
            &mut derived_specs,
            &mut spec_order,
        );
        self.record_phase_samples(
            spec,
            SamplePhase::Measured,
            self.config.samples,
            &f,
            &mut derived_specs,
            &mut spec_order,
        );
        self.record_phase_samples(
            spec,
            SamplePhase::Cooldown,
            self.config.cooldown_samples,
            &f,
            &mut derived_specs,
            &mut spec_order,
        );

        assert!(
            !derived_specs.is_empty(),
            "Benchmark '{}' did not record a measurement. Call ctx.measure(\"name\", ...) or another named timing helper.",
            spec.name
        );

        for spec_id in spec_order {
            let spec = derived_specs
                .remove(&spec_id)
                .expect("spec order contains known ids");
            let summary = summarize_benchmark(&spec, &self.samples[start_sample..]);
            for reporter in &self.reporters {
                reporter.bench_end(&summary);
            }
            self.benchmark_specs.push(spec);
            self.summaries.push(summary);
        }
    }

    /// Finish the run without a baseline comparison.
    #[must_use]
    pub fn finish(self) -> StressRun {
        self.finish_inner(Vec::new())
    }

    /// Finish the run with a current baseline artifact.
    ///
    /// # Errors
    ///
    /// Returns an error if the baseline cannot be loaded.
    pub fn finish_with_baseline(
        self,
        baseline_path: impl AsRef<Path>,
    ) -> std::io::Result<StressRun> {
        let baseline = StressRun::load(baseline_path)?;
        let comparisons =
            compare_summaries(&self.summaries, &baseline.summaries, self.config.threshold);
        Ok(self.finish_inner(comparisons))
    }

    fn finish_inner(mut self, comparisons: Vec<crate::result::ComparisonResult>) -> StressRun {
        attach_regression_diagnostics(&mut self.summaries, &comparisons);
        let run = StressRun {
            schema_version: SCHEMA_VERSION.to_string(),
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            suite: self.suite,
            run_profile: self.config.profile,
            environment: self.environment,
            benchmark_specs: self.benchmark_specs,
            samples: self.samples,
            summaries: self.summaries,
            comparisons,
            started_at: chrono_timestamp(),
            total_elapsed_ns: self.suite_start.elapsed().as_nanos(),
            metadata: self.metadata,
        };

        for reporter in &self.reporters {
            reporter.suite_end(&run);
        }

        run
    }

    fn should_run(&self, spec: &BenchmarkSpec) -> bool {
        if let Some(tier) = self.config.tier {
            if spec.tier != tier {
                return false;
            }
        }
        if let Some(filter) = &self.config.filter {
            spec.name.contains(filter) || spec.id.contains(filter)
        } else {
            true
        }
    }

    fn record_phase_samples<F>(
        &mut self,
        base_spec: &BenchmarkSpec,
        phase: SamplePhase,
        default_count: usize,
        f: &F,
        derived_specs: &mut BTreeMap<String, BenchmarkSpec>,
        spec_order: &mut Vec<String>,
    ) where
        F: Fn(&mut StressContext),
    {
        if default_count == 0 {
            return;
        }

        let mut counts = BTreeMap::<String, usize>::new();
        loop {
            let (records, wall_clock) = invoke_benchmark(base_spec, f);
            assert!(
                !records.is_empty(),
                "Benchmark '{}' did not record a measurement. Call ctx.measure(\"name\", ...) or another named timing helper.",
                base_spec.name
            );

            let mut needs_more = false;
            for record in records {
                let benchmark_id = measurement_id(&base_spec.id, &record.name);
                let target = record.overrides.target_for_phase(phase, default_count);
                register_measurement_spec(
                    base_spec,
                    &record,
                    &benchmark_id,
                    derived_specs,
                    spec_order,
                );
                let current_count = counts.entry(benchmark_id.clone()).or_default();
                if *current_count >= target {
                    continue;
                }
                let sample_number = self
                    .samples
                    .iter()
                    .filter(|sample| sample.benchmark_id == benchmark_id)
                    .count();
                let sample = self.sample_from_record(
                    &benchmark_id,
                    sample_number,
                    phase,
                    wall_clock,
                    record,
                );
                self.samples.push(sample);
                *current_count += 1;
                if *current_count < target {
                    needs_more = true;
                }
            }

            if !needs_more {
                break;
            }
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn sample_from_record(
        &self,
        benchmark_id: &str,
        sample_number: usize,
        phase: SamplePhase,
        wall_clock: std::time::Duration,
        record: MeasurementRecord,
    ) -> Sample {
        let duration = if record.duration.is_zero() && record.intent != MeasurementIntent::External
        {
            std::time::Duration::from_nanos(1)
        } else {
            record.duration
        };
        let elapsed_secs = duration.as_secs_f64();
        let operations_attempted = record.counters.attempted;
        let operations_completed = record.counters.completed;
        let throughput = if elapsed_secs > 0.0 {
            operations_completed as f64 / elapsed_secs
        } else {
            0.0
        };
        let micro = record.micro;
        let gross_elapsed_ns = micro.map(|micro| micro.gross_elapsed.as_nanos());
        let overhead_ns = micro.map(|micro| micro.overhead.as_nanos());
        let net_elapsed_ns = micro.map(|micro| micro.net_elapsed.as_nanos());
        let calibrated_iterations = micro.map(|micro| micro.iterations);
        let gross_ns_per_op =
            micro.and_then(|micro| ns_per_op(micro.gross_elapsed.as_nanos(), micro.iterations));
        let overhead_ns_per_op =
            micro.and_then(|micro| ns_per_op(micro.overhead.as_nanos(), micro.iterations));
        let net_ns_per_op =
            micro.and_then(|micro| ns_per_op(micro.net_elapsed.as_nanos(), micro.iterations));
        let allocation = record.allocation;
        let allocs = allocation.map(|allocation| allocation.allocs);
        let bytes = allocation.map(|allocation| allocation.bytes);
        let allocs_per_op = allocs.and_then(|allocs| count_per_op(allocs, operations_completed));
        let bytes_per_op = bytes.and_then(|bytes| count_per_op(bytes, operations_completed));

        Sample {
            benchmark_id: benchmark_id.to_string(),
            intent: record.intent,
            sample_number,
            phase,
            elapsed_ns: duration.as_nanos(),
            wall_clock_ns: wall_clock.as_nanos(),
            operations_attempted,
            operations_completed,
            throughput,
            calibrated_iterations,
            gross_elapsed_ns,
            overhead_ns,
            net_elapsed_ns,
            gross_ns_per_op,
            overhead_ns_per_op,
            net_ns_per_op,
            allocs,
            bytes,
            allocs_per_op,
            bytes_per_op,
            latency_ns: record.latency_ns,
            parameters: record.parameters,
            counters: record.counters,
            environment: self.environment.clone(),
        }
    }
}

fn invoke_benchmark<F>(spec: &BenchmarkSpec, f: &F) -> (Vec<MeasurementRecord>, std::time::Duration)
where
    F: Fn(&mut StressContext),
{
    let mut ctx = StressContext::new(spec.tier, spec.mode.clone());
    let wall_clock_start = Instant::now();
    f(&mut ctx);
    let wall_clock = wall_clock_start.elapsed();
    (ctx.take_measurements(), wall_clock)
}

fn measurement_id(base_id: &str, measurement_name: &str) -> String {
    format!("{base_id}/{measurement_name}")
}

fn register_measurement_spec(
    base_spec: &BenchmarkSpec,
    record: &MeasurementRecord,
    benchmark_id: &str,
    derived_specs: &mut BTreeMap<String, BenchmarkSpec>,
    spec_order: &mut Vec<String>,
) {
    if let Some(spec) = derived_specs.get_mut(benchmark_id) {
        spec.metadata.extend(record.metadata.clone());
        spec.parameters.extend(record.parameters.clone());
        return;
    }

    spec_order.push(benchmark_id.to_string());
    let mut metadata = base_spec.metadata.clone();
    metadata.extend(record.metadata.clone());
    let mut parameters = base_spec.parameters.clone();
    parameters.extend(record.parameters.clone());
    derived_specs.insert(
        benchmark_id.to_string(),
        BenchmarkSpec {
            id: benchmark_id.to_string(),
            name: record.name.clone(),
            tier: base_spec.tier,
            mode: record.mode.clone(),
            intent: record.intent,
            budgets: base_spec.budgets,
            parameters,
            metadata,
        },
    );
}

#[allow(clippy::cast_precision_loss)]
fn ns_per_op(elapsed_ns: u128, operations: u64) -> Option<f64> {
    (operations != 0)
        .then(|| elapsed_ns as f64 / operations as f64)
        .filter(|value| value.is_finite())
}

#[allow(clippy::cast_precision_loss)]
fn count_per_op(count: u64, operations: u64) -> Option<f64> {
    (operations != 0)
        .then(|| count as f64 / operations as f64)
        .filter(|value| value.is_finite())
}

fn capture_environment(config: &StressRunnerConfig) -> EnvironmentInfo {
    EnvironmentInfo {
        cpu_model: detect_cpu_model(),
        core_count: std::thread::available_parallelism()
            .ok()
            .map(std::num::NonZeroUsize::get),
        os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        rustc_version: command_stdout("rustc", &["--version"])
            .unwrap_or_else(|| "unknown".to_string()),
        allocator: "unknown".to_string(),
        build_profile: if cfg!(debug_assertions) {
            "debug".to_string()
        } else {
            "release".to_string()
        },
        git_commit: config.git_sha.clone().or_else(detect_git_sha),
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        command_line: std::env::args().collect(),
        profile_config: config.profile_config(),
    }
}

fn detect_cpu_model() -> String {
    #[cfg(target_os = "macos")]
    {
        command_stdout("sysctl", &["-n", "machdep.cpu.brand_string"])
            .unwrap_or_else(|| "unknown".to_string())
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|content| {
                content.lines().find_map(|line| {
                    line.strip_prefix("model name").and_then(|line| {
                        line.split_once(':')
                            .map(|(_, value)| value.trim().to_string())
                    })
                })
            })
            .unwrap_or_else(|| "unknown".to_string())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        "unknown".to_string()
    }
}

fn detect_git_sha() -> Option<String> {
    command_stdout("git", &["rev-parse", "HEAD"])
}

fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|value| value.trim().to_string())
            } else {
                None
            }
        })
        .filter(|value| !value.is_empty())
}

fn chrono_timestamp() -> String {
    let now = std::time::SystemTime::now();
    let duration = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    duration.as_millis().to_string()
}

/// Gate decision for a finished run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunGate {
    /// The run satisfies correctness, quality, and regression policy.
    Passed,
    /// At least one correctness counter failed.
    CorrectnessFailed,
    /// Release quality policy failed.
    QualityFailed,
    /// Meaningful regression policy failed.
    RegressionFailed,
    /// At least one configured benchmark budget failed.
    BudgetFailed,
}

/// Evaluate a run against its profile policy.
#[must_use]
pub fn evaluate_run_gate(run: &StressRun) -> RunGate {
    if !run.correctness_passed() {
        return RunGate::CorrectnessFailed;
    }
    if !run.budgets_passed() {
        return RunGate::BudgetFailed;
    }
    let profile_config = &run.environment.profile_config;
    if profile_config.fail_on_quality && !run.meets_min_quality(profile_config.min_quality) {
        return RunGate::QualityFailed;
    }
    if profile_config.fail_on_regression
        && run
            .comparisons
            .iter()
            .any(|comparison| comparison.classification == ComparisonClass::Regression)
    {
        return RunGate::RegressionFailed;
    }
    RunGate::Passed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{
        BenchmarkBudgets, BenchmarkMode, BenchmarkModeKind, CorrectnessCounters, PrimaryMetric,
        RunProfile,
    };
    use std::time::Duration;

    #[test]
    fn records_raw_samples_and_summarizes_measured_only() {
        let config = StressRunnerConfig::new()
            .samples(2)
            .warmup_samples(1)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("bench", |ctx| {
            ctx.parameter("client_count", 1);
            ctx.measure("lookup", || {
                std::hint::black_box(1_u64);
            });
        });
        let run = runner.finish();

        assert_eq!(run.schema_version, SCHEMA_VERSION);
        assert_eq!(run.samples.len(), 3);
        assert!(run.samples.iter().all(|sample| sample.wall_clock_ns > 0));
        assert_eq!(run.summaries[0].warmup_samples, 1);
        assert_eq!(run.summaries[0].measured_samples, 2);
        assert!(run.summaries[0].total_wall_clock_ns > 0);
        assert!(run.summaries[0].wall_clock.is_some());
        assert_eq!(
            run.summaries[0].parameters.get("client_count"),
            Some(&"1".to_string())
        );
    }

    #[test]
    fn run_spec_respects_tier_filter() {
        let config = StressRunnerConfig::new().tier(3);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("tier2", |ctx| {
            ctx.measure("work", || {});
        });

        let run = runner.finish();
        assert!(run.summaries.is_empty());
    }

    #[test]
    #[should_panic(expected = "tiers are 1 through 6")]
    fn run_spec_rejects_undefined_tiers() {
        let config = StressRunnerConfig::new();
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());
        let spec = BenchmarkSpec {
            id: "suite/undefined".to_string(),
            name: "undefined".to_string(),
            tier: MAX_TIER + 1,
            mode: BenchmarkMode::FixedOperations {
                operations_per_sample: 1,
            },
            intent: MeasurementIntent::General,
            budgets: crate::result::BenchmarkBudgets::default(),
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
        };

        runner.run_spec(&spec, |ctx| {
            ctx.measure("work", || {});
        });
    }

    #[test]
    #[should_panic(
        expected = "Tier 3 uses fixed_duration; remove mode or use tier = 2 for fixed_operations."
    )]
    fn run_spec_rejects_tier_mode_mismatches() {
        let config = StressRunnerConfig::new();
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());
        let spec = BenchmarkSpec {
            id: "suite/mismatch".to_string(),
            name: "mismatch".to_string(),
            tier: 3,
            mode: BenchmarkMode::FixedOperations {
                operations_per_sample: 1,
            },
            intent: MeasurementIntent::General,
            budgets: crate::result::BenchmarkBudgets::default(),
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
        };

        runner.run_spec(&spec, |ctx| {
            ctx.measure("work", || {});
        });
    }

    #[test]
    fn correctness_error_fails_run_gate() {
        let config = StressRunnerConfig::new()
            .samples(2)
            .profile(RunProfile::Release);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("bench", |ctx| {
            ctx.measure("work", || {});
            let _ = ctx.correctness().attempted(1).completed(0).failures(1);
        });
        let run = runner.finish();

        assert_eq!(evaluate_run_gate(&run), RunGate::CorrectnessFailed);
    }

    #[test]
    fn run_gate_fails_release_quality_policy() {
        let config = StressRunnerConfig::for_profile(RunProfile::Release).samples(2);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("bench", |ctx| {
            ctx.measure("work", || {});
        });
        let run = runner.finish();

        assert_eq!(evaluate_run_gate(&run), RunGate::QualityFailed);
    }

    #[test]
    fn low_ceremony_fixed_operation_benchmark_has_one_completed_operation() {
        let config = StressRunnerConfig::new();
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("bench", |ctx| {
            ctx.measure("work", || std::hint::black_box(1_u64));
        });
        let run = runner.finish();

        assert_eq!(run.samples[0].operations_attempted, 1);
        assert_eq!(run.samples[0].operations_completed, 1);
    }

    #[test]
    fn explicit_fixed_duration_workload_uses_active_mode() {
        let config = StressRunnerConfig::new().sample_duration(Duration::from_millis(1));
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());
        let spec = BenchmarkSpec {
            id: "suite/throughput".to_string(),
            name: "throughput".to_string(),
            tier: 3,
            mode: BenchmarkMode::FixedDuration {
                sample_duration: Duration::from_millis(1),
            },
            intent: MeasurementIntent::General,
            budgets: crate::result::BenchmarkBudgets::default(),
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
        };

        runner.run_spec(&spec, |ctx| {
            ctx.measure("throughput", || {
                std::hint::black_box(1_u64);
            });
        });
        let run = runner.finish();

        assert!(run.samples[0].operations_completed > 0);
        assert!(run.samples[0].throughput > 0.0);
    }

    #[test]
    fn tier2_counted_recipe_records_logical_operation_totals() {
        let config = StressRunnerConfig::for_profile(RunProfile::Smoke);
        let mut runner = StressRunner::with_config("suite", config.clone());
        runner.reporters(Vec::new());
        let spec = BenchmarkSpec {
            id: "suite/counted".to_string(),
            name: "counted".to_string(),
            tier: 2,
            mode: config.mode_for_kind(BenchmarkModeKind::FixedOperations),
            intent: MeasurementIntent::General,
            budgets: BenchmarkBudgets::default(),
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
        };

        runner.run_spec(&spec, |ctx| {
            let completed = ctx.measure_batch("counted", 768, || {
                for _ in 0..3 {
                    std::hint::black_box(1_u64);
                }
            });
            assert_eq!(completed, 768);
        });
        let run = runner.finish();
        let sample = &run.samples[0];

        assert_eq!(sample.operations_attempted, 768);
        assert_eq!(sample.operations_completed, 768);
        assert_eq!(sample.counters.attempted, 768);
        assert_eq!(sample.counters.completed, 768);
        assert!(sample.throughput > 0.0);
    }

    #[test]
    fn externally_timed_recipe_records_logical_throughput_without_allocation_stats() {
        let config = StressRunnerConfig::for_profile(RunProfile::Smoke);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("external", |ctx| {
            ctx.record_external("remote", Duration::from_millis(10), 500);
        });
        let run = runner.finish();
        let sample = &run.samples[0];

        assert_eq!(sample.elapsed_ns, 10_000_000);
        assert_eq!(sample.operations_attempted, 500);
        assert_eq!(sample.operations_completed, 500);
        assert!((sample.throughput - 50_000.0).abs() < f64::EPSILON);
        assert!(sample.allocs.is_none());
        assert!(sample.bytes.is_none());
        assert!(sample.allocs_per_op.is_none());
        assert!(sample.bytes_per_op.is_none());
    }

    #[test]
    fn micro_mode_records_raw_overhead_and_per_operation_fields() {
        let config = StressRunnerConfig::for_profile(RunProfile::Smoke)
            .micro_sample_duration(Duration::from_millis(1));
        let mut runner = StressRunner::with_config("suite", config.clone());
        runner.reporters(Vec::new());
        let spec = BenchmarkSpec {
            id: "suite/hot_path".to_string(),
            name: "hot_path".to_string(),
            tier: 1,
            mode: config.mode_for_kind(BenchmarkModeKind::Micro),
            intent: MeasurementIntent::General,
            budgets: BenchmarkBudgets::default(),
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
        };

        runner.run_spec(&spec, |ctx| {
            ctx.measure("hot_path", || std::hint::black_box(1_u64));
        });
        let run = runner.finish();
        let sample = &run.samples[0];
        let summary = &run.summaries[0];

        assert!(sample.calibrated_iterations.expect("iterations") > 0);
        assert!(sample.gross_elapsed_ns.expect("gross") >= sample.net_elapsed_ns.expect("net"));
        assert!(sample.net_ns_per_op.expect("ns/op") >= 0.0);
        assert_eq!(summary.primary_metric, PrimaryMetric::NsPerOp);
        assert!(summary.ns_per_op.is_some());
    }

    fn run_allocating_fixed_operation(budgets: BenchmarkBudgets) -> StressRun {
        let config = StressRunnerConfig::for_profile(RunProfile::Smoke)
            .samples(2)
            .operations_per_sample(2);
        let mut runner = StressRunner::with_config("suite", config.clone());
        runner.reporters(Vec::new());
        let spec = BenchmarkSpec {
            id: "suite/allocating".to_string(),
            name: "allocating".to_string(),
            tier: 2,
            mode: config.mode_for_kind(BenchmarkModeKind::FixedOperations),
            intent: MeasurementIntent::General,
            budgets,
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
        };

        runner.run_spec(&spec, |ctx| {
            ctx.measure("allocating", || {
                let data = vec![1_u8; 16];
                std::hint::black_box(data);
            });
        });
        runner.finish()
    }

    #[test]
    fn fixed_operation_samples_record_allocation_stats() {
        let run = run_allocating_fixed_operation(BenchmarkBudgets::default());
        let measured = run
            .samples
            .iter()
            .filter(|sample| sample.phase == SamplePhase::Measured)
            .collect::<Vec<_>>();

        assert_eq!(measured.len(), 2);
        for sample in measured {
            assert!(sample.allocs.expect("allocs") >= 2);
            assert!(sample.bytes.expect("bytes") >= 32);
            assert!(sample.allocs_per_op.expect("allocs/op") > 0.0);
            assert!(sample.bytes_per_op.expect("bytes/op") > 0.0);
        }
        assert!(run.summaries[0].allocs_per_op.is_some());
        assert!(run.summaries[0].bytes_per_op.is_some());
    }

    #[test]
    fn non_micro_allocation_budgets_use_measured_stats() {
        let passing = run_allocating_fixed_operation(BenchmarkBudgets {
            max_allocs_per_op: Some(10_000.0),
            max_bytes_per_op: Some(100_000.0),
            ..BenchmarkBudgets::default()
        });
        assert!(passing.summaries[0]
            .budget_results
            .iter()
            .all(|result| result.passed));
        assert_eq!(evaluate_run_gate(&passing), RunGate::Passed);

        let failing = run_allocating_fixed_operation(BenchmarkBudgets {
            max_allocs_per_op: Some(0.0),
            max_bytes_per_op: Some(0.0),
            ..BenchmarkBudgets::default()
        });
        assert!(failing.summaries[0]
            .budget_results
            .iter()
            .any(|result| !result.passed
                && result.actual.is_some()
                && result
                    .reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("exceeds"))));
        assert_eq!(evaluate_run_gate(&failing), RunGate::BudgetFailed);
    }

    #[test]
    fn manual_correctness_counters_are_preserved() {
        let config = StressRunnerConfig::new();
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("bench", |ctx| {
            ctx.measure("work", || {});
            let _ = ctx.correctness().attempted(5).completed(5);
        });
        let run = runner.finish();

        assert_eq!(
            run.samples[0].counters,
            CorrectnessCounters {
                attempted: 5,
                completed: 5,
                ..CorrectnessCounters::default()
            }
        );
    }
}
