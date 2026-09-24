//! Stress runner that records raw samples and derives current artifacts.

use crate::allocation;
use crate::artifact::{
    attach_measurement_mode_mismatch_diagnostics, attach_regression_diagnostics,
    attach_timer_resolution_evidence, compare_summaries_with_specs, diagnostic_summary_for_run,
    incompatible_environment_reason, pooled_baseline_summaries, summarize_benchmark,
    BenchmarkModeKind, BenchmarkSpec, BenchmarkSummary, ComparisonClass, ComparisonResult,
    ConfirmationRun, EnvironmentInfo, MeasurementIntent, RunProfile, Sample, SamplePhase,
    SourceLocation, StressRun, MAX_TIER, SCHEMA_VERSION, SUMMARY_SEMANTICS_CURRENT,
    SUMMARY_SEMANTICS_METADATA_KEY,
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
    base_specs: BTreeMap<String, BenchmarkSpec>,
    measurement_bases: BTreeMap<String, String>,
    confirmation_runs: Vec<ConfirmationRun>,
}

/// Baseline evidence pooled from one or more environment-compatible runs.
///
/// The anchor run (the configured baseline) supplies the specs and
/// environment; extra runs contribute raw samples for rows whose spec is
/// identical, and summaries are recomputed from the pooled raw samples.
#[derive(Debug, Clone)]
pub(crate) struct BaselinePool {
    pub(crate) summaries: Vec<BenchmarkSummary>,
    pub(crate) specs: Vec<BenchmarkSpec>,
    pub(crate) environment: EnvironmentInfo,
    /// `started_at` of every pooled run, anchor first.
    pub(crate) pooled_runs: Vec<String>,
    /// Why candidate runs (or rows of them) were left out of the pool.
    pub(crate) skipped: Vec<String>,
    /// Whether any extra run was considered, so single-run artifacts stay
    /// byte-for-byte unchanged.
    pub(crate) considered_extras: bool,
}

/// Load a baseline run that may anchor or join a pool: it must parse, carry a
/// passed recorded gate, and have serialized summaries matching its raw
/// samples.
fn load_eligible_baseline(path: &Path) -> std::io::Result<(StressRun, Vec<BenchmarkSummary>)> {
    let baseline = StressRun::load(path)?;
    let baseline_gate = evaluate_run_gate(&baseline);
    if baseline_gate != RunGate::Passed {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "baseline run is not eligible because its recorded gate evaluates to {baseline_gate:?}; use a passed run saved with --save-baseline"
            ),
        ));
    }
    let summaries = baseline
        .canonical_baseline_summaries()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    Ok((baseline, summaries))
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
            base_specs: BTreeMap::new(),
            measurement_bases: BTreeMap::new(),
            confirmation_runs: Vec::new(),
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
    ///
    /// Each new reporter receives `suite_start` immediately, so it observes
    /// exactly one suite start before any benchmark events.
    pub fn reporters(&mut self, reporters: Vec<Box<dyn Reporter>>) -> &mut Self {
        for reporter in &reporters {
            reporter.suite_start(&self.suite, &self.config);
        }
        self.reporters = reporters;
        self.deferred_reporters.clear();
        self
    }

    /// Add a reporter.
    ///
    /// The reporter receives `suite_start` immediately, so it observes exactly
    /// one suite start before any benchmark events.
    pub fn add_reporter(&mut self, reporter: Box<dyn Reporter>) -> &mut Self {
        reporter.suite_start(&self.suite, &self.config);
        self.reporters.push(reporter);
        self
    }

    /// Run a Tier 2 fixed-operations benchmark with low ceremony.
    ///
    /// The caller's location becomes the rows' fallback source location.
    #[track_caller]
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
        let source = SourceLocation::from_std(std::panic::Location::caller());
        self.run_spec_at(&spec, Some(&source), f);
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
        self.run_spec_at(spec, None, f);
    }

    /// Run a benchmark spec whose rows fall back to `source` when they did
    /// not record their own location.
    pub(crate) fn run_spec_at<F, O>(
        &mut self,
        spec: &BenchmarkSpec,
        source: Option<&SourceLocation>,
        f: F,
    ) where
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
        self.base_specs.insert(spec.id.clone(), spec.clone());

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
            mut sources,
            ..
        } = topology;
        let base_id = spec.id.clone();
        let peak_rss_bytes = crate::memory::peak_rss_bytes();
        for spec_id in spec_order {
            let spec = specs
                .remove(&spec_id)
                .expect("spec order contains known ids");
            let mut summary = summarize_benchmark(&spec, &self.samples[start_sample..]);
            summary.source = sources.remove(&spec_id).or_else(|| source.cloned());
            summary.peak_rss_bytes = peak_rss_bytes;
            crate::artifact::attach_peak_rss_diagnostic(&mut summary);
            for reporter in &self.reporters {
                reporter.bench_end(&summary);
            }
            self.measurement_bases
                .insert(spec.id.clone(), base_id.clone());
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
        let pool = self.load_baseline_pool(baseline_path.as_ref(), &[], 1)?;
        Ok(self.finish_with_baseline_pool(&pool))
    }

    /// Load `anchor` plus up to `max_runs - 1` distinct extra baseline runs.
    ///
    /// The anchor must be eligible exactly as for [`Self::finish_with_baseline`].
    /// An extra run joins the pool only when it is eligible, has a different
    /// `started_at` than every pooled run, shares the anchor's run profile, and
    /// its environment is compatible with this run; rows whose spec differs
    /// from the anchor's are left out. Everything left out is noted in
    /// `skipped` instead of failing the run.
    pub(crate) fn load_baseline_pool(
        &self,
        anchor: &Path,
        extras: &[std::path::PathBuf],
        max_runs: usize,
    ) -> std::io::Result<BaselinePool> {
        let (anchor_run, anchor_summaries) = load_eligible_baseline(anchor)?;
        let mut pool = BaselinePool {
            summaries: anchor_summaries,
            specs: anchor_run.benchmark_specs.clone(),
            environment: anchor_run.environment.clone(),
            pooled_runs: vec![anchor_run.started_at.clone()],
            skipped: Vec::new(),
            considered_extras: !extras.is_empty(),
        };
        let mut pooled = anchor_run;
        for path in extras {
            if pool.pooled_runs.len() >= max_runs.max(1) {
                break;
            }
            let run = match load_eligible_baseline(path) {
                Ok((run, _)) => run,
                Err(error) => {
                    pool.skipped.push(format!("{}: {error}", path.display()));
                    continue;
                }
            };
            if pool.pooled_runs.contains(&run.started_at) {
                // `latest` is a copy of the newest timestamped run.
                continue;
            }
            if let Some(reason) =
                incompatible_environment_reason(&self.environment, &run.environment)
            {
                pool.skipped.push(format!("{}: {reason}", path.display()));
                continue;
            }
            if run.run_profile != pooled.run_profile {
                pool.skipped.push(format!(
                    "{}: run profile {:?} differs from the baseline's {:?}",
                    path.display(),
                    run.run_profile,
                    pooled.run_profile
                ));
                continue;
            }
            let mut matching_ids = BTreeSet::new();
            for spec in &run.benchmark_specs {
                if pooled.benchmark_specs.contains(spec) {
                    matching_ids.insert(spec.id.as_str());
                } else if pooled
                    .benchmark_specs
                    .iter()
                    .any(|candidate| candidate.id == spec.id)
                {
                    pool.skipped.push(format!(
                        "{}: row {:?} has a different benchmark spec",
                        path.display(),
                        spec.id
                    ));
                }
            }
            pooled.samples.extend(
                run.samples
                    .iter()
                    .filter(|sample| matching_ids.contains(sample.benchmark_id.as_str()))
                    .cloned(),
            );
            pool.pooled_runs.push(run.started_at.clone());
        }
        if pool.pooled_runs.len() > 1 {
            pool.summaries = pooled_baseline_summaries(&pooled);
        }
        Ok(pool)
    }

    /// Finish the run compared against a pooled baseline.
    pub(crate) fn finish_with_baseline_pool(mut self, pool: &BaselinePool) -> StressRun {
        if pool.considered_extras {
            self.metadata.insert(
                "baseline_runs_pooled".to_string(),
                pool.pooled_runs.len().to_string(),
            );
            if !pool.skipped.is_empty() {
                self.metadata
                    .insert("baseline_runs_skipped".to_string(), pool.skipped.join("; "));
            }
        }
        let comparisons = self.baseline_comparisons(pool);
        self.finish_inner(comparisons)
    }

    fn baseline_comparisons(&self, pool: &BaselinePool) -> Vec<ComparisonResult> {
        compare_summaries_with_specs(
            &self.summaries,
            &self.benchmark_specs,
            &self.environment,
            &pool.summaries,
            &pool.specs,
            &pool.environment,
            self.config.threshold,
        )
    }

    /// Row ids currently classified as regressions against `pool`.
    pub(crate) fn regressed_rows(&self, pool: &BaselinePool) -> Vec<String> {
        self.baseline_comparisons(pool)
            .into_iter()
            .filter(|comparison| comparison.classification == ComparisonClass::Regression)
            .map(|comparison| comparison.benchmark_id)
            .collect()
    }

    /// Re-run benchmarks with regression rows up to `max_attempts` times.
    ///
    /// Each attempt calls `rerun` once per benchmark (function-level) id that
    /// still has a regression row, then re-classifies against the samples
    /// pooled across the original run and every attempt. Attempts stop once
    /// nothing regresses or an attempt fails; every attempt is recorded.
    pub(crate) fn confirm_regressions<R>(
        &mut self,
        pool: &BaselinePool,
        max_attempts: usize,
        mut rerun: R,
    ) where
        R: FnMut(&mut Self, &str) -> Result<(), String>,
    {
        if max_attempts > 0
            && self.summaries.iter().any(|summary| {
                summary
                    .budget_results
                    .iter()
                    .any(|budget_result| !budget_result.passed)
            })
        {
            // Pooling re-evaluates every budget of a re-run benchmark, so it
            // could clear an absolute budget failure; those are final.
            self.metadata.insert(
                "confirmation_skipped".to_string(),
                "the run already failed a benchmark budget, which confirmation cannot clear"
                    .to_string(),
            );
            return;
        }
        for attempt in 1..=max_attempts {
            let before = self.regressed_rows(pool);
            if before.is_empty() {
                break;
            }
            let base_ids = before
                .iter()
                .filter_map(|row| self.measurement_bases.get(row).cloned())
                .collect::<BTreeSet<_>>();
            let start = self.samples.len();
            let mut errors = Vec::new();
            for base_id in &base_ids {
                if let Err(error) = rerun(self, base_id) {
                    errors.push(format!("{base_id}: {error}"));
                }
            }
            let mut record = ConfirmationRun::new(attempt);
            record.benchmark_ids = base_ids.into_iter().collect();
            record.regressions_before = before;
            record.regressions_after = self.regressed_rows(pool);
            record.samples_added = self.samples.len() - start;
            let failed = !errors.is_empty();
            if failed {
                record.error = Some(errors.join("; "));
            }
            self.confirmation_runs.push(record);
            if failed {
                break;
            }
        }
    }

    /// Re-run an already-run benchmark with the same config and pool its new
    /// raw samples into the existing rows, recomputing their summaries.
    ///
    /// Returns the number of raw samples added. On failure nothing is added.
    pub(crate) fn confirm_spec<F, O>(&mut self, base_id: &str, f: F) -> Result<usize, String>
    where
        F: Fn(&mut StressContext) -> O,
        O: IntoStressResult,
    {
        let spec = self
            .base_specs
            .get(base_id)
            .cloned()
            .ok_or_else(|| format!("benchmark {base_id:?} was not run"))?;
        let last_error = std::cell::RefCell::new(None::<String>);
        let body = |ctx: &mut StressContext| {
            let result = f(ctx).into_stress_result();
            if let Err(error) = &result {
                *last_error.borrow_mut() = Some(error.message().to_string());
            }
            result
        };
        let start = self.samples.len();
        let mut topology = MeasurementTopology::default();
        let failed = self.record_phase_samples(
            &spec,
            SamplePhase::Warmup,
            self.config.warmup_samples,
            &body,
            &mut topology,
            start,
        ) || self.record_phase_samples(
            &spec,
            SamplePhase::Measured,
            self.config.samples,
            &body,
            &mut topology,
            start,
        ) || self.record_phase_samples(
            &spec,
            SamplePhase::Cooldown,
            self.config.cooldown_samples,
            &body,
            &mut topology,
            start,
        );
        if failed {
            self.samples.truncate(start);
            let error = last_error
                .into_inner()
                .unwrap_or_else(|| "the benchmark returned an error".to_string());
            return Err(format!("confirmation run failed: {error}"));
        }
        let expected = self
            .benchmark_specs
            .iter()
            .filter(|candidate| {
                self.measurement_bases
                    .get(&candidate.id)
                    .map(String::as_str)
                    == Some(base_id)
            })
            .collect::<Vec<_>>();
        let topology_matches = expected.len() == topology.spec_order.len()
            && expected
                .iter()
                .all(|candidate| topology.specs.get(&candidate.id) == Some(*candidate));
        if !topology_matches {
            self.samples.truncate(start);
            return Err(
                "confirmation run recorded different measurement rows than the original run"
                    .to_string(),
            );
        }
        for spec in expected {
            if let Some(summary) = self
                .summaries
                .iter_mut()
                .find(|summary| summary.benchmark_id == spec.id)
            {
                let source = summary.source.take();
                *summary = summarize_benchmark(spec, &self.samples);
                summary.source = source;
            }
        }
        Ok(self.samples.len() - start)
    }

    fn finish_inner(mut self, comparisons: Vec<crate::artifact::ComparisonResult>) -> StressRun {
        attach_regression_diagnostics(&mut self.summaries, &comparisons);
        attach_measurement_mode_mismatch_diagnostics(&mut self.summaries);
        crate::scaling::attach_scaling_diagnostics(
            &mut self.summaries,
            self.environment.core_count,
        );
        attach_timer_resolution_evidence(&mut self.summaries, self.environment.timer_resolution_ns);
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
            confirmation_runs: self.confirmation_runs,
            started_at: run_timestamp_stem(),
            total_elapsed_ns: self.suite_start.elapsed().as_nanos(),
            metadata: self.metadata,
        };
        run.metadata.insert(
            SUMMARY_SEMANTICS_METADATA_KEY.to_string(),
            SUMMARY_SEMANTICS_CURRENT.to_string(),
        );

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
            // Match the benchmark name; a filter containing '/' may also match
            // the suite-qualified id ("suite/bench"). The id is never matched
            // for a plain filter, so the suite name alone does not select
            // every benchmark.
            spec.name.contains(filter.as_str())
                || (filter.contains('/') && spec.id.contains(filter.as_str()))
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
                // The user's error is the result. Register rows leniently so a
                // topology or override mismatch cannot panic and hide it.
                topology.register_failed_invocation(base_spec, &records);
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
        // A zero duration is recorded as-is: it marks the sample's timing as
        // invalid (and its throughput as 0) instead of being floored to 1ns,
        // which would report an absurd throughput for real work.
        let duration = record.duration;
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
        ("max_peak_rss_mb", spec.budgets.max_peak_rss_mb, None),
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
            let mut error_records = error_ctx.take_measurements();
            for error_record in &mut error_records {
                error_record.name = unique_record_name(&records, &error_record.name);
            }
            records.extend(error_records);
        }
        (records, wall_clock, true)
    } else {
        (ctx.take_measurements(), wall_clock, false)
    }
}

fn unique_record_name(records: &[MeasurementRecord], base: &str) -> String {
    let taken = |candidate: &str| records.iter().any(|record| record.name == candidate);
    if !taken(base) {
        return base.to_string();
    }
    // At most `records.len()` names are taken, so this range always has a free slot.
    (2..=records.len() + 2)
        .map(|suffix| format!("{base} ({suffix})"))
        .find(|candidate| !taken(candidate))
        .expect("an unused suffix exists")
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
    sources: BTreeMap<String, SourceLocation>,
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

    /// Register the rows of a failed invocation without enforcing the
    /// cross-invocation contract, so the user's error is always reported.
    fn register_failed_invocation(
        &mut self,
        base_spec: &BenchmarkSpec,
        records: &[MeasurementRecord],
    ) {
        for record in records {
            let benchmark_id = measurement_id(&base_spec.id, &record.name);
            if self.specs.contains_key(&benchmark_id) {
                continue;
            }
            let candidate = measurement_spec(base_spec, record, &benchmark_id);
            self.spec_order.push(benchmark_id.clone());
            self.record_source(&benchmark_id, record);
            self.specs.insert(benchmark_id.clone(), candidate);
            self.overrides.insert(
                benchmark_id,
                MeasurementOverrideContract::from_record(record),
            );
        }
        self.names = Some(records.iter().map(|record| record.name.clone()).collect());
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
        self.record_source(&benchmark_id, record);
        self.specs.insert(benchmark_id.clone(), candidate);
        self.overrides.insert(benchmark_id, overrides);
    }

    fn record_source(&mut self, benchmark_id: &str, record: &MeasurementRecord) {
        if let Some(source) = &record.source {
            self.sources
                .insert(benchmark_id.to_string(), source.clone());
        }
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
    let core_count = std::thread::available_parallelism()
        .ok()
        .map(std::num::NonZeroUsize::get);
    EnvironmentInfo {
        cpu_model: detect_cpu_model(),
        core_count,
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
        timer_resolution_ns: measure_timer_resolution_ns(),
        observations: crate::environment::capture_observations(core_count),
    }
}

/// Smallest nonzero `Instant` increment observed over a few trials.
fn measure_timer_resolution_ns() -> Option<u64> {
    const TRIALS: usize = 16;
    const MAX_SPINS: usize = 1_000_000;
    (0..TRIALS)
        .filter_map(|_| {
            let start = Instant::now();
            (0..MAX_SPINS).find_map(|_| {
                let elapsed = start.elapsed();
                (!elapsed.is_zero()).then_some(elapsed)
            })
        })
        .min()
        .and_then(|elapsed| u64::try_from(elapsed.as_nanos()).ok())
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
    #[cfg(target_os = "windows")]
    {
        command_stdout(
            "reg",
            &[
                "query",
                r"HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0",
                "/v",
                "ProcessorNameString",
            ],
        )
        .and_then(|output| parse_reg_query_string_value(&output, "ProcessorNameString"))
        .or_else(|| {
            std::env::var("PROCESSOR_IDENTIFIER")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_string())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        "unknown".to_string()
    }
}

/// Extracts a `REG_SZ` value from `reg query <key> /v <name>` output, whose
/// value line looks like `    ProcessorNameString    REG_SZ    <value>`.
#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn parse_reg_query_string_value(output: &str, name: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let rest = line.trim_start().strip_prefix(name)?;
        let rest = rest.trim_start().strip_prefix("REG_SZ")?;
        let value = rest.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
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
///
/// New gate outcomes may be added in minor releases, so matches outside this
/// crate need a wildcard arm:
///
/// ```compile_fail
/// use cntryl_stress::runner::RunGate;
///
/// fn label(gate: RunGate) -> &'static str {
///     match gate {
///         RunGate::Passed => "passed",
///         RunGate::CorrectnessFailed => "correctness",
///         RunGate::QualityFailed => "quality",
///         RunGate::RegressionFailed => "regression",
///         RunGate::DiagnosticsFailed => "diagnostics",
///         RunGate::BudgetFailed => "budget",
///         RunGate::ArtifactFailed => "artifact",
///     }
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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
    /// `require_quiet_env` is set and an environment observation is adverse.
    EnvironmentFailed,
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
    if run.environment.profile_config.require_quiet_env
        && run.environment.adverse_observations().next().is_some()
    {
        return RunGate::EnvironmentFailed;
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
    if !run.diagnostic_gate_failures().is_empty() {
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
        SourceLocation, TrustClass,
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
    fn captured_environment_measures_timer_resolution() {
        let environment = capture_environment(&StressRunnerConfig::new());
        assert!(environment
            .timer_resolution_ns
            .is_some_and(|resolution| resolution > 0));
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
    fn run_records_the_caller_location_as_the_function_level_source() {
        let config = StressRunnerConfig::new()
            .samples(1)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        let expected_line = line!() + 1;
        runner.run("bench", |ctx| {
            crate::__private::block_on(
                ctx.measure_async("lookup", || async { std::hint::black_box(1_u64) }),
            );
        });
        let run = runner.finish();

        let source = run.summaries[0].source.as_ref().expect("source location");
        assert_eq!(source.file, file!());
        assert_eq!(source.line, expected_line);
    }

    #[test]
    fn row_level_source_wins_over_function_level_source() {
        let config = StressRunnerConfig::new()
            .samples(2)
            .warmup_samples(1)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        let spec = BenchmarkSpec::new(
            "suite/bench",
            "bench",
            2,
            runner
                .config
                .mode_for_kind(BenchmarkModeKind::FixedOperations),
        );
        let fn_source = SourceLocation::new("benches/registered.rs", 7);
        let row_line = std::cell::Cell::new(0);
        let measure_line = std::cell::Cell::new(0);
        runner.run_spec_at(&spec, Some(&fn_source), |ctx| {
            row_line.set(line!() + 1);
            ctx.benchmark("builder_row")
                .measure(|| std::hint::black_box(1_u64));
            measure_line.set(line!() + 1);
            ctx.measure("plain_row", || std::hint::black_box(2_u64));
        });
        let run = runner.finish();

        let by_name = |name: &str| {
            run.summaries
                .iter()
                .find(|summary| summary.name.ends_with(name))
                .and_then(|summary| summary.source.clone())
                .expect("row source")
        };
        assert_eq!(
            by_name("builder_row"),
            SourceLocation::new(file!(), row_line.get())
        );
        assert_eq!(
            by_name("plain_row"),
            SourceLocation::new(file!(), measure_line.get())
        );
    }

    #[test]
    fn function_level_source_is_used_when_the_row_has_none() {
        let config = StressRunnerConfig::new()
            .samples(1)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());
        let spec = BenchmarkSpec::new(
            "suite/bench",
            "bench",
            2,
            runner
                .config
                .mode_for_kind(BenchmarkModeKind::FixedOperations),
        );
        let fn_source = SourceLocation::new("benches/registered.rs", 7);
        runner.run_spec_at(&spec, Some(&fn_source), |ctx| {
            crate::__private::block_on(
                ctx.measure_async("async_row", || async { std::hint::black_box(1_u64) }),
            );
        });
        let run = runner.finish();

        assert_eq!(run.summaries[0].source, Some(fn_source));
    }

    #[test]
    fn baseline_with_a_different_source_location_still_compares() {
        let mut baseline = external_throughput_runner(1_000).finish();
        baseline.summaries[0].source =
            Some(SourceLocation::new("/some/other/checkout/benches/x.rs", 99));
        let baseline_path = unique_temp_path("stress-baseline-source-location.json");
        std::fs::write(
            &baseline_path,
            serde_json::to_string(&baseline).expect("serialize baseline"),
        )
        .expect("write baseline");

        let run = external_throughput_runner(1_000)
            .finish_with_baseline(&baseline_path)
            .expect("source locations must not affect baseline validation");

        assert_eq!(run.comparisons.len(), 1);
        let _ = std::fs::remove_file(&baseline_path);
    }

    #[test]
    fn baseline_with_peak_rss_and_its_diagnostic_still_compares() {
        let mut baseline = external_throughput_runner(1_000).finish();
        baseline.summaries[0].peak_rss_bytes = Some(123_456_789);
        baseline.summaries[0]
            .diagnostics
            .push(crate::artifact::BenchmarkDiagnostic::new(
                "peak_rss_exceeded",
                DiagnosticSeverity::Warning,
                "Process peak RSS exceeded max_peak_rss_mb.",
            ));
        let baseline_path = unique_temp_path("stress-baseline-peak-rss.json");
        std::fs::write(
            &baseline_path,
            serde_json::to_string(&baseline).expect("serialize baseline"),
        )
        .expect("write baseline");

        let run = external_throughput_runner(1_000)
            .finish_with_baseline(&baseline_path)
            .expect("peak RSS must not affect baseline validation");

        assert_eq!(run.comparisons.len(), 1);
        let _ = std::fs::remove_file(&baseline_path);
    }

    fn peak_rss_spec(budget_mb: f64) -> BenchmarkSpec {
        BenchmarkSpec {
            id: "suite/rss".to_string(),
            name: "rss".to_string(),
            tier: 2,
            mode: BenchmarkMode::FixedOperations {
                operations_per_sample: 1,
            },
            intent: crate::artifact::MeasurementIntent::General,
            budgets: BenchmarkBudgets::new().with_max_peak_rss_mb(budget_mb),
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
        }
    }

    fn run_peak_rss_spec(budget_mb: f64) -> StressRun {
        let config = StressRunnerConfig::new()
            .samples(2)
            .warmup_samples(0)
            .cooldown_samples(0)
            .progress(false);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());
        runner.run_spec(&peak_rss_spec(budget_mb), |ctx| {
            ctx.measure("work", || std::hint::black_box(1_u64));
        });
        runner.finish()
    }

    #[cfg(unix)]
    #[test]
    fn exceeded_peak_rss_budget_is_a_warning_diagnostic_not_a_budget_failure() {
        let run = run_peak_rss_spec(0.001);
        let summary = &run.summaries[0];

        assert!(summary.peak_rss_bytes.is_some_and(|bytes| bytes > 0));
        let diagnostic = summary
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "peak_rss_exceeded")
            .expect("peak_rss_exceeded diagnostic");
        assert_eq!(diagnostic.severity, DiagnosticSeverity::Warning);
        assert!(diagnostic.evidence.contains_key("peak_rss_bytes"));
        assert!(summary
            .budget_results
            .iter()
            .all(|result| result.metric != "max_peak_rss_mb"));
        assert!(run.budgets_passed());
        assert!(run.diagnostic_gate_failures().is_empty());
    }

    #[test]
    fn generous_peak_rss_budget_emits_no_diagnostic() {
        let run = run_peak_rss_spec(1_000_000.0);

        assert!(run.summaries[0]
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "peak_rss_exceeded"));
    }

    #[test]
    #[should_panic(expected = "max_peak_rss_mb must be a finite non-negative number")]
    fn negative_peak_rss_budget_is_rejected() {
        let _ = run_peak_rss_spec(-1.0);
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
            // Sleep so one operation always spans at least one timer tick; a
            // zero reading is (correctly) invalid timing.
            ctx.measure("work", || std::thread::sleep(Duration::from_micros(1)));
        });
        runner.finish()
    }

    #[test]
    fn adverse_observations_fail_the_gate_only_when_quiet_env_is_required() {
        let mut run = warning_diagnostic_run(None);
        run.environment.observations = vec![crate::artifact::EnvironmentObservation::new(
            "load_average",
            "0.1",
            false,
            "",
        )];
        run.environment.profile_config.require_quiet_env = true;
        assert_eq!(evaluate_run_gate(&run), RunGate::Passed);

        run.environment
            .observations
            .push(crate::artifact::EnvironmentObservation::new(
                "power_source",
                "battery",
                true,
                "on battery",
            ));
        assert_eq!(evaluate_run_gate(&run), RunGate::EnvironmentFailed);

        run.environment.profile_config.require_quiet_env = false;
        assert_eq!(evaluate_run_gate(&run), RunGate::Passed);
    }

    #[test]
    fn captured_environment_records_observations_from_the_host_probe() {
        let environment = capture_environment(&StressRunnerConfig::new());
        assert!(environment.observations.iter().all(|observation| [
            "cpu_governor",
            "cpu_boost",
            "load_average",
            "cpu_quota",
            "power_source"
        ]
        .contains(&observation.key.as_str())));
        #[cfg(target_os = "linux")]
        assert!(environment
            .observations
            .iter()
            .any(|observation| observation.key == "load_average"));
    }

    #[test]
    fn denied_codes_fail_the_gate_without_a_severity_threshold() {
        let mut run = warning_diagnostic_run(None);
        assert!(run
            .diagnostics_summary
            .iter()
            .any(|diagnostic| diagnostic.code == "too_few_samples"));
        run.environment.profile_config.deny_codes = vec!["regression".to_string()];
        assert_eq!(evaluate_run_gate(&run), RunGate::Passed);
        assert!(run.diagnostic_gate_failures().is_empty());

        run.environment.profile_config.deny_codes = vec!["too_few_samples".to_string()];
        assert_eq!(evaluate_run_gate(&run), RunGate::DiagnosticsFailed);
        assert!(run
            .diagnostic_gate_failures()
            .iter()
            .all(|diagnostic| diagnostic.code == "too_few_samples"));
    }

    #[test]
    fn console_attention_and_verdict_follow_the_code_policy() {
        let mut run = warning_diagnostic_run(None);
        run.environment.profile_config.deny_codes = vec!["too_few_samples".to_string()];
        let report = crate::reporting::format_console_run(&run);
        let attention = crate::reporting::attention_items(&run);
        assert!(
            attention
                .iter()
                .any(|item| item.1.contains("=too_few_samples:")),
            "{attention:?}"
        );
        assert!(report.contains("denied codes: too_few_samples"), "{report}");
        assert!(!report.contains(">= unknown"), "{report}");

        let mut run = warning_diagnostic_run(Some(DiagnosticSeverity::Info));
        let present = run
            .diagnostics_summary
            .iter()
            .map(|diagnostic| diagnostic.code.clone())
            .collect::<Vec<_>>();
        run.environment.profile_config.allow_codes = present;
        let report = crate::reporting::format_console_run(&run);
        let attention = crate::reporting::attention_items(&run);
        assert!(
            !attention.iter().any(|item| item.1.contains(" diagnostic ")),
            "{attention:?}"
        );
        assert!(!report.contains("failed diagnostics"), "{report}");
    }

    #[test]
    fn allowed_codes_are_exempt_from_severity_gating_but_deny_wins() {
        let mut run = warning_diagnostic_run(Some(DiagnosticSeverity::Info));
        assert_eq!(evaluate_run_gate(&run), RunGate::DiagnosticsFailed);
        let present = run
            .diagnostics_summary
            .iter()
            .map(|diagnostic| diagnostic.code.clone())
            .collect::<Vec<_>>();
        run.environment
            .profile_config
            .allow_codes
            .clone_from(&present);
        assert_eq!(evaluate_run_gate(&run), RunGate::Passed);

        run.environment.profile_config.deny_codes = vec![present[0].clone()];
        assert_eq!(evaluate_run_gate(&run), RunGate::DiagnosticsFailed);
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

    #[test]
    fn finished_runs_carry_scaling_diagnostics_for_sweeps() {
        let config = StressRunnerConfig::new()
            .samples(5)
            .warmup_samples(0)
            .cooldown_samples(0)
            .operations_per_sample(1);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());
        for size in [1_u64, 2, 4, 8] {
            runner.run(&format!("scan/size={size}"), move |ctx| {
                ctx.parameter("size", size);
                ctx.record_external("work", Duration::from_millis(size), 1);
            });
        }
        let run = runner.finish();

        assert!(run.summaries.iter().all(|summary| summary
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "scaling_anomaly")));
        assert!(run
            .diagnostics_summary
            .iter()
            .any(|diagnostic| diagnostic.code == "scaling_anomaly"));
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
        // Pin the environment so baseline compatibility does not depend on
        // host detection (CPU model detection is unavailable on some hosts).
        runner.environment.cpu_model = "fixture test cpu".to_string();
        runner.run("bench", |ctx| {
            ctx.record_external("work", Duration::from_millis(10), completed_operations);
        });
        runner
    }

    #[test]
    fn zero_net_duration_produces_invalid_timing_not_absurd_throughput() {
        let runner = StressRunner::with_config("suite", StressRunnerConfig::new());
        let record = crate::context::MeasurementRecord {
            name: "zero".to_string(),
            intent: MeasurementIntent::General,
            mode: BenchmarkMode::Micro {
                target_sample_duration: Duration::from_millis(1),
            },
            duration: Duration::ZERO,
            latency_ns: Vec::new(),
            observations: Vec::new(),
            counters: CorrectnessCounters {
                attempted: 1_000_000,
                completed: 1_000_000,
                ..CorrectnessCounters::default()
            },
            operations_hint: None,
            micro: None,
            allocation: None,
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
            overrides: crate::context::MeasurementOverrides::default(),
            source: None,
        };

        let sample = runner.sample_from_record(
            "suite/zero",
            1,
            SamplePhase::Measured,
            Duration::from_millis(1),
            record,
        );

        assert_eq!(sample.elapsed_ns, 0);
        assert!(
            sample.throughput.abs() < f64::EPSILON,
            "{}",
            sample.throughput
        );
        assert!(!sample.has_valid_timing());
    }

    #[test]
    fn baseline_written_by_main_v0_4_0_loads_and_compares() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/baseline-main-v0.4.0.json");
        let baseline: StressRun =
            serde_json::from_str(&std::fs::read_to_string(&fixture).expect("read fixture"))
                .expect("parse fixture");
        baseline
            .canonical_baseline_summaries()
            .expect("a baseline written by main must still load");

        let config = StressRunnerConfig::new()
            .samples(7)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("fixture", config);
        runner.reporters(Vec::new());
        runner.run("throughput", |ctx| {
            ctx.record_external("work", Duration::from_micros(1_000), 1_000);
        });
        runner.run("latency", |ctx| {
            for _ in 0..23 {
                ctx.record_latency(Duration::from_nanos(1_200));
            }
            ctx.metadata("primary_metric", "latency");
            ctx.record_external("requests", Duration::from_micros(500), 23);
        });
        let run = runner
            .finish_with_baseline(&fixture)
            .expect("compare against a main baseline");

        assert_eq!(run.comparisons.len(), 2, "{:?}", run.comparisons);
    }

    #[test]
    fn parses_windows_processor_name_from_reg_query_output() {
        let output = "\r\nHKEY_LOCAL_MACHINE\\HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0\r\n    ProcessorNameString    REG_SZ    AMD EPYC 7763 64-Core Processor\r\n\r\n";
        assert_eq!(
            parse_reg_query_string_value(output, "ProcessorNameString").as_deref(),
            Some("AMD EPYC 7763 64-Core Processor")
        );
        assert_eq!(
            parse_reg_query_string_value("", "ProcessorNameString"),
            None
        );
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

    fn write_baseline(name: &str, run: &StressRun) -> std::path::PathBuf {
        let path = unique_temp_path(name);
        std::fs::write(
            &path,
            serde_json::to_string(run).expect("serialize baseline"),
        )
        .expect("write baseline");
        path
    }

    /// A runner whose first `slow_invocations` invocations take `slow`, and
    /// every later invocation takes 10ms, each completing 1000 operations.
    fn timed_runner(slow_invocations: usize, slow: Duration) -> (StressRunner, Arc<Mutex<usize>>) {
        let config = StressRunnerConfig::new()
            .samples(10)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());
        runner.environment.cpu_model = "fixture test cpu".to_string();
        let invocations = Arc::new(Mutex::new(0usize));
        let counter = Arc::clone(&invocations);
        runner.run("bench", move |ctx| {
            let mut count = counter.lock().expect("counter");
            let duration = if *count < slow_invocations {
                slow
            } else {
                Duration::from_millis(10)
            };
            *count += 1;
            ctx.record_external("work", duration, 1_000);
        });
        (runner, invocations)
    }

    fn timed_body(
        invocations: Arc<Mutex<usize>>,
        slow_invocations: usize,
        slow: Duration,
    ) -> impl Fn(&mut StressContext) {
        move |ctx| {
            let mut count = invocations.lock().expect("counter");
            let duration = if *count < slow_invocations {
                slow
            } else {
                Duration::from_millis(10)
            };
            *count += 1;
            ctx.record_external("work", duration, 1_000);
        }
    }

    #[test]
    fn pooled_baseline_of_one_run_matches_single_run_comparison() {
        let baseline = external_throughput_runner(1_000).finish();
        let path = write_baseline("stress-pool-one.json", &baseline);

        let single = external_throughput_runner(900)
            .finish_with_baseline(&path)
            .expect("single");
        let runner = external_throughput_runner(900);
        // The anchor duplicated among the extras is pooled once.
        let pool = runner
            .load_baseline_pool(&path, std::slice::from_ref(&path), 5)
            .expect("pool");
        assert_eq!(pool.pooled_runs.len(), 1);
        assert_eq!(
            pool.summaries,
            baseline.canonical_baseline_summaries().expect("canonical")
        );
        let pooled = runner.finish_with_baseline_pool(&pool);
        assert_eq!(pooled.comparisons, single.comparisons);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn compatible_baseline_runs_pool_raw_samples_and_incompatible_ones_are_skipped() {
        let anchor = external_throughput_runner(1_000).finish();
        let older = external_throughput_runner(1_000).finish();
        let mut foreign = external_throughput_runner(1_000).finish();
        foreign.environment.cpu_model = "other cpu".to_string();
        for sample in &mut foreign.samples {
            sample.environment.cpu_model = "other cpu".to_string();
        }
        let anchor_path = write_baseline("stress-pool-anchor.json", &anchor);
        let older_path = write_baseline("stress-pool-older.json", &older);
        let foreign_path = write_baseline("stress-pool-foreign.json", &foreign);
        let missing_path = unique_temp_path("stress-pool-missing.json");

        let runner = external_throughput_runner(1_000);
        let pool = runner
            .load_baseline_pool(
                &anchor_path,
                &[foreign_path.clone(), missing_path, older_path.clone()],
                5,
            )
            .expect("pool");
        assert_eq!(
            pool.pooled_runs,
            vec![anchor.started_at.clone(), older.started_at.clone()]
        );
        assert_eq!(pool.skipped.len(), 2, "{:?}", pool.skipped);
        assert!(pool.skipped.iter().any(|note| note.contains("CPU model")));
        assert_eq!(pool.summaries[0].measured_samples, 20);

        let run = runner.finish_with_baseline_pool(&pool);
        assert_eq!(
            run.metadata.get("baseline_runs_pooled"),
            Some(&"2".to_string())
        );
        assert!(run
            .metadata
            .get("baseline_runs_skipped")
            .is_some_and(|note| note.contains("CPU model")));
        assert_eq!(
            run.comparisons[0].classification,
            ComparisonClass::Inconclusive
        );

        // max_runs caps the pool including the anchor.
        let capped = external_throughput_runner(1_000)
            .load_baseline_pool(&anchor_path, std::slice::from_ref(&older_path), 1)
            .expect("capped pool");
        assert_eq!(capped.pooled_runs.len(), 1);

        for path in [anchor_path, older_path, foreign_path] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn flaky_regression_clears_after_confirmation() {
        let baseline = timed_runner(0, Duration::ZERO).0.finish();
        let path = write_baseline("stress-confirm-flaky.json", &baseline);
        let slow = Duration::from_micros(10_600);

        let (unconfirmed, _) = timed_runner(10, slow);
        let pool = unconfirmed.load_baseline_pool(&path, &[], 1).expect("pool");
        let mut run = unconfirmed.finish_with_baseline_pool(&pool);
        run.environment.profile_config.fail_on_regression = true;
        assert_eq!(evaluate_run_gate(&run), RunGate::RegressionFailed);

        let (mut runner, invocations) = timed_runner(10, slow);
        let body = timed_body(invocations, 10, slow);
        runner.confirm_regressions(&pool, 3, |runner, base_id| {
            runner.confirm_spec(base_id, &body).map(|_| ())
        });
        let mut run = runner.finish_with_baseline_pool(&pool);
        run.environment.profile_config.fail_on_regression = true;

        assert_eq!(
            run.confirmation_runs.len(),
            1,
            "{:?}",
            run.confirmation_runs
        );
        let attempt = &run.confirmation_runs[0];
        assert_eq!(attempt.attempt, 1);
        assert_eq!(attempt.benchmark_ids, vec!["suite/bench".to_string()]);
        assert_eq!(
            attempt.regressions_before,
            vec!["suite/bench/work".to_string()]
        );
        assert!(attempt.regressions_after.is_empty());
        assert_eq!(attempt.samples_added, 10);
        assert_eq!(run.samples.len(), 20);
        assert_eq!(run.summaries[0].measured_samples, 20);
        assert!(
            run.summaries[0].source.is_some(),
            "confirmation keeps the source"
        );
        run.canonical_baseline_summaries()
            .expect("a confirmed run is internally consistent");
        assert_eq!(
            evaluate_run_gate(&run),
            RunGate::Passed,
            "{:?}",
            run.comparisons
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn persistent_regression_still_fails_after_every_confirmation_attempt() {
        let baseline = timed_runner(0, Duration::ZERO).0.finish();
        let path = write_baseline("stress-confirm-true.json", &baseline);
        let slow = Duration::from_micros(10_600);

        let (mut runner, invocations) = timed_runner(usize::MAX, slow);
        let pool = runner.load_baseline_pool(&path, &[], 1).expect("pool");
        let body = timed_body(invocations, usize::MAX, slow);
        runner.confirm_regressions(&pool, 2, |runner, base_id| {
            runner.confirm_spec(base_id, &body).map(|_| ())
        });
        let mut run = runner.finish_with_baseline_pool(&pool);
        run.environment.profile_config.fail_on_regression = true;

        assert_eq!(run.confirmation_runs.len(), 2);
        assert!(run
            .confirmation_runs
            .iter()
            .all(|attempt| attempt.regressions_after == vec!["suite/bench/work".to_string()]));
        assert_eq!(run.samples.len(), 30);
        assert_eq!(evaluate_run_gate(&run), RunGate::RegressionFailed);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn failed_confirmation_attempt_never_clears_a_regression() {
        let baseline = timed_runner(0, Duration::ZERO).0.finish();
        let path = write_baseline("stress-confirm-failed.json", &baseline);
        let slow = Duration::from_micros(10_600);

        let (mut runner, _) = timed_runner(10, slow);
        let pool = runner.load_baseline_pool(&path, &[], 1).expect("pool");
        runner.confirm_regressions(&pool, 3, |runner, _| {
            runner
                .confirm_spec(
                    "suite/bench",
                    |_ctx: &mut StressContext| -> crate::error::StressResult {
                        Err(crate::error::StressError::new("boom"))
                    },
                )
                .map(|_| ())
        });
        let mut run = runner.finish_with_baseline_pool(&pool);
        run.environment.profile_config.fail_on_regression = true;

        assert_eq!(run.confirmation_runs.len(), 1);
        assert!(run.confirmation_runs[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("boom")));
        assert_eq!(run.confirmation_runs[0].samples_added, 0);
        assert_eq!(run.samples.len(), 10);
        assert_eq!(evaluate_run_gate(&run), RunGate::RegressionFailed);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn confirmation_never_re_pools_a_run_that_already_failed_a_budget() {
        let baseline = timed_runner(0, Duration::ZERO).0.finish();
        let path = write_baseline("stress-confirm-budget.json", &baseline);
        let slow = Duration::from_micros(10_600);

        let (mut runner, _) = timed_runner(10, slow);
        runner.summaries[0]
            .budget_results
            .push(crate::artifact::BudgetResult {
                metric: "ns_per_op".to_string(),
                limit: 1.0,
                actual: Some(2.0),
                passed: false,
                reason: None,
            });
        let pool = runner.load_baseline_pool(&path, &[], 1).expect("pool");
        let mut reruns = 0;
        runner.confirm_regressions(&pool, 3, |_, _| {
            reruns += 1;
            Ok(())
        });
        let run = runner.finish_with_baseline_pool(&pool);

        assert_eq!(
            reruns, 0,
            "pooling must not clear an absolute budget failure"
        );
        assert!(run.confirmation_runs.is_empty());
        assert!(run
            .metadata
            .get("confirmation_skipped")
            .is_some_and(|reason| reason.contains("budget")));
        assert_eq!(evaluate_run_gate(&run), RunGate::BudgetFailed);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn confirmation_is_a_no_op_without_regressions() {
        let baseline = timed_runner(0, Duration::ZERO).0.finish();
        let path = write_baseline("stress-confirm-noop.json", &baseline);
        let (mut runner, _) = timed_runner(0, Duration::ZERO);
        let pool = runner.load_baseline_pool(&path, &[], 1).expect("pool");
        let mut reruns = 0;
        runner.confirm_regressions(&pool, 3, |_, _| {
            reruns += 1;
            Ok(())
        });
        let run = runner.finish_with_baseline_pool(&pool);
        assert_eq!(reruns, 0);
        assert!(run.confirmation_runs.is_empty());
        assert!(serde_json::to_value(&run)
            .expect("json")
            .get("confirmation_runs")
            .is_none());

        let _ = std::fs::remove_file(&path);
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

    #[test]
    fn failed_invocation_with_row_warmup_override_records_user_error() {
        let config = StressRunnerConfig::new()
            .samples(2)
            .warmup_samples(1)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("fallible", |ctx| {
            ctx.benchmark("work")
                .warmup(1)
                .measure_result(|| Err::<(), _>("warmup transport failed"))?;
            Ok::<(), &str>(())
        });
        let run = runner.finish();

        assert_eq!(run.summaries.len(), 1);
        assert!(!run.summaries[0].correctness.passed);
        assert_eq!(
            run.summaries[0].metadata.get("benchmark_error"),
            Some(&"warmup transport failed".to_string())
        );
        assert_eq!(evaluate_run_gate(&run), RunGate::CorrectnessFailed);
    }

    #[test]
    fn failed_invocation_after_overridden_rows_records_synthetic_error_row() {
        let config = StressRunnerConfig::new()
            .samples(2)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("fallible", |ctx| {
            ctx.benchmark("work").samples(3).cooldown(1).measure(|| {});
            Err::<(), _>("teardown failed")
        });
        let run = runner.finish();

        let error_row = run
            .summaries
            .iter()
            .find(|summary| summary.metadata.contains_key("benchmark_error"))
            .expect("error row");
        assert_eq!(
            error_row.metadata.get("benchmark_error"),
            Some(&"teardown failed".to_string())
        );
        assert!(!error_row.correctness.passed);
        assert_eq!(evaluate_run_gate(&run), RunGate::CorrectnessFailed);
    }

    #[test]
    fn failed_invocation_with_user_row_named_benchmark_error_does_not_collide() {
        let config = StressRunnerConfig::new()
            .samples(1)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(Vec::new());

        runner.run("fallible", |ctx| {
            ctx.measure("benchmark error", || {});
            Err::<(), _>("late failure")
        });
        let run = runner.finish();

        assert_eq!(run.summaries.len(), 2);
        let ids = run
            .summaries
            .iter()
            .map(|summary| summary.benchmark_id.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), 2);
        assert!(run.summaries.iter().any(|summary| summary
            .metadata
            .get("benchmark_error")
            .is_some_and(|message| message == "late failure")));
        assert_eq!(evaluate_run_gate(&run), RunGate::CorrectnessFailed);
    }

    #[test]
    fn filter_matches_benchmark_name_not_suite_name() {
        let config = StressRunnerConfig::new()
            .samples(1)
            .warmup_samples(0)
            .filter("storage");
        let mut runner = StressRunner::with_config("storage", config);
        runner.reporters(Vec::new());

        runner.run("parse", |ctx| {
            ctx.measure("work", || {});
        });
        runner.run("storage_write", |ctx| {
            ctx.measure("work", || {});
        });
        let run = runner.finish();

        assert_eq!(run.summaries.len(), 1);
        assert_eq!(run.summaries[0].benchmark_id, "storage/storage_write/work");
    }

    #[test]
    fn filter_containing_slash_matches_the_suite_qualified_id() {
        let config = StressRunnerConfig::new()
            .samples(1)
            .warmup_samples(0)
            .filter("storage/parse");
        let mut runner = StressRunner::with_config("storage", config);
        runner.reporters(Vec::new());

        runner.run("parse", |ctx| {
            ctx.measure("work", || {});
        });
        runner.run("storage_write", |ctx| {
            ctx.measure("work", || {});
        });
        let run = runner.finish();

        assert_eq!(run.summaries.len(), 1);
        assert_eq!(run.summaries[0].benchmark_id, "storage/parse/work");
    }

    struct CountingSuiteStartReporter {
        starts: Arc<Mutex<Vec<String>>>,
    }

    impl Reporter for CountingSuiteStartReporter {
        fn suite_start(&self, suite: &str, _config: &StressRunnerConfig) {
            self.starts
                .lock()
                .expect("capture start")
                .push(format!("start:{suite}"));
        }

        fn bench_start(&self, spec: &BenchmarkSpec) {
            self.starts
                .lock()
                .expect("capture bench")
                .push(format!("bench:{}", spec.name));
        }
    }

    #[test]
    fn late_reporters_receive_suite_start_exactly_once_before_bench_events() {
        let replaced = Arc::new(Mutex::new(Vec::new()));
        let added = Arc::new(Mutex::new(Vec::new()));
        let config = StressRunnerConfig::new().samples(1).warmup_samples(0);
        let mut runner = StressRunner::with_config("suite", config);
        runner.reporters(vec![Box::new(CountingSuiteStartReporter {
            starts: Arc::clone(&replaced),
        })]);
        runner.add_reporter(Box::new(CountingSuiteStartReporter {
            starts: Arc::clone(&added),
        }));
        runner.run("one", |ctx| {
            ctx.measure("work", || {});
        });
        runner.run("two", |ctx| {
            ctx.measure("work", || {});
        });
        let _ = runner.finish();

        let expected = vec![
            "start:suite".to_string(),
            "bench:one".to_string(),
            "bench:two".to_string(),
        ];
        assert_eq!(*replaced.lock().expect("replaced"), expected);
        assert_eq!(*added.lock().expect("added"), expected);
    }
}
