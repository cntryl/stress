//! Stress runner that records raw samples and derives current artifacts.

use crate::allocation;
use crate::artifact::{
    attach_measurement_mode_mismatch_diagnostics, attach_regression_diagnostics,
    compare_summaries_with_specs, diagnostic_summary_for_run, summarize_benchmark,
    BenchmarkModeKind, BenchmarkSpec, EnvironmentInfo, MeasurementIntent, RunProfile, Sample,
    SamplePhase, StressRun, MAX_TIER, SCHEMA_VERSION,
};
use crate::config::StressRunnerConfig;
use crate::context::{MeasurementRecord, StressContext};
use crate::error::IntoStressResult;
use crate::reporting::{
    ConsoleReporter, JsonReporter, JsonStdoutReporter, Reporter, SampleProgress,
    StderrProgressReporter,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static RUN_TIMESTAMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Runner for Tier 1 through Tier 6 stress benchmarks.
pub struct StressRunner {
    suite: String,
    config: StressRunnerConfig,
    benchmark_specs: Vec<BenchmarkSpec>,
    seen_base_spec_ids: BTreeSet<String>,
    samples: Vec<Sample>,
    summaries: Vec<crate::artifact::BenchmarkSummary>,
    suite_start: Instant,
    reporters: Vec<Box<dyn Reporter>>,
    deferred_reporters: Vec<Box<dyn Reporter>>,
    metadata: BTreeMap<String, String>,
    environment: EnvironmentInfo,
}

impl StressRunner {
    /// Create a runner from `STRESS_*` environment configuration.
    ///
    /// # Panics
    ///
    /// Panics if the resolved config has zero measured samples, zero
    /// fixed-operations sample size, or an invalid regression threshold.
    #[must_use]
    pub fn new(suite: &str) -> Self {
        Self::with_config(suite, StressRunnerConfig::from_env())
    }

    /// Create a runner with explicit config.
    ///
    /// # Panics
    ///
    /// Panics if the suite name is not a portable path component, or if the
    /// config has zero measured samples, zero fixed-operations sample size, or
    /// an invalid regression threshold.
    #[must_use]
    pub fn with_config(suite: &str, config: StressRunnerConfig) -> Self {
        Self::with_config_and_metadata(suite, config, BTreeMap::new())
    }

    /// Create a runner with explicit config and run metadata.
    ///
    /// # Panics
    ///
    /// Panics if the config has zero measured samples, zero fixed-operations
    /// sample size, or an invalid regression threshold.
    #[must_use]
    pub fn with_config_and_metadata(
        suite: &str,
        config: StressRunnerConfig,
        mut metadata: BTreeMap<String, String>,
    ) -> Self {
        assert!(
            !suite.trim().is_empty(),
            "stress suite name must not be empty"
        );
        assert!(
            !matches!(suite, "." | ".."),
            "stress suite name must not be a filesystem dot segment"
        );
        assert!(
            suite
                .chars()
                .all(|character| character.is_ascii_alphanumeric()
                    || matches!(character, '-' | '_' | '.')),
            "stress suite name must contain only ASCII letters, digits, '.', '-', or '_'"
        );
        let validation_errors = config.validation_errors();
        assert!(
            validation_errors.is_empty(),
            "invalid stress config: {}",
            validation_errors.join("; ")
        );

        let environment = capture_environment(&config);
        if let Some(run_id) = std::env::var("STRESS_RUN_ID")
            .ok()
            .filter(|value| !value.is_empty())
        {
            metadata.entry("run_id".to_string()).or_insert(run_id);
        }
        let mut reporters: Vec<Box<dyn Reporter>> = Vec::new();
        let mut deferred_reporters: Vec<Box<dyn Reporter>> = Vec::new();
        if config.json_stdout {
            // Emit the single machine receipt only after artifact reporters
            // have attached any publication failures to the canonical run.
            deferred_reporters.push(Box::new(JsonStdoutReporter::new()));
        } else {
            // The final human result line must reflect artifact publication
            // failures just like the machine receipt does.
            deferred_reporters.push(Box::new(ConsoleReporter::new()));
            if config.progress {
                reporters.push(Box::new(StderrProgressReporter::new()));
            }
        }
        // Publish artifacts after live progress and before the deferred final
        // human or machine receipt.
        reporters.push(Box::new(
            JsonReporter::new(config.output_dir.clone()).announce(false),
        ));

        let runner = Self {
            suite: suite.to_string(),
            config,
            benchmark_specs: Vec::new(),
            seen_base_spec_ids: BTreeSet::new(),
            samples: Vec::new(),
            summaries: Vec::new(),
            suite_start: Instant::now(),
            reporters,
            deferred_reporters,
            metadata,
            environment,
        };

        for reporter in runner.reporters.iter().chain(&runner.deferred_reporters) {
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
        self.deferred_reporters.clear();
        self
    }

    /// Add a reporter.
    pub fn add_reporter(&mut self, reporter: Box<dyn Reporter>) -> &mut Self {
        self.reporters.push(reporter);
        self
    }

    /// Run a Tier 2 fixed-operations benchmark with low ceremony.
    pub fn run<F, O>(&mut self, name: &str, f: F)
    where
        F: Fn(&mut StressContext) -> O,
        O: IntoStressResult,
    {
        let spec = BenchmarkSpec {
            id: format!("{}/{}", self.suite, name),
            name: name.to_string(),
            tier: 2,
            mode: self
                .config
                .mode_for_kind(BenchmarkModeKind::FixedOperations),
            intent: MeasurementIntent::General,
            budgets: crate::artifact::BenchmarkBudgets::default(),
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
        };
        self.run_spec(&spec, f);
    }

    /// Run a benchmark using a complete spec.
    ///
    /// # Panics
    ///
    /// Panics when the spec has an invalid id, name, tier, mode, role, or budget.
    pub fn run_spec<F, O>(&mut self, spec: &BenchmarkSpec, f: F)
    where
        F: Fn(&mut StressContext) -> O,
        O: IntoStressResult,
    {
        let validation_errors = benchmark_spec_validation_errors(spec);
        assert!(
            validation_errors.is_empty(),
            "Benchmark spec {:?} is invalid: {}",
            spec.name,
            validation_errors.join("; ")
        );
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
        assert!(
            self.seen_base_spec_ids.insert(spec.id.clone()),
            "Benchmark id {:?} was registered more than once; every benchmark function must have a unique suite-qualified id",
            spec.id
        );

        for reporter in &self.reporters {
            reporter.bench_start(spec);
        }

        let start_sample = self.samples.len();
        let mut topology = MeasurementTopology::default();
        let failed = self.record_phase_samples(
            spec,
            SamplePhase::Warmup,
            self.config.warmup_samples,
            &f,
            &mut topology,
            start_sample,
        );
        let failed = failed
            || self.record_phase_samples(
                spec,
                SamplePhase::Measured,
                self.config.samples,
                &f,
                &mut topology,
                start_sample,
            );
        if !failed {
            self.record_phase_samples(
                spec,
                SamplePhase::Cooldown,
                self.config.cooldown_samples,
                &f,
                &mut topology,
                start_sample,
            );
        }

        assert!(
            !topology.specs.is_empty(),
            "Benchmark '{}' did not record a measurement. Call ctx.measure(\"name\", ...) or another named timing helper.",
            spec.name
        );

        let MeasurementTopology {
            mut specs,
            spec_order,
            ..
        } = topology;
        for spec_id in spec_order {
            let spec = specs
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
    /// Returns an error if the baseline cannot be loaded or its serialized
    /// summaries do not match its canonical specs and raw samples.
    pub fn finish_with_baseline(
        self,
        baseline_path: impl AsRef<Path>,
    ) -> std::io::Result<StressRun> {
        let baseline = StressRun::load(baseline_path)?;
        let baseline_gate = evaluate_run_gate(&baseline);
        if baseline_gate != RunGate::Passed {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "baseline run is not eligible because its recorded gate evaluates to {baseline_gate:?}; use a passed run saved with --save-baseline"
                ),
            ));
        }
        let baseline_summaries = baseline
            .canonical_baseline_summaries()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let comparisons = compare_summaries_with_specs(
            &self.summaries,
            &self.benchmark_specs,
            &self.environment,
            &baseline_summaries,
            &baseline.benchmark_specs,
            &baseline.environment,
            self.config.threshold,
        );
        Ok(self.finish_inner(comparisons))
    }

    fn finish_inner(mut self, comparisons: Vec<crate::artifact::ComparisonResult>) -> StressRun {
        attach_regression_diagnostics(&mut self.summaries, &comparisons);
        attach_measurement_mode_mismatch_diagnostics(&mut self.summaries);
        let diagnostics_summary = diagnostic_summary_for_run(&self.suite, &self.summaries);
        let mut run = StressRun {
            schema_version: SCHEMA_VERSION.to_string(),
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            suite: self.suite,
            run_profile: self.config.profile,
            environment: self.environment,
            benchmark_specs: self.benchmark_specs,
            samples: self.samples,
            summaries: self.summaries,
            comparisons,
            diagnostics_summary,
            started_at: run_timestamp_stem(),
            total_elapsed_ns: self.suite_start.elapsed().as_nanos(),
            metadata: self.metadata,
        };

        for reporter in self.reporters.iter().chain(&self.deferred_reporters) {
            if let Err(error) = reporter.suite_end(&run) {
                let message = error.to_string();
                eprintln!("Stress reporter failed: {message}");
                run.metadata
                    .entry("reporter_errors".to_string())
                    .and_modify(|existing| {
                        existing.push_str("; ");
                        existing.push_str(&message);
                    })
                    .or_insert(message);
            }
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

    fn record_phase_samples<F, O>(
        &mut self,
        base_spec: &BenchmarkSpec,
        phase: SamplePhase,
        default_count: usize,
        f: &F,
        topology: &mut MeasurementTopology,
        start_sample: usize,
    ) -> bool
    where
        F: Fn(&mut StressContext) -> O,
        O: IntoStressResult,
    {
        if !topology.phase_requires_invocation(phase, default_count) {
            return false;
        }

        let mut counts = BTreeMap::<String, usize>::new();
        loop {
            let (records, wall_clock, failed) = invoke_benchmark(base_spec, f);
            if failed {
                self.samples.truncate(start_sample);
                *topology = MeasurementTopology::default();
                topology.validate_invocation(base_spec, SamplePhase::Measured, &records);
                for record in records {
                    let benchmark_id = measurement_id(&base_spec.id, &record.name);
                    let progress_name = topology
                        .specs
                        .get(&benchmark_id)
                        .map_or_else(|| record.name.clone(), |spec| spec.name.clone());
                    self.samples.push(self.sample_from_record(
                        &benchmark_id,
                        0,
                        SamplePhase::Measured,
                        wall_clock,
                        record,
                    ));
                    self.emit_sample_progress(
                        &benchmark_id,
                        &progress_name,
                        base_spec.tier,
                        SamplePhase::Measured,
                        1,
                        1,
                    );
                }
                return true;
            }
            assert!(
                !records.is_empty(),
                "Benchmark '{}' did not record a measurement. Call ctx.measure(\"name\", ...) or another named timing helper.",
                base_spec.name
            );
            topology.validate_invocation(base_spec, phase, &records);

            let mut needs_more = false;
            for record in records {
                let benchmark_id = measurement_id(&base_spec.id, &record.name);
                let target = record.overrides.target_for_phase(phase, default_count);
                let progress_name = topology
                    .specs
                    .get(&benchmark_id)
                    .map_or_else(|| record.name.clone(), |spec| spec.name.clone());
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
                self.emit_sample_progress(
                    &benchmark_id,
                    &progress_name,
                    base_spec.tier,
                    phase,
                    *current_count,
                    target,
                );
                if *current_count < target {
                    needs_more = true;
                }
            }

            if !needs_more {
                break;
            }
        }
        false
    }

    fn emit_sample_progress(
        &self,
        benchmark_id: &str,
        name: &str,
        tier: u32,
        phase: SamplePhase,
        completed_samples: usize,
        target_samples: usize,
    ) {
        let progress = SampleProgress {
            benchmark_id: benchmark_id.to_string(),
            name: name.to_string(),
            tier,
            phase,
            completed_samples,
            target_samples,
        };
        for reporter in &self.reporters {
            reporter.sample_progress(&progress);
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
            micro.and_then(|micro| ns_per_op(micro.gross_elapsed.as_nanos(), operations_completed));
        let overhead_ns_per_op =
            micro.and_then(|micro| ns_per_op(micro.overhead.as_nanos(), operations_completed));
        let net_ns_per_op =
            micro.and_then(|micro| ns_per_op(micro.net_elapsed.as_nanos(), operations_completed));
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
            observations: record.observations,
            parameters: record.parameters,
            counters: record.counters,
            environment: self.environment.clone(),
        }
    }
}

fn benchmark_spec_validation_errors(spec: &BenchmarkSpec) -> Vec<String> {
    let mut errors = Vec::new();
    if spec.id.trim().is_empty() {
        errors.push("id must not be empty".to_string());
    }
    if spec.name.trim().is_empty() {
        errors.push("name must not be empty".to_string());
    }
    match spec.mode {
        crate::artifact::BenchmarkMode::Micro {
            target_sample_duration,
        } if target_sample_duration.is_zero() => {
            errors.push("micro target_sample_duration must be greater than 0".to_string());
        }
        crate::artifact::BenchmarkMode::FixedDuration { sample_duration }
            if sample_duration.is_zero() =>
        {
            errors.push("fixed sample_duration must be greater than 0".to_string());
        }
        crate::artifact::BenchmarkMode::FixedOperations {
            operations_per_sample: 0,
        } => {
            errors.push("fixed operations_per_sample must be greater than 0".to_string());
        }
        _ => {}
    }
    for (name, value, maximum) in [
        ("max_ns_per_op", spec.budgets.max_ns_per_op, None),
        ("max_allocs_per_op", spec.budgets.max_allocs_per_op, None),
        ("max_bytes_per_op", spec.budgets.max_bytes_per_op, None),
        (
            "max_regression_pct",
            spec.budgets.max_regression_pct,
            Some(100.0),
        ),
        ("max_rsd_pct", spec.budgets.max_rsd_pct, None),
    ] {
        if value.is_some_and(|value| {
            !value.is_finite() || value < 0.0 || maximum.is_some_and(|maximum| value > maximum)
        }) {
            errors.push(maximum.map_or_else(
                || format!("{name} must be a finite non-negative number"),
                |maximum| format!("{name} must be finite and between 0 and {maximum}"),
            ));
        }
    }
    if spec.metadata.get("trust_class").is_some_and(|value| {
        value
            .parse::<crate::artifact::TrustClass>()
            .map_or(true, |role| role == crate::artifact::TrustClass::Invalid)
    }) {
        errors.push("metadata trust_class must be gate, diagnostic, or experimental".to_string());
    }
    errors
}

fn invoke_benchmark<F, O>(
    spec: &BenchmarkSpec,
    f: &F,
) -> (Vec<MeasurementRecord>, std::time::Duration, bool)
where
    F: Fn(&mut StressContext) -> O,
    O: IntoStressResult,
{
    let mut ctx = StressContext::new(spec.tier, spec.mode.clone());
    let wall_clock_start = Instant::now();
    let error = f(&mut ctx).into_stress_result().err();
    let wall_clock = wall_clock_start.elapsed();
    if let Some(error) = error {
        let mut records = ctx.take_measurements();
        if let Some(failed_record) = records
            .iter_mut()
            .rev()
            .find(|record| !record.counters.passed())
        {
            failed_record
                .metadata
                .insert("benchmark_error".to_string(), error.message().to_string());
        } else {
            let mut error_ctx = StressContext::new(spec.tier, spec.mode.clone());
            error_ctx.record_benchmark_error(error.message());
            records.extend(error_ctx.take_measurements());
        }
        (records, wall_clock, true)
    } else {
        (ctx.take_measurements(), wall_clock, false)
    }
}

fn measurement_id(base_id: &str, measurement_name: &str) -> String {
    format!("{base_id}/{measurement_name}")
}

#[derive(Default)]
struct MeasurementTopology {
    names: Option<Vec<String>>,
    specs: BTreeMap<String, BenchmarkSpec>,
    spec_order: Vec<String>,
    overrides: BTreeMap<String, MeasurementOverrideContract>,
}

impl MeasurementTopology {
    fn phase_requires_invocation(&self, phase: SamplePhase, default_count: usize) -> bool {
        if self.names.is_none() {
            return default_count != 0;
        }
        self.spec_order.iter().any(|benchmark_id| {
            self.overrides
                .get(benchmark_id)
                .is_some_and(|overrides| overrides.target_for_phase(phase, default_count) != 0)
        })
    }

    fn validate_invocation(
        &mut self,
        base_spec: &BenchmarkSpec,
        phase: SamplePhase,
        records: &[MeasurementRecord],
    ) {
        let mut unique_names = BTreeSet::new();
        let names = records
            .iter()
            .map(|record| {
                assert!(
                    unique_names.insert(record.name.as_str()),
                    "Benchmark {:?} recorded duplicate measurement name {:?} during {}; each invocation must record every named row exactly once",
                    base_spec.name,
                    record.name,
                    sample_phase_label(phase),
                );
                record.name.clone()
            })
            .collect::<Vec<_>>();

        if let Some(expected) = &self.names {
            assert_measurement_names_match(base_spec, phase, expected, &names);
        } else {
            self.names = Some(names);
        }

        for record in records {
            self.register_record(base_spec, phase, record);
        }
    }

    fn register_record(
        &mut self,
        base_spec: &BenchmarkSpec,
        phase: SamplePhase,
        record: &MeasurementRecord,
    ) {
        let benchmark_id = measurement_id(&base_spec.id, &record.name);
        let candidate = measurement_spec(base_spec, record, &benchmark_id);
        let overrides = MeasurementOverrideContract::from_record(record);
        assert!(
            phase == SamplePhase::Warmup || overrides.warmup.unwrap_or_default() == 0,
            "Benchmark {:?} measurement {:?} cannot enable warmup after the suite's warmup count disabled the phase; set a nonzero suite/profile warmup or remove the row override",
            base_spec.name,
            record.name,
        );
        if let Some(existing) = self.specs.get(&benchmark_id) {
            assert_measurement_spec_matches(base_spec, phase, existing, &candidate);
            let expected_overrides = self
                .overrides
                .get(&benchmark_id)
                .expect("registered measurement has override contract");
            assert!(
                *expected_overrides == overrides,
                "Benchmark {:?} measurement {:?} changed sample overrides during {} from {expected_overrides:?} to {overrides:?}; declare identical samples/warmup/cooldown overrides on every invocation",
                base_spec.name,
                record.name,
                sample_phase_label(phase),
            );
            return;
        }

        if let Some(first_id) = self.spec_order.first() {
            let first_overrides = self
                .overrides
                .get(first_id)
                .expect("first registered measurement has an override contract");
            assert!(
                *first_overrides == overrides,
                "Benchmark {:?} recorded rows with different sample overrides during {}: first row {first_overrides:?}, measurement {:?} {overrides:?}; split rows with different samples/warmup/cooldown targets into separate #[stress] functions so no invocation is silently discarded",
                base_spec.name,
                sample_phase_label(phase),
                record.name,
            );
        }

        self.spec_order.push(benchmark_id.clone());
        self.specs.insert(benchmark_id.clone(), candidate);
        self.overrides.insert(benchmark_id, overrides);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MeasurementOverrideContract {
    measured: Option<usize>,
    warmup: Option<usize>,
    cooldown: Option<usize>,
}

impl MeasurementOverrideContract {
    fn from_record(record: &MeasurementRecord) -> Self {
        Self {
            measured: record.overrides.samples,
            warmup: record.overrides.warmup_samples,
            cooldown: record.overrides.cooldown_samples,
        }
    }

    fn target_for_phase(self, phase: SamplePhase, default: usize) -> usize {
        match phase {
            SamplePhase::Warmup => self.warmup.unwrap_or(default),
            SamplePhase::Measured => self.measured.unwrap_or(default),
            SamplePhase::Cooldown => self.cooldown.unwrap_or(default),
        }
    }
}

fn assert_measurement_names_match(
    base_spec: &BenchmarkSpec,
    phase: SamplePhase,
    expected: &[String],
    actual: &[String],
) {
    let expected_set = expected.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let actual_set = actual.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let missing = expected_set
        .difference(&actual_set)
        .copied()
        .collect::<Vec<_>>();
    let unexpected = actual_set
        .difference(&expected_set)
        .copied()
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty() && unexpected.is_empty(),
        "Benchmark {:?} changed measurement rows during {}: missing {missing:?}, unexpected {unexpected:?}; every invocation must record the same named rows, so keep conditional work inside stable measurements",
        base_spec.name,
        sample_phase_label(phase),
    );
    assert!(
        expected == actual,
        "Benchmark {:?} changed measurement order during {} from {expected:?} to {actual:?}; record named rows in deterministic order",
        base_spec.name,
        sample_phase_label(phase),
    );
}

fn assert_measurement_spec_matches(
    base_spec: &BenchmarkSpec,
    phase: SamplePhase,
    expected: &BenchmarkSpec,
    actual: &BenchmarkSpec,
) {
    let changed = if expected.mode != actual.mode {
        Some((
            "mode",
            format!("{:?}", expected.mode),
            format!("{:?}", actual.mode),
        ))
    } else if expected.intent != actual.intent {
        Some((
            "intent",
            expected.intent.to_string(),
            actual.intent.to_string(),
        ))
    } else if expected.parameters != actual.parameters {
        Some((
            "parameters",
            format!("{:?}", expected.parameters),
            format!("{:?}", actual.parameters),
        ))
    } else if expected.metadata != actual.metadata {
        Some((
            "metadata",
            format!("{:?}", expected.metadata),
            format!("{:?}", actual.metadata),
        ))
    } else {
        None
    };
    if let Some((field, expected_value, actual_value)) = changed {
        panic!(
            "Benchmark {:?} measurement {:?} changed {field} during {} from {expected_value} to {actual_value}; mode, intent, parameters, and metadata must be identical on every invocation",
            base_spec.name,
            actual.name,
            sample_phase_label(phase),
        );
    }
}

fn measurement_spec(
    base_spec: &BenchmarkSpec,
    record: &MeasurementRecord,
    benchmark_id: &str,
) -> BenchmarkSpec {
    let mut metadata = base_spec.metadata.clone();
    metadata.extend(record.metadata.clone());
    if !record.observations.is_empty() {
        let topology = record
            .observations
            .iter()
            .map(|observation| {
                format!(
                    "{}:{:?}:{:?}",
                    observation.name, observation.unit, observation.direction
                )
            })
            .collect::<Vec<_>>()
            .join("|");
        metadata.insert("cntryl_stress_observation_topology".to_string(), topology);
    }
    if record.mode.kind() == BenchmarkModeKind::Micro && record.intent == MeasurementIntent::Batch {
        metadata.insert(
            "ns_per_op_basis".to_string(),
            "logical_completed_operation".to_string(),
        );
    }
    let mut parameters = base_spec.parameters.clone();
    parameters.extend(record.parameters.clone());
    parameters
        .entry("measurement_mode".to_string())
        .or_insert_with(|| measurement_mode_label(record.mode.kind()).to_string());
    BenchmarkSpec {
        id: benchmark_id.to_string(),
        name: if record.metadata.contains_key("benchmark_error") {
            format!("{}::{}", base_spec.name, record.name)
        } else {
            record.name.clone()
        },
        tier: base_spec.tier,
        mode: record.mode.clone(),
        intent: record.intent,
        budgets: base_spec.budgets,
        parameters,
        metadata,
    }
}

const fn sample_phase_label(phase: SamplePhase) -> &'static str {
    match phase {
        SamplePhase::Warmup => "warmup",
        SamplePhase::Measured => "measured",
        SamplePhase::Cooldown => "cooldown",
    }
}

const fn measurement_mode_label(kind: BenchmarkModeKind) -> &'static str {
    match kind {
        BenchmarkModeKind::Micro => "micro",
        BenchmarkModeKind::FixedOperations => "fixed_ops",
        BenchmarkModeKind::FixedDuration => "duration",
    }
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
        allocator: allocator_label().to_string(),
        build_profile: std::env::var("STRESS_BUILD_INPUT_IDENTITY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                if cfg!(debug_assertions) {
                    "debug".to_string()
                } else {
                    "release".to_string()
                }
            }),
        git_commit: config.git_sha.clone().or_else(detect_git_sha),
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        command_line: std::env::args().collect(),
        profile_config: config.profile_config(),
    }
}

fn allocator_label() -> &'static str {
    if allocation::allocation_tracking_available() {
        "cntryl-stress allocator installed"
    } else {
        "cntryl-stress allocator not installed"
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

fn run_timestamp_stem() -> String {
    let now = std::time::SystemTime::now();
    let duration = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let epoch_nanos = duration.as_nanos();
    let process_id = std::process::id();
    let sequence = RUN_TIMESTAMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    // The fixed-width epoch prefix keeps stems lexically sortable. PID and a
    // process-local sequence make independently generated stems collision
    // resistant without relying on clock precision alone.
    format!("{epoch_nanos:020}-{process_id:010}-{sequence:020}")
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
    /// Strict diagnostic policy failed.
    DiagnosticsFailed,
    /// At least one configured benchmark budget failed.
    BudgetFailed,
    /// Canonical result publication failed.
    ArtifactFailed,
}

/// Evaluate a run against its profile policy.
#[must_use]
pub fn evaluate_run_gate(run: &StressRun) -> RunGate {
    if run.metadata.contains_key("reporter_errors") {
        return RunGate::ArtifactFailed;
    }
    if !run.correctness_passed() {
        return RunGate::CorrectnessFailed;
    }
    if !run.budgets_passed() {
        return RunGate::BudgetFailed;
    }
    if !run.regression_budgets_passed() {
        return RunGate::BudgetFailed;
    }
    if run.summaries.is_empty() {
        return RunGate::QualityFailed;
    }
    let profile_config = &run.environment.profile_config;
    let smoke_profile =
        run.run_profile == RunProfile::Smoke || profile_config.profile == RunProfile::Smoke;
    if !smoke_profile
        && run
            .summaries
            .iter()
            .any(|summary| summary.trust_class == crate::artifact::TrustClass::Invalid)
    {
        return RunGate::QualityFailed;
    }
    let performance_gate_enabled = (run.run_profile == RunProfile::Release
        || profile_config.profile == RunProfile::Release)
        || profile_config.fail_on_quality
        || profile_config.fail_on_regression;
    if performance_gate_enabled && !run.gate_obligations_satisfied() {
        return RunGate::QualityFailed;
    }
    // Supplying a baseline creates an obligation to compare every intended
    // gate against compatible evidence, even when actual regressions are only
    // report-only under the selected profile.
    if !run.rejected_gate_comparisons().is_empty() {
        return RunGate::RegressionFailed;
    }
    if profile_config.fail_on_regression && !run.regressions().is_empty() {
        return RunGate::RegressionFailed;
    }
    if profile_config
        .deny_diagnostics
        .is_some_and(|threshold| !run.diagnostics_passed(threshold))
    {
        return RunGate::DiagnosticsFailed;
    }
    if profile_config.fail_on_quality && !run.meets_min_quality(profile_config.min_quality) {
        return RunGate::QualityFailed;
    }
    RunGate::Passed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{
        BenchmarkBudgets, BenchmarkMode, BenchmarkModeKind, ComparisonClass, ComparisonResult,
        CorrectnessCounters, DiagnosticSeverity, PrimaryMetric, QualityClass, RunProfile,
        TrustClass,
    };
    use std::cell::Cell;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct FailingArtifactReporter;

    impl Reporter for FailingArtifactReporter {
        fn suite_end(&self, _run: &StressRun) -> std::io::Result<()> {
            Err(std::io::Error::other("injected artifact failure"))
        }
    }

    struct CapturingReceiptReporter {
        receipts: Arc<Mutex<Vec<String>>>,
    }

    impl Reporter for CapturingReceiptReporter {
        fn suite_end(&self, run: &StressRun) -> std::io::Result<()> {
            self.receipts
                .lock()
                .expect("capture receipt")
                .push(serde_json::to_string(run).map_err(std::io::Error::other)?);
            Ok(())
        }
    }

    struct CapturingGateReporter {
        gates: Arc<Mutex<Vec<RunGate>>>,
    }

    impl Reporter for CapturingGateReporter {
        fn suite_end(&self, run: &StressRun) -> std::io::Result<()> {
            self.gates
                .lock()
                .expect("capture gate")
                .push(evaluate_run_gate(run));
            Ok(())
        }
    }

    #[test]
    fn machine_receipt_is_deferred_until_artifact_failures_are_attached() {
        let receipts = Arc::new(Mutex::new(Vec::new()));
        let mut runner = StressRunner::with_config(
            "suite",
            StressRunnerConfig::for_profile(RunProfile::Smoke).json_stdout(true),
        );
        assert_eq!(runner.reporters.len(), 1);
        assert_eq!(runner.deferred_reporters.len(), 1);
        runner.reporters = vec![Box::new(FailingArtifactReporter)];
        runner.deferred_reporters = vec![Box::new(CapturingReceiptReporter {
            receipts: Arc::clone(&receipts),
        })];
        runner.run("bench", |ctx| {
            ctx.measure("work", || std::hint::black_box(1_u64));
        });

        let run = runner.finish();
        let receipts = receipts.lock().expect("read receipts");
        assert_eq!(receipts.len(), 1);
        let receipt: StressRun = serde_json::from_str(&receipts[0]).expect("JSON receipt");
        assert_eq!(
            receipt.metadata.get("reporter_errors"),
            Some(&"injected artifact failure".to_string())
        );
        assert_eq!(evaluate_run_gate(&receipt), RunGate::ArtifactFailed);
        assert_eq!(evaluate_run_gate(&run), RunGate::ArtifactFailed);
    }

    #[test]
    fn human_result_is_deferred_until_artifact_failures_are_attached() {
        let gates = Arc::new(Mutex::new(Vec::new()));
        let mut runner = StressRunner::with_config(
            "suite",
            StressRunnerConfig::for_profile(RunProfile::Smoke).progress(false),
        );
        assert_eq!(runner.reporters.len(), 1);
        assert_eq!(runner.deferred_reporters.len(), 1);
        runner.reporters = vec![Box::new(FailingArtifactReporter)];
        runner.deferred_reporters = vec![Box::new(CapturingGateReporter {
            gates: Arc::clone(&gates),
        })];
        runner.run("bench", |ctx| {
            ctx.measure("work", || std::hint::black_box(1_u64));
        });

        let run = runner.finish();
        assert_eq!(
            gates.lock().expect("read gates").as_slice(),
            &[RunGate::ArtifactFailed]
        );
        assert_eq!(evaluate_run_gate(&run), RunGate::ArtifactFailed);
    }

    #[test]
    fn generated_run_timestamp_stems_are_sortable_and_process_unique() {
        let first = run_timestamp_stem();
        let second = run_timestamp_stem();
        let first_parts = first.split('-').collect::<Vec<_>>();
        let second_parts = second.split('-').collect::<Vec<_>>();

        assert_eq!(first_parts.len(), 3);
        assert_eq!(second_parts.len(), 3);
        assert!(first_parts[0].len() >= 19);
        assert_eq!(
            first_parts[1].parse::<u32>().expect("process id"),
            std::process::id()
        );
        assert_eq!(
            second_parts[1].parse::<u32>().expect("process id"),
            std::process::id()
        );
        assert!(
            second_parts[2].parse::<u64>().expect("second sequence")
                > first_parts[2].parse::<u64>().expect("first sequence")
        );
        assert!(second > first);
    }

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
    #[should_panic(expected = "duplicate measurement name \"work\"")]
    fn duplicate_measurement_names_in_one_invocation_are_rejected() {
        let config = StressRunnerConfig::new()
            .samples(1)
            .warmup_samples(1)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("bench", |ctx| {
            ctx.measure("work", || {});
            ctx.measure("work", || {});
        });
    }

    #[test]
    #[should_panic(expected = "changed measurement rows")]
    fn disappearing_measurement_rows_are_rejected() {
        let invocation = Cell::new(0);
        let config = StressRunnerConfig::new()
            .samples(2)
            .warmup_samples(1)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("bench", |ctx| {
            let invocation = invocation.replace(invocation.get() + 1);
            ctx.measure("always", || {});
            if invocation == 0 {
                ctx.measure("conditional", || {});
            }
        });
    }

    #[test]
    #[should_panic(expected = "changed measurement rows")]
    fn appearing_measurement_rows_are_rejected() {
        let invocation = Cell::new(0);
        let config = StressRunnerConfig::new()
            .samples(2)
            .warmup_samples(1)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("bench", |ctx| {
            let invocation = invocation.replace(invocation.get() + 1);
            ctx.measure("always", || {});
            if invocation != 0 {
                ctx.measure("conditional", || {});
            }
        });
    }

    #[test]
    #[should_panic(expected = "changed intent")]
    fn changing_measurement_intent_is_rejected() {
        let invocation = Cell::new(0);
        let config = StressRunnerConfig::new()
            .samples(1)
            .warmup_samples(1)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("bench", |ctx| {
            let invocation = invocation.replace(invocation.get() + 1);
            let intent = if invocation == 0 {
                MeasurementIntent::General
            } else {
                MeasurementIntent::Io
            };
            ctx.benchmark("work").intent(intent).measure(|| {});
        });
    }

    #[test]
    #[should_panic(expected = "changed parameters")]
    fn changing_measurement_parameters_are_rejected() {
        let invocation = Cell::new(0);
        let config = StressRunnerConfig::new()
            .samples(1)
            .warmup_samples(1)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("bench", |ctx| {
            let invocation = invocation.replace(invocation.get() + 1);
            ctx.benchmark("work")
                .parameter("variant", invocation)
                .measure(|| {});
        });
    }

    #[test]
    #[should_panic(expected = "changed metadata")]
    fn changing_measurement_metadata_is_rejected() {
        let invocation = Cell::new(0);
        let config = StressRunnerConfig::new()
            .samples(1)
            .warmup_samples(1)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("bench", |ctx| {
            let invocation = invocation.replace(invocation.get() + 1);
            ctx.benchmark("work")
                .metadata("variant", invocation)
                .measure(|| {});
        });
    }

    #[test]
    #[should_panic(expected = "changed mode")]
    fn changing_measurement_mode_is_rejected() {
        let base_spec = BenchmarkSpec {
            id: "suite/bench".to_string(),
            name: "bench".to_string(),
            tier: 2,
            mode: BenchmarkMode::FixedOperations {
                operations_per_sample: 1,
            },
            intent: MeasurementIntent::General,
            budgets: BenchmarkBudgets::default(),
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
        };
        let mut changed_mode = base_spec.clone();
        changed_mode.mode = BenchmarkMode::FixedOperations {
            operations_per_sample: 2,
        };
        let (first, _, _) = invoke_benchmark(&base_spec, &|ctx| ctx.measure("work", || {}));
        let (changed, _, _) = invoke_benchmark(&changed_mode, &|ctx| ctx.measure("work", || {}));
        let mut topology = MeasurementTopology::default();

        topology.validate_invocation(&base_spec, SamplePhase::Warmup, &first);
        topology.validate_invocation(&base_spec, SamplePhase::Measured, &changed);
    }

    #[test]
    #[should_panic(expected = "changed sample overrides")]
    fn changing_measurement_sample_overrides_is_rejected() {
        let invocation = Cell::new(0);
        let config = StressRunnerConfig::new()
            .samples(1)
            .warmup_samples(1)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("bench", |ctx| {
            let invocation = invocation.replace(invocation.get() + 1);
            ctx.benchmark("work").samples(invocation + 1).measure(|| {});
        });
    }

    #[test]
    #[should_panic(expected = "rows with different sample overrides")]
    fn rows_with_different_sample_targets_are_rejected() {
        let config = StressRunnerConfig::new()
            .samples(2)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("bench", |ctx| {
            ctx.benchmark("short").samples(1).measure(|| {});
            ctx.benchmark("long").samples(2).measure(|| {});
        });
    }

    #[test]
    fn row_overrides_can_enable_zero_default_cooldown_phase() {
        let config = StressRunnerConfig::new()
            .samples(1)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("bench", |ctx| {
            ctx.benchmark("work").cooldown(1).measure(|| {});
        });
        let run = runner.finish();

        assert_eq!(run.summaries[0].warmup_samples, 0);
        assert_eq!(run.summaries[0].measured_samples, 1);
        assert_eq!(run.summaries[0].cooldown_samples, 1);
    }

    #[test]
    #[should_panic(expected = "cannot enable warmup after the suite's warmup count disabled")]
    fn row_override_cannot_hide_warmup_work_when_suite_warmup_is_zero() {
        let config = StressRunnerConfig::new()
            .samples(1)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("bench", |ctx| {
            ctx.benchmark("work").warmup(2).measure(|| {});
        });
    }

    #[test]
    fn zero_warmup_does_not_invoke_unrecorded_work() {
        let invocations = Cell::new(0_u64);
        let config = StressRunnerConfig::new()
            .samples(1)
            .warmup_samples(0)
            .cooldown_samples(0)
            .operations_per_sample(3);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("bench", |ctx| {
            ctx.measure("work", || {
                invocations.set(invocations.get() + 1);
            });
        });
        let run = runner.finish();

        assert_eq!(invocations.get(), 3);
        assert_eq!(run.samples.len(), 1);
        assert_eq!(run.samples[0].operations_attempted, 3);
    }

    #[test]
    fn captured_environment_labels_allocator_installation_state() {
        let environment = capture_environment(&StressRunnerConfig::new());
        let expected = if crate::allocation::allocation_tracking_available() {
            "cntryl-stress allocator installed"
        } else {
            "cntryl-stress allocator not installed"
        };

        assert_eq!(environment.allocator, expected);
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
        assert_eq!(evaluate_run_gate(&run), RunGate::QualityFailed);
    }

    #[test]
    fn empty_programmatic_run_fails_closed() {
        let mut runner = StressRunner::with_config("suite", StressRunnerConfig::new());
        runner.reporters(Vec::new());

        assert_eq!(evaluate_run_gate(&runner.finish()), RunGate::QualityFailed);
    }

    #[test]
    fn invalid_measurement_trust_fails_even_the_default_profile() {
        let mut run = warning_diagnostic_run(None);
        run.summaries[0].trust_class = TrustClass::Invalid;

        assert_eq!(evaluate_run_gate(&run), RunGate::QualityFailed);
    }

    #[test]
    fn smoke_is_an_explicit_diagnostic_override_for_one_sample_rows() {
        let config = StressRunnerConfig::for_profile(RunProfile::Smoke);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());
        runner.run("bench", |ctx| {
            ctx.measure("work", || std::hint::black_box(1_u64));
        });

        let run = runner.finish();

        assert_eq!(run.summaries[0].trust_class, TrustClass::Invalid);
        assert_eq!(evaluate_run_gate(&run), RunGate::Passed);
    }

    #[test]
    #[should_panic(expected = "registered more than once")]
    fn duplicate_benchmark_ids_are_rejected_before_publication() {
        let config = StressRunnerConfig::new()
            .samples(1)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("same", |ctx| ctx.measure("work", || {}));
        runner.run("same", |ctx| ctx.measure("work", || {}));
    }

    #[test]
    fn programmatic_specs_reject_invalid_shape_and_budgets() {
        let spec = BenchmarkSpec {
            id: " ".to_string(),
            name: String::new(),
            tier: 2,
            mode: BenchmarkMode::FixedOperations {
                operations_per_sample: 0,
            },
            intent: MeasurementIntent::General,
            budgets: BenchmarkBudgets {
                max_ns_per_op: Some(f64::NAN),
                max_regression_pct: Some(101.0),
                ..BenchmarkBudgets::default()
            },
            parameters: BTreeMap::new(),
            metadata: BTreeMap::from([("trust_class".to_string(), "gatte".to_string())]),
        };

        let errors = benchmark_spec_validation_errors(&spec);
        assert!(errors.iter().any(|error| error == "id must not be empty"));
        assert!(errors.iter().any(|error| error == "name must not be empty"));
        assert!(errors
            .iter()
            .any(|error| error == "fixed operations_per_sample must be greater than 0"));
        assert!(errors.iter().any(|error| error.contains("max_ns_per_op")));
        assert!(errors
            .iter()
            .any(|error| error.contains("max_regression_pct")));
        assert!(errors
            .iter()
            .any(|error| error.contains("metadata trust_class")));
    }

    #[test]
    #[should_panic(expected = "suite name must not be empty")]
    fn empty_suite_names_are_rejected() {
        let _ = StressRunner::with_config("  ", StressRunnerConfig::new());
    }

    #[test]
    #[should_panic(expected = "filesystem dot segment")]
    fn suite_names_cannot_escape_the_output_directory() {
        let _ = StressRunner::with_config("..", StressRunnerConfig::new());
    }

    #[test]
    #[should_panic(expected = "only ASCII letters")]
    fn suite_names_must_be_portable_path_components() {
        let _ = StressRunner::with_config("package/suite", StressRunnerConfig::new());
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
            budgets: crate::artifact::BenchmarkBudgets::default(),
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
            budgets: crate::artifact::BenchmarkBudgets::default(),
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
    fn returned_benchmark_error_becomes_a_structured_failing_row() {
        let config = StressRunnerConfig::for_profile(RunProfile::Release)
            .samples(2)
            .warmup_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("fallible", |_ctx| Err::<(), _>("transport failed"));
        let run = runner.finish();

        assert_eq!(run.summaries.len(), 1);
        assert_eq!(run.summaries[0].name, "fallible::benchmark error");
        assert_eq!(
            run.summaries[0].metadata.get("benchmark_error"),
            Some(&"transport failed".to_string())
        );
        assert!(!run.summaries[0].correctness.passed);
        assert_eq!(run.summaries[0].quality, QualityClass::Untrustworthy);
        assert_eq!(run.summaries[0].trust_class, TrustClass::Invalid);
        assert_eq!(evaluate_run_gate(&run), RunGate::CorrectnessFailed);
    }

    #[test]
    fn first_transient_function_error_aborts_with_one_stable_error_row() {
        let invocation = Cell::new(0_u64);
        let config = StressRunnerConfig::new()
            .samples(2)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("fallible", |ctx| {
            let invocation = invocation.replace(invocation.get() + 1);
            if invocation == 0 {
                Err("first setup attempt failed")
            } else {
                ctx.measure("recovered work", || {});
                Ok(())
            }
        });
        let run = runner.finish();

        assert_eq!(invocation.get(), 1);
        assert_eq!(run.samples.len(), 1);
        assert_eq!(run.summaries.len(), 1);
        assert_eq!(run.summaries[0].name, "fallible::benchmark error");
        assert_eq!(
            run.summaries[0].metadata.get("benchmark_error"),
            Some(&"first setup attempt failed".to_string())
        );
        assert_eq!(evaluate_run_gate(&run), RunGate::CorrectnessFailed);
    }

    #[test]
    fn measured_result_error_preserves_named_timing_and_observed_counters() {
        let config = StressRunnerConfig::new()
            .samples(2)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("fallible", |ctx| {
            ctx.measure_result("operation", || Err::<(), _>("operation failed"))?;
            Ok::<(), &str>(())
        });
        let run = runner.finish();

        assert_eq!(run.samples.len(), 1);
        assert_eq!(run.samples[0].operations_attempted, 1);
        assert_eq!(run.samples[0].operations_completed, 0);
        assert_eq!(run.samples[0].counters.failures, 1);
        assert_eq!(run.summaries.len(), 1);
        assert_eq!(run.summaries[0].name, "fallible::operation");
        assert_eq!(
            run.summaries[0].metadata.get("benchmark_error"),
            Some(&"operation failed".to_string())
        );
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
        let mut run = runner.finish();
        run.summaries[0].quality = QualityClass::Noisy;
        run.summaries[0].trust_class = TrustClass::Gate;

        assert_eq!(evaluate_run_gate(&run), RunGate::QualityFailed);
    }

    #[test]
    fn quality_or_regression_policy_requires_at_least_one_intended_gate() {
        for (fail_on_quality, fail_on_regression) in [(true, false), (false, true)] {
            let mut run = warning_diagnostic_run(None);
            run.summaries.clear();
            run.environment.profile_config.fail_on_quality = fail_on_quality;
            run.environment.profile_config.fail_on_regression = fail_on_regression;

            assert_eq!(evaluate_run_gate(&run), RunGate::QualityFailed);
        }

        let mut release = warning_diagnostic_run(None);
        release.run_profile = RunProfile::Release;
        release.environment.profile_config.fail_on_quality = false;
        release.environment.profile_config.fail_on_regression = false;
        release.summaries[0].trust_class = TrustClass::Diagnostic;
        release.summaries[0]
            .metadata
            .insert("trust_class".to_string(), "diagnostic".to_string());

        assert_eq!(evaluate_run_gate(&release), RunGate::QualityFailed);
    }

    #[test]
    fn artifact_publication_error_fails_the_run_gate() {
        let mut run = warning_diagnostic_run(None);
        run.metadata.insert(
            "reporter_errors".to_string(),
            "permission denied".to_string(),
        );

        assert_eq!(evaluate_run_gate(&run), RunGate::ArtifactFailed);
    }

    #[test]
    fn intended_gates_fail_when_derived_trust_is_downgraded() {
        for trust_class in [
            TrustClass::Diagnostic,
            TrustClass::Experimental,
            TrustClass::Invalid,
        ] {
            let mut run = warning_diagnostic_run(None);
            run.environment.profile_config.fail_on_quality = true;
            run.environment.profile_config.min_quality = QualityClass::Acceptable;
            run.summaries[0].quality = QualityClass::Authoritative;
            run.summaries[0].trust_class = trust_class;
            run.summaries[0].metadata.remove("trust_class");

            assert_eq!(evaluate_run_gate(&run), RunGate::QualityFailed);
        }
    }

    #[test]
    fn explicit_diagnostic_rows_do_not_create_gate_obligations() {
        let mut run = warning_diagnostic_run(None);
        run.environment.profile_config.fail_on_quality = true;
        run.environment.profile_config.min_quality = QualityClass::Acceptable;
        run.summaries[0].quality = QualityClass::Authoritative;
        run.summaries[0].trust_class = TrustClass::Gate;

        let mut diagnostic = run.summaries[0].clone();
        diagnostic.benchmark_id.push_str("/diagnostic");
        diagnostic.name.push_str(" diagnostic");
        diagnostic.quality = QualityClass::Noisy;
        diagnostic.trust_class = TrustClass::Diagnostic;
        diagnostic
            .metadata
            .insert("trust_class".to_string(), "diagnostic".to_string());
        run.summaries.push(diagnostic);

        assert_eq!(evaluate_run_gate(&run), RunGate::Passed);
    }

    #[test]
    fn regression_policy_fails_rejected_supplied_baselines_but_allows_no_baseline() {
        let mut run = warning_diagnostic_run(None);
        run.environment.profile_config.fail_on_regression = true;
        run.summaries[0].quality = QualityClass::Acceptable;
        run.summaries[0].trust_class = TrustClass::Gate;

        assert_eq!(evaluate_run_gate(&run), RunGate::Passed);

        let benchmark_id = run.summaries[0].benchmark_id.clone();
        run.comparisons.push(ComparisonResult {
            benchmark_id,
            current_quality: QualityClass::Acceptable,
            baseline_quality: None,
            primary_metric: PrimaryMetric::Throughput,
            baseline_value: None,
            current_value: Some(100.0),
            change_percent: None,
            threshold: 0.05,
            confidence_intervals_overlap: None,
            classification: ComparisonClass::MissingBaseline,
            reason: Some("no exact baseline id".to_string()),
        });
        assert_eq!(evaluate_run_gate(&run), RunGate::RegressionFailed);

        run.comparisons[0].classification = ComparisonClass::Inconclusive;
        run.comparisons[0].reason = Some("baseline parameters changed".to_string());
        assert_eq!(evaluate_run_gate(&run), RunGate::RegressionFailed);

        run.environment.profile_config.fail_on_regression = false;
        assert_eq!(evaluate_run_gate(&run), RunGate::RegressionFailed);
    }

    #[test]
    fn explicit_regression_budget_is_enforced_without_profile_regression_policy() {
        let mut run = warning_diagnostic_run(None);
        run.summaries[0].budgets.max_regression_pct = Some(5.0);
        let benchmark_id = run.summaries[0].benchmark_id.clone();
        run.comparisons.push(ComparisonResult {
            benchmark_id,
            current_quality: QualityClass::Acceptable,
            baseline_quality: Some(QualityClass::Acceptable),
            primary_metric: PrimaryMetric::Throughput,
            baseline_value: Some(100.0),
            current_value: Some(80.0),
            change_percent: Some(-20.0),
            threshold: 0.05,
            confidence_intervals_overlap: Some(false),
            classification: ComparisonClass::Regression,
            reason: None,
        });

        assert!(!run.environment.profile_config.fail_on_regression);
        assert_eq!(evaluate_run_gate(&run), RunGate::BudgetFailed);

        run.comparisons.clear();
        assert_eq!(evaluate_run_gate(&run), RunGate::Passed);
    }

    #[test]
    fn explicit_regression_budget_rejects_incompatible_supplied_baseline() {
        let mut run = warning_diagnostic_run(None);
        run.summaries[0].budgets.max_regression_pct = Some(5.0);
        let benchmark_id = run.summaries[0].benchmark_id.clone();
        run.comparisons.push(ComparisonResult {
            benchmark_id,
            current_quality: QualityClass::Acceptable,
            baseline_quality: Some(QualityClass::Acceptable),
            primary_metric: PrimaryMetric::Throughput,
            baseline_value: Some(100.0),
            current_value: Some(100.0),
            change_percent: None,
            threshold: 0.05,
            confidence_intervals_overlap: None,
            classification: ComparisonClass::Inconclusive,
            reason: Some("parameters changed".to_string()),
        });

        assert_eq!(evaluate_run_gate(&run), RunGate::BudgetFailed);
    }

    fn warning_diagnostic_run(threshold: Option<DiagnosticSeverity>) -> StressRun {
        let mut config = StressRunnerConfig::new()
            .samples(2)
            .warmup_samples(0)
            .cooldown_samples(0);
        config.deny_diagnostics = threshold;
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("bench", |ctx| {
            ctx.parameter("clients", 4);
            ctx.measure("work", || std::hint::black_box(1_u64));
        });
        runner.finish()
    }

    #[test]
    fn strict_diagnostics_gate_uses_configured_threshold() {
        assert_eq!(
            evaluate_run_gate(&warning_diagnostic_run(None)),
            RunGate::Passed
        );
        assert_eq!(
            evaluate_run_gate(&warning_diagnostic_run(Some(DiagnosticSeverity::Info))),
            RunGate::DiagnosticsFailed
        );
        assert_eq!(
            evaluate_run_gate(&warning_diagnostic_run(Some(DiagnosticSeverity::Warning))),
            RunGate::DiagnosticsFailed
        );
        assert_eq!(
            evaluate_run_gate(&warning_diagnostic_run(Some(DiagnosticSeverity::Error))),
            RunGate::Passed
        );
    }

    #[test]
    fn diagnostics_summary_mirrors_summary_diagnostics() {
        let run = warning_diagnostic_run(None);
        let summary = &run.summaries[0];

        assert_eq!(run.diagnostics_summary.len(), summary.diagnostics.len());
        assert!(run.diagnostics_summary.iter().any(|diagnostic| {
            diagnostic.suite == "suite"
                && diagnostic.benchmark_id == summary.benchmark_id
                && diagnostic.name == summary.name
                && diagnostic.tier == summary.tier
                && diagnostic.quality == summary.quality
                && diagnostic.parameters.get("clients") == Some(&"4".to_string())
                && diagnostic.code == "too_few_samples"
        }));
    }

    fn external_throughput_runner(completed_operations: u64) -> StressRunner {
        external_throughput_runner_with_operations(completed_operations, 1)
    }

    fn external_throughput_runner_with_operations(
        completed_operations: u64,
        operations_per_sample: u64,
    ) -> StressRunner {
        let config = StressRunnerConfig::new()
            .samples(10)
            .warmup_samples(0)
            .cooldown_samples(0)
            .operations_per_sample(operations_per_sample);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());
        runner.run("bench", |ctx| {
            ctx.record_external("work", Duration::from_millis(10), completed_operations);
        });
        runner
    }

    #[test]
    fn supplied_baseline_with_different_concrete_mode_fails_regression_policy() {
        let baseline = external_throughput_runner_with_operations(1_000, 1).finish();
        let baseline_path = unique_temp_path("stress-baseline-mode-change.json");
        std::fs::write(
            &baseline_path,
            serde_json::to_string(&baseline).expect("serialize baseline"),
        )
        .expect("write baseline");

        let mut run = external_throughput_runner_with_operations(100, 2)
            .finish_with_baseline(&baseline_path)
            .expect("finish with baseline");
        run.environment.profile_config.fail_on_regression = true;

        assert_eq!(run.comparisons.len(), 1);
        assert_eq!(
            run.comparisons[0].classification,
            ComparisonClass::Inconclusive
        );
        assert_eq!(run.comparisons[0].change_percent, None);
        assert!(run.comparisons[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("benchmark mode changed")));
        assert_eq!(evaluate_run_gate(&run), RunGate::RegressionFailed);

        let _ = std::fs::remove_file(&baseline_path);
    }

    #[test]
    fn supplied_baseline_from_incompatible_environment_fails_regression_policy() {
        let mut baseline = external_throughput_runner(1_000).finish();
        baseline.environment.cpu_model = "baseline test cpu".to_string();
        for sample in &mut baseline.samples {
            sample.environment.cpu_model = "baseline test cpu".to_string();
        }
        let baseline_path = unique_temp_path("stress-baseline-environment-change.json");
        std::fs::write(
            &baseline_path,
            serde_json::to_string(&baseline).expect("serialize baseline"),
        )
        .expect("write baseline");

        let mut current = external_throughput_runner(100);
        current.environment.cpu_model = "current test cpu".to_string();
        let mut run = current
            .finish_with_baseline(&baseline_path)
            .expect("finish with baseline");
        run.environment.profile_config.fail_on_regression = true;

        assert_eq!(run.comparisons.len(), 1);
        assert_eq!(
            run.comparisons[0].classification,
            ComparisonClass::Inconclusive
        );
        assert_eq!(run.comparisons[0].change_percent, None);
        assert!(run.comparisons[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("CPU model differs")));
        assert_eq!(evaluate_run_gate(&run), RunGate::RegressionFailed);

        let _ = std::fs::remove_file(&baseline_path);
    }

    #[test]
    fn finish_with_baseline_rejects_a_tampered_serialized_summary() {
        let mut baseline = external_throughput_runner(1_000).finish();
        baseline.summaries[0]
            .stats
            .as_mut()
            .expect("baseline stats")
            .mean *= 10.0;
        let baseline_path = unique_temp_path("stress-baseline-tampered-summary.json");
        std::fs::write(
            &baseline_path,
            serde_json::to_string(&baseline).expect("serialize baseline"),
        )
        .expect("write baseline");

        let error = external_throughput_runner(100)
            .finish_with_baseline(&baseline_path)
            .expect_err("tampered serialized summary must be rejected");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("canonical raw samples"));

        let _ = std::fs::remove_file(&baseline_path);
    }

    #[test]
    fn finish_with_baseline_rejects_a_run_whose_recorded_gate_failed() {
        let root_baseline = external_throughput_runner(1_000).finish();
        let root_path = unique_temp_path("stress-baseline-passed-root.json");
        std::fs::write(
            &root_path,
            serde_json::to_string(&root_baseline).expect("serialize root baseline"),
        )
        .expect("write root baseline");

        let mut failed_run = external_throughput_runner(100)
            .finish_with_baseline(&root_path)
            .expect("build regressed run");
        failed_run.environment.profile_config.fail_on_regression = true;
        assert_eq!(evaluate_run_gate(&failed_run), RunGate::RegressionFailed);
        let failed_path = unique_temp_path("stress-baseline-failed-gate.json");
        std::fs::write(
            &failed_path,
            serde_json::to_string(&failed_run).expect("serialize failed run"),
        )
        .expect("write failed run");

        let error = external_throughput_runner(100)
            .finish_with_baseline(&failed_path)
            .expect_err("a failed run must not become a ratcheted baseline");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("recorded gate"));
        assert!(error.to_string().contains("RegressionFailed"));

        let _ = std::fs::remove_file(&root_path);
        let _ = std::fs::remove_file(&failed_path);
    }

    #[test]
    fn diagnostics_summary_includes_regression_diagnostics() {
        let baseline = external_throughput_runner(1_000).finish();
        let baseline_path = unique_temp_path("stress-baseline-regression.json");
        std::fs::write(
            &baseline_path,
            serde_json::to_string(&baseline).expect("serialize baseline"),
        )
        .expect("write baseline");

        let run = external_throughput_runner(100)
            .finish_with_baseline(&baseline_path)
            .expect("finish with baseline");

        assert!(run
            .comparisons
            .iter()
            .any(|comparison| comparison.classification == ComparisonClass::Regression));
        assert!(run
            .summaries
            .iter()
            .flat_map(|summary| &summary.diagnostics)
            .any(|diagnostic| diagnostic.code == "regression"));
        assert!(run
            .diagnostics_summary
            .iter()
            .any(|diagnostic| diagnostic.code == "regression"
                && diagnostic.severity == DiagnosticSeverity::Error));

        let _ = std::fs::remove_file(&baseline_path);
    }

    fn unique_temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ))
    }

    #[test]
    fn regression_gate_precedes_diagnostics_gate() {
        let mut run = warning_diagnostic_run(Some(DiagnosticSeverity::Warning));
        run.environment.profile_config.fail_on_regression = true;
        run.summaries[0].quality = QualityClass::Acceptable;
        run.summaries[0].trust_class = TrustClass::Gate;
        run.comparisons.push(ComparisonResult {
            benchmark_id: run.summaries[0].benchmark_id.clone(),
            current_quality: QualityClass::Acceptable,
            baseline_quality: Some(QualityClass::Acceptable),
            primary_metric: PrimaryMetric::Throughput,
            baseline_value: Some(100.0),
            current_value: Some(50.0),
            change_percent: Some(-50.0),
            threshold: 0.05,
            confidence_intervals_overlap: Some(false),
            classification: ComparisonClass::Regression,
            reason: None,
        });

        assert_eq!(evaluate_run_gate(&run), RunGate::RegressionFailed);
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
            budgets: crate::artifact::BenchmarkBudgets::default(),
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

    #[test]
    fn tier1_batch_ns_per_op_uses_logical_completed_operations() {
        let config = StressRunnerConfig::for_profile(RunProfile::Smoke)
            .micro_sample_duration(Duration::from_millis(1));
        let mut runner = StressRunner::with_config("suite", config.clone());
        runner.reporters(Vec::new());
        let spec = BenchmarkSpec {
            id: "suite/batch".to_string(),
            name: "batch".to_string(),
            tier: 1,
            mode: config.mode_for_kind(BenchmarkModeKind::Micro),
            intent: MeasurementIntent::General,
            budgets: BenchmarkBudgets::default(),
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
        };

        runner.run_spec(&spec, |ctx| {
            let completed = ctx.measure_batch("batch", 8, || std::hint::black_box(1_u64));
            assert!(completed >= 8);
        });
        let run = runner.finish();
        let sample = &run.samples[0];

        assert!(sample.calibrated_iterations.expect("iterations") > 0);
        assert_eq!(
            sample.operations_completed,
            sample.calibrated_iterations.expect("iterations") * 8
        );
        assert_eq!(
            run.summaries[0].metadata.get("ns_per_op_basis"),
            Some(&"logical_completed_operation".to_string())
        );
        assert_eq!(
            sample.net_ns_per_op,
            sample
                .net_elapsed_ns
                .and_then(|elapsed| ns_per_op(elapsed, sample.operations_completed))
        );
    }

    #[test]
    fn runner_records_stress_run_id_metadata_from_env() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let previous = std::env::var("STRESS_RUN_ID").ok();
        std::env::set_var("STRESS_RUN_ID", "run-123");
        let mut runner = StressRunner::with_config("suite", StressRunnerConfig::new());
        runner.reporters(Vec::new());

        runner.run("bench", |ctx| {
            ctx.measure("work", || std::hint::black_box(1_u64));
        });
        let run = runner.finish();
        if let Some(previous) = previous {
            std::env::set_var("STRESS_RUN_ID", previous);
        } else {
            std::env::remove_var("STRESS_RUN_ID");
        }

        assert_eq!(run.metadata.get("run_id"), Some(&"run-123".to_string()));
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
        assert!(failing
            .diagnostics_summary
            .iter()
            .any(|diagnostic| diagnostic.code == "budget_failure"));
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
