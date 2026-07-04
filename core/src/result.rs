//! Raw-sample result types and v2 artifact helpers.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::Path;
use std::time::Duration;

/// Authoritative JSON schema version for cntryl-stress v0.3 artifacts.
pub const SCHEMA_VERSION: &str = "cntryl-stress.v2";

/// Benchmark run profile. Profiles control sample counts, gates, and report depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunProfile {
    /// Fast correctness-focused runs.
    #[default]
    Smoke,
    /// CI/release runs with quality and regression gates.
    Release,
    /// Deep exploratory runs. Correctness still fails, quality is reported.
    Lab,
}

impl fmt::Display for RunProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Smoke => f.write_str("smoke"),
            Self::Release => f.write_str("release"),
            Self::Lab => f.write_str("lab"),
        }
    }
}

impl std::str::FromStr for RunProfile {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "smoke" => Ok(Self::Smoke),
            "release" => Ok(Self::Release),
            "lab" => Ok(Self::Lab),
            other => Err(format!("unknown run profile '{other}'")),
        }
    }
}

/// Static mode family from `#[stress_test(mode = "...")]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkModeKind {
    /// Execute a fixed number of operations for each sample.
    #[default]
    FixedOperations,
    /// Execute work until a fixed wall-clock duration has elapsed.
    FixedDuration,
}

impl fmt::Display for BenchmarkModeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FixedOperations => f.write_str("fixed_operations"),
            Self::FixedDuration => f.write_str("fixed_duration"),
        }
    }
}

impl std::str::FromStr for BenchmarkModeKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "fixed_operations" => Ok(Self::FixedOperations),
            "fixed_duration" => Ok(Self::FixedDuration),
            other => Err(format!("unknown benchmark mode '{other}'")),
        }
    }
}

/// Concrete mode used for a benchmark run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BenchmarkMode {
    /// Run until `sample_duration` elapses.
    FixedDuration {
        /// Wall-clock duration per sample.
        #[serde(with = "duration_serde")]
        sample_duration: Duration,
    },
    /// Run exactly `operations_per_sample` operations per sample.
    FixedOperations {
        /// Operation count per sample.
        operations_per_sample: u64,
    },
}

impl BenchmarkMode {
    /// Return this mode's static mode family.
    #[must_use]
    pub const fn kind(&self) -> BenchmarkModeKind {
        match self {
            Self::FixedDuration { .. } => BenchmarkModeKind::FixedDuration,
            Self::FixedOperations { .. } => BenchmarkModeKind::FixedOperations,
        }
    }
}

impl Default for BenchmarkMode {
    fn default() -> Self {
        Self::FixedOperations {
            operations_per_sample: 1,
        }
    }
}

/// Phase for a raw sample row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplePhase {
    /// Warmup sample retained for reproducibility but excluded from statistics.
    Warmup,
    /// Measured sample included in statistics and comparisons.
    Measured,
    /// Cooldown sample retained for reproducibility but excluded from statistics.
    Cooldown,
}

/// Summary quality classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityClass {
    /// At least 10 measured samples and relative standard deviation <= 5%.
    Authoritative,
    /// At least 5 measured samples and relative standard deviation <= 10%.
    Acceptable,
    /// Correctness passed, but sample count or variance is weak.
    Noisy,
    /// Not usable for performance decisions.
    Untrustworthy,
}

impl fmt::Display for QualityClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authoritative => f.write_str("authoritative"),
            Self::Acceptable => f.write_str("acceptable"),
            Self::Noisy => f.write_str("noisy"),
            Self::Untrustworthy => f.write_str("untrustworthy"),
        }
    }
}

/// Primary metric used for quality and baseline comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimaryMetric {
    /// Operations per second. Higher is better.
    Throughput,
    /// p95 latency in nanoseconds. Lower is better.
    LatencyP95,
    /// Elapsed nanoseconds per completed operation. Lower is better.
    ElapsedPerOperation,
}

impl PrimaryMetric {
    /// Whether larger values are better for this metric.
    #[must_use]
    pub const fn higher_is_better(self) -> bool {
        matches!(self, Self::Throughput)
    }
}

/// Closed 95% confidence interval around the mean.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ConfidenceInterval {
    /// Lower bound.
    pub lower: f64,
    /// Upper bound.
    pub upper: f64,
}

impl ConfidenceInterval {
    /// Return whether two confidence intervals overlap.
    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        self.lower <= other.upper && other.lower <= self.upper
    }
}

/// Statistics computed from measured raw samples only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummaryStats {
    /// Arithmetic mean.
    pub mean: f64,
    /// Median.
    pub median: f64,
    /// Minimum.
    pub min: f64,
    /// Maximum.
    pub max: f64,
    /// Sample standard deviation.
    pub std_dev: f64,
    /// `std_dev / mean`.
    pub relative_std_dev: f64,
    /// 95% confidence interval around the mean.
    pub confidence_interval_95: ConfidenceInterval,
    /// 50th percentile.
    pub p50: f64,
    /// 95th percentile.
    pub p95: f64,
    /// 99th percentile.
    pub p99: f64,
}

impl SummaryStats {
    /// Compute summary statistics from raw values.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn from_values(values: &[f64]) -> Option<Self> {
        let mut sorted: Vec<f64> = values
            .iter()
            .copied()
            .filter(|value| value.is_finite())
            .collect();
        if sorted.is_empty() {
            return None;
        }

        sorted.sort_by(f64::total_cmp);
        let len = sorted.len();
        let sum = sorted.iter().sum::<f64>();
        let mean = sum / len as f64;
        let median = percentile_sorted(&sorted, 0.50);
        let min = sorted[0];
        let max = sorted[len - 1];
        let std_dev = if len < 2 {
            0.0
        } else {
            let variance = sorted
                .iter()
                .map(|value| {
                    let diff = *value - mean;
                    diff * diff
                })
                .sum::<f64>()
                / (len - 1) as f64;
            variance.sqrt()
        };
        let relative_std_dev = if mean == 0.0 {
            f64::INFINITY
        } else {
            std_dev / mean.abs()
        };
        let half_width = if len < 2 {
            0.0
        } else {
            1.96 * (std_dev / (len as f64).sqrt())
        };

        Some(Self {
            mean,
            median,
            min,
            max,
            std_dev,
            relative_std_dev,
            confidence_interval_95: ConfidenceInterval {
                lower: mean - half_width,
                upper: mean + half_width,
            },
            p50: median,
            p95: percentile_sorted(&sorted, 0.95),
            p99: percentile_sorted(&sorted, 0.99),
        })
    }
}

/// Canonical correctness counters. Non-zero error counters fail correctness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CorrectnessCounters {
    /// Operations attempted.
    pub attempted: u64,
    /// Operations completed successfully.
    pub completed: u64,
    /// Failed operations.
    pub failures: u64,
    /// Timed out operations.
    pub timeouts: u64,
    /// Duplicate operations/results.
    pub duplicates: u64,
    /// Dropped operations/results.
    pub dropped: u64,
    /// Validation errors.
    pub validation_errors: u64,
}

impl CorrectnessCounters {
    /// True when no canonical correctness errors were observed.
    #[must_use]
    pub const fn passed(self) -> bool {
        self.failures == 0
            && self.timeouts == 0
            && self.duplicates == 0
            && self.dropped == 0
            && self.validation_errors == 0
            && self.attempted == self.completed
    }

    /// Human-readable correctness error labels.
    #[must_use]
    pub fn error_labels(self) -> Vec<&'static str> {
        let mut labels = Vec::new();
        if self.failures != 0 {
            labels.push("failures");
        }
        if self.timeouts != 0 {
            labels.push("timeouts");
        }
        if self.duplicates != 0 {
            labels.push("duplicates");
        }
        if self.dropped != 0 {
            labels.push("dropped");
        }
        if self.validation_errors != 0 {
            labels.push("validation_errors");
        }
        if self.attempted != self.completed {
            labels.push("attempted_completed_mismatch");
        }
        labels
    }
}

/// Environment captured with a run and each sample.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentInfo {
    /// CPU model, or `"unknown"` when not available.
    pub cpu_model: String,
    /// Logical core count, or `null` when not available.
    pub core_count: Option<usize>,
    /// Operating system description.
    pub os: String,
    /// `rustc --version`, or `"unknown"`.
    pub rustc_version: String,
    /// Allocator name when known.
    pub allocator: String,
    /// Cargo build profile, usually `release` or `debug`.
    pub build_profile: String,
    /// Git commit, or `null` when not available.
    pub git_commit: Option<String>,
    /// cntryl-stress tool version.
    pub tool_version: String,
    /// Command line used for the run.
    pub command_line: Vec<String>,
    /// Resolved profile configuration.
    pub profile_config: ProfileConfig,
}

impl EnvironmentInfo {
    /// Construct an explicit unknown environment for imports/tests.
    #[must_use]
    pub fn unknown(profile_config: ProfileConfig) -> Self {
        Self {
            cpu_model: "unknown".to_string(),
            core_count: None,
            os: "unknown".to_string(),
            rustc_version: "unknown".to_string(),
            allocator: "unknown".to_string(),
            build_profile: "unknown".to_string(),
            git_commit: None,
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            command_line: Vec::new(),
            profile_config,
        }
    }
}

/// Resolved profile configuration stored in the artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileConfig {
    /// Selected run profile.
    pub profile: RunProfile,
    /// Measured samples per benchmark.
    pub measured_samples: usize,
    /// Warmup samples per benchmark.
    pub warmup_samples: usize,
    /// Cooldown samples per benchmark.
    pub cooldown_samples: usize,
    /// Minimum quality required by the profile.
    pub min_quality: QualityClass,
    /// Whether the profile fails when quality is below `min_quality`.
    pub fail_on_quality: bool,
    /// Whether the profile fails on meaningful regressions.
    pub fail_on_regression: bool,
    /// Regression/improvement threshold.
    pub regression_threshold: f64,
    /// Default fixed-duration sample budget.
    #[serde(with = "duration_serde")]
    pub sample_duration: Duration,
    /// Default fixed-operations sample size.
    pub operations_per_sample: u64,
    /// Report depth label.
    pub report_depth: String,
}

impl Default for ProfileConfig {
    fn default() -> Self {
        Self {
            profile: RunProfile::Smoke,
            measured_samples: 1,
            warmup_samples: 0,
            cooldown_samples: 0,
            min_quality: QualityClass::Untrustworthy,
            fail_on_quality: false,
            fail_on_regression: false,
            regression_threshold: 0.05,
            sample_duration: Duration::from_millis(100),
            operations_per_sample: 1,
            report_depth: "summary".to_string(),
        }
    }
}

/// Benchmark specification captured before samples are recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkSpec {
    /// Stable benchmark id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Numeric tier. Tier 1 is intentionally out of scope.
    pub tier: u32,
    /// Concrete execution mode.
    pub mode: BenchmarkMode,
    /// Structured sweep/scaling parameters.
    pub parameters: BTreeMap<String, String>,
    /// Descriptive benchmark metadata.
    pub metadata: BTreeMap<String, String>,
}

/// One raw sample row. This is the authoritative source for summaries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Benchmark id matching `BenchmarkSpec::id`.
    pub benchmark_id: String,
    /// Zero-based sample number within the benchmark.
    pub sample_number: usize,
    /// Sample phase.
    pub phase: SamplePhase,
    /// Elapsed wall-clock time in nanoseconds.
    pub elapsed_ns: u128,
    /// Operations attempted.
    pub operations_attempted: u64,
    /// Operations completed.
    pub operations_completed: u64,
    /// Completed operations per second.
    pub throughput: f64,
    /// Optional raw latency observations in nanoseconds.
    pub latency_ns: Vec<u128>,
    /// Structured parameters active for this sample.
    pub parameters: BTreeMap<String, String>,
    /// Correctness counters.
    pub counters: CorrectnessCounters,
    /// Environment snapshot for this sample.
    pub environment: EnvironmentInfo,
}

impl Sample {
    /// Whether this sample has valid timing.
    #[must_use]
    pub const fn has_valid_timing(&self) -> bool {
        self.elapsed_ns != 0
    }

    /// Whether this sample passed correctness checks.
    #[must_use]
    pub const fn correctness_passed(&self) -> bool {
        self.counters.passed()
    }
}

/// Correctness summary across measured samples.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrectnessSummary {
    /// Whether all measured samples passed canonical correctness checks.
    pub passed: bool,
    /// Aggregated counters across measured samples.
    pub counters: CorrectnessCounters,
    /// Human-readable error labels.
    pub errors: Vec<String>,
}

/// Summary computed from measured samples.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkSummary {
    /// Benchmark id.
    pub benchmark_id: String,
    /// Display name.
    pub name: String,
    /// Numeric tier.
    pub tier: u32,
    /// Primary metric used for comparison.
    pub primary_metric: PrimaryMetric,
    /// Measured sample count.
    pub measured_samples: usize,
    /// Warmup sample count retained in artifact.
    pub warmup_samples: usize,
    /// Cooldown sample count retained in artifact.
    pub cooldown_samples: usize,
    /// Summary statistics from measured samples only.
    pub stats: Option<SummaryStats>,
    /// Quality classification.
    pub quality: QualityClass,
    /// Correctness summary from measured samples.
    pub correctness: CorrectnessSummary,
    /// Structured parameters.
    pub parameters: BTreeMap<String, String>,
    /// Benchmark metadata.
    pub metadata: BTreeMap<String, String>,
}

impl BenchmarkSummary {
    /// Value used for baseline comparison.
    #[must_use]
    pub fn primary_value(&self) -> Option<f64> {
        let stats = self.stats.as_ref()?;
        match self.primary_metric {
            PrimaryMetric::Throughput | PrimaryMetric::ElapsedPerOperation => Some(stats.mean),
            PrimaryMetric::LatencyP95 => Some(stats.p95),
        }
    }
}

/// Baseline comparison classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonClass {
    /// Metric moved past threshold and confidence intervals do not overlap.
    Regression,
    /// Metric improved past threshold and confidence intervals do not overlap.
    Improvement,
    /// Metric did not move past threshold, confidence intervals overlap, or quality is weak.
    Inconclusive,
    /// No matching baseline row.
    MissingBaseline,
}

/// Baseline comparison for one benchmark.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComparisonResult {
    /// Benchmark id.
    pub benchmark_id: String,
    /// Current quality class.
    pub current_quality: QualityClass,
    /// Baseline quality class when present.
    pub baseline_quality: Option<QualityClass>,
    /// Primary metric.
    pub primary_metric: PrimaryMetric,
    /// Baseline primary value.
    pub baseline_value: Option<f64>,
    /// Current primary value.
    pub current_value: Option<f64>,
    /// Current-vs-baseline change percentage.
    pub change_percent: Option<f64>,
    /// Configured threshold.
    pub threshold: f64,
    /// Whether confidence intervals overlap.
    pub confidence_intervals_overlap: Option<bool>,
    /// Comparison classification.
    pub classification: ComparisonClass,
}

/// Complete v2 run artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StressRun {
    /// Schema version. Always `cntryl-stress.v2` for new artifacts.
    pub schema_version: String,
    /// cntryl-stress version.
    pub tool_version: String,
    /// Suite name.
    pub suite: String,
    /// Run profile.
    pub run_profile: RunProfile,
    /// Run-level environment.
    pub environment: EnvironmentInfo,
    /// Benchmark specifications.
    pub benchmark_specs: Vec<BenchmarkSpec>,
    /// Raw samples, including warmup/measured/cooldown.
    pub samples: Vec<Sample>,
    /// Measured-sample summaries.
    pub summaries: Vec<BenchmarkSummary>,
    /// Baseline comparisons, when requested.
    pub comparisons: Vec<ComparisonResult>,
    /// Timestamp/run id.
    pub started_at: String,
    /// Total elapsed wall-clock time in nanoseconds.
    pub total_elapsed_ns: u128,
    /// Run metadata.
    pub metadata: BTreeMap<String, String>,
}

impl StressRun {
    /// Load a v2 artifact.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or does not contain valid JSON.
    pub fn load(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Self::from_json_str(&content)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    }

    /// Parse a v2 artifact.
    ///
    /// # Errors
    ///
    /// Returns a serde error when JSON is invalid or cannot be imported.
    pub fn from_json_str(content: &str) -> Result<Self, serde_json::Error> {
        let run: Self = serde_json::from_str(content)?;
        if run.schema_version == SCHEMA_VERSION {
            Ok(run)
        } else {
            Err(serde::de::Error::custom(format!(
                "unsupported schema_version '{}'",
                run.schema_version
            )))
        }
    }

    /// Whether every measured summary passed correctness.
    #[must_use]
    pub fn correctness_passed(&self) -> bool {
        self.summaries
            .iter()
            .all(|summary| summary.correctness.passed)
    }

    /// Whether every summary satisfies the requested minimum quality.
    #[must_use]
    pub fn meets_min_quality(&self, min_quality: QualityClass) -> bool {
        self.summaries
            .iter()
            .all(|summary| quality_rank(summary.quality) >= quality_rank(min_quality))
    }

    /// Meaningful regression rows.
    #[must_use]
    pub fn regressions(&self) -> Vec<&ComparisonResult> {
        self.comparisons
            .iter()
            .filter(|comparison| comparison.classification == ComparisonClass::Regression)
            .collect()
    }
}

/// Summarize one benchmark from raw samples.
#[must_use]
pub fn summarize_benchmark(spec: &BenchmarkSpec, samples: &[Sample]) -> BenchmarkSummary {
    let measured: Vec<&Sample> = samples
        .iter()
        .filter(|sample| sample.benchmark_id == spec.id && sample.phase == SamplePhase::Measured)
        .collect();
    let warmup_samples = samples
        .iter()
        .filter(|sample| sample.benchmark_id == spec.id && sample.phase == SamplePhase::Warmup)
        .count();
    let cooldown_samples = samples
        .iter()
        .filter(|sample| sample.benchmark_id == spec.id && sample.phase == SamplePhase::Cooldown)
        .count();
    let correctness = summarize_correctness(&measured);
    let primary_metric = infer_primary_metric(spec, &measured);
    let values = primary_values(primary_metric, &measured);
    let stats = SummaryStats::from_values(&values);
    let quality = classify_quality(
        measured.len(),
        stats.as_ref(),
        correctness.passed,
        &measured,
    );

    BenchmarkSummary {
        benchmark_id: spec.id.clone(),
        name: spec.name.clone(),
        tier: spec.tier,
        primary_metric,
        measured_samples: measured.len(),
        warmup_samples,
        cooldown_samples,
        stats,
        quality,
        correctness,
        parameters: merged_parameters(spec, &measured),
        metadata: spec.metadata.clone(),
    }
}

/// Compare current summaries to baseline summaries.
#[must_use]
pub fn compare_summaries(
    current: &[BenchmarkSummary],
    baseline: &[BenchmarkSummary],
    threshold: f64,
) -> Vec<ComparisonResult> {
    let by_id: HashMap<&str, &BenchmarkSummary> = baseline
        .iter()
        .map(|summary| (summary.benchmark_id.as_str(), summary))
        .collect();
    let by_name: HashMap<&str, &BenchmarkSummary> = baseline
        .iter()
        .map(|summary| (summary.name.as_str(), summary))
        .collect();

    current
        .iter()
        .map(|summary| {
            let baseline_summary = by_id
                .get(summary.benchmark_id.as_str())
                .copied()
                .or_else(|| by_name.get(summary.name.as_str()).copied());
            compare_one_summary(summary, baseline_summary, threshold)
        })
        .collect()
}

fn compare_one_summary(
    current: &BenchmarkSummary,
    baseline: Option<&BenchmarkSummary>,
    threshold: f64,
) -> ComparisonResult {
    let Some(baseline) = baseline else {
        return ComparisonResult {
            benchmark_id: current.benchmark_id.clone(),
            current_quality: current.quality,
            baseline_quality: None,
            primary_metric: current.primary_metric,
            baseline_value: None,
            current_value: current.primary_value(),
            change_percent: None,
            threshold,
            confidence_intervals_overlap: None,
            classification: ComparisonClass::MissingBaseline,
        };
    };

    let baseline_value = baseline.primary_value();
    let current_value = current.primary_value();
    let change_percent = baseline_value
        .zip(current_value)
        .map(|(base, current)| ((current / base) - 1.0) * 100.0);
    let confidence_intervals_overlap =
        baseline
            .stats
            .as_ref()
            .zip(current.stats.as_ref())
            .map(|(base, current)| {
                base.confidence_interval_95
                    .overlaps(current.confidence_interval_95)
            });
    let classification = classify_comparison(
        current.primary_metric,
        baseline_value,
        current_value,
        threshold,
        confidence_intervals_overlap,
    );

    ComparisonResult {
        benchmark_id: current.benchmark_id.clone(),
        current_quality: current.quality,
        baseline_quality: Some(baseline.quality),
        primary_metric: current.primary_metric,
        baseline_value,
        current_value,
        change_percent,
        threshold,
        confidence_intervals_overlap,
        classification,
    }
}

fn classify_comparison(
    metric: PrimaryMetric,
    baseline_value: Option<f64>,
    current_value: Option<f64>,
    threshold: f64,
    confidence_intervals_overlap: Option<bool>,
) -> ComparisonClass {
    let Some((base, current)) = baseline_value.zip(current_value) else {
        return ComparisonClass::Inconclusive;
    };
    if base <= 0.0 || current <= 0.0 {
        return ComparisonClass::Inconclusive;
    }
    if confidence_intervals_overlap.unwrap_or(true) {
        return ComparisonClass::Inconclusive;
    }

    if metric.higher_is_better() {
        if current < base * (1.0 - threshold) {
            ComparisonClass::Regression
        } else if current > base * (1.0 + threshold) {
            ComparisonClass::Improvement
        } else {
            ComparisonClass::Inconclusive
        }
    } else if current > base * (1.0 + threshold) {
        ComparisonClass::Regression
    } else if current < base * (1.0 - threshold) {
        ComparisonClass::Improvement
    } else {
        ComparisonClass::Inconclusive
    }
}

fn merged_parameters(
    spec: &BenchmarkSpec,
    measured_samples: &[&Sample],
) -> BTreeMap<String, String> {
    let mut parameters = spec.parameters.clone();
    if let Some(sample) = measured_samples.first() {
        parameters.extend(sample.parameters.clone());
    }
    parameters
}

fn summarize_correctness(samples: &[&Sample]) -> CorrectnessSummary {
    let counters = samples
        .iter()
        .fold(CorrectnessCounters::default(), |mut acc, sample| {
            acc.attempted = acc.attempted.saturating_add(sample.counters.attempted);
            acc.completed = acc.completed.saturating_add(sample.counters.completed);
            acc.failures = acc.failures.saturating_add(sample.counters.failures);
            acc.timeouts = acc.timeouts.saturating_add(sample.counters.timeouts);
            acc.duplicates = acc.duplicates.saturating_add(sample.counters.duplicates);
            acc.dropped = acc.dropped.saturating_add(sample.counters.dropped);
            acc.validation_errors = acc
                .validation_errors
                .saturating_add(sample.counters.validation_errors);
            acc
        });
    let errors = counters
        .error_labels()
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();

    CorrectnessSummary {
        passed: counters.passed(),
        counters,
        errors,
    }
}

fn infer_primary_metric(spec: &BenchmarkSpec, samples: &[&Sample]) -> PrimaryMetric {
    if spec
        .metadata
        .get("primary_metric")
        .is_some_and(|value| value == "latency")
        && samples.iter().any(|sample| !sample.latency_ns.is_empty())
    {
        return PrimaryMetric::LatencyP95;
    }
    if samples
        .iter()
        .any(|sample| sample.throughput.is_finite() && sample.throughput > 0.0)
    {
        PrimaryMetric::Throughput
    } else {
        PrimaryMetric::ElapsedPerOperation
    }
}

#[allow(clippy::cast_precision_loss)]
fn primary_values(metric: PrimaryMetric, samples: &[&Sample]) -> Vec<f64> {
    match metric {
        PrimaryMetric::Throughput => samples.iter().map(|sample| sample.throughput).collect(),
        PrimaryMetric::LatencyP95 => samples
            .iter()
            .flat_map(|sample| sample.latency_ns.iter().map(|latency| *latency as f64))
            .collect(),
        PrimaryMetric::ElapsedPerOperation => samples
            .iter()
            .filter_map(|sample| {
                if sample.operations_completed == 0 {
                    None
                } else {
                    Some(sample.elapsed_ns as f64 / sample.operations_completed as f64)
                }
            })
            .collect(),
    }
}

fn classify_quality(
    measured_samples: usize,
    stats: Option<&SummaryStats>,
    correctness_passed: bool,
    samples: &[&Sample],
) -> QualityClass {
    if !correctness_passed
        || measured_samples < 2
        || samples.iter().any(|sample| {
            !sample.has_valid_timing()
                || sample.operations_completed == 0
                || !sample.throughput.is_finite()
        })
    {
        return QualityClass::Untrustworthy;
    }

    let Some(stats) = stats else {
        return QualityClass::Untrustworthy;
    };

    if measured_samples >= 10 && stats.relative_std_dev <= 0.05 {
        QualityClass::Authoritative
    } else if measured_samples >= 5 && stats.relative_std_dev <= 0.10 {
        QualityClass::Acceptable
    } else {
        QualityClass::Noisy
    }
}

const fn quality_rank(quality: QualityClass) -> u8 {
    match quality {
        QualityClass::Untrustworthy => 0,
        QualityClass::Noisy => 1,
        QualityClass::Acceptable => 2,
        QualityClass::Authoritative => 3,
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn percentile_sorted(sorted: &[f64], quantile: f64) -> f64 {
    debug_assert!(!sorted.is_empty());
    if sorted.len() == 1 {
        return sorted[0];
    }
    let index = ((sorted.len() - 1) as f64 * quantile).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

pub(crate) mod duration_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
        duration.as_nanos().serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
        let nanos = u128::deserialize(deserializer)?;
        u64::try_from(nanos)
            .map(Duration::from_nanos)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_env() -> EnvironmentInfo {
        EnvironmentInfo::unknown(ProfileConfig::default())
    }

    fn spec(id: &str) -> BenchmarkSpec {
        BenchmarkSpec {
            id: id.to_string(),
            name: id.to_string(),
            tier: 2,
            mode: BenchmarkMode::FixedOperations {
                operations_per_sample: 1,
            },
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn sample(id: &str, phase: SamplePhase, sample_number: usize, elapsed_ns: u128) -> Sample {
        let completed = 1;
        let throughput = if elapsed_ns == 0 {
            0.0
        } else {
            1_000_000_000.0 / elapsed_ns as f64
        };
        Sample {
            benchmark_id: id.to_string(),
            sample_number,
            phase,
            elapsed_ns,
            operations_attempted: completed,
            operations_completed: completed,
            throughput,
            latency_ns: Vec::new(),
            parameters: BTreeMap::new(),
            counters: CorrectnessCounters {
                attempted: completed,
                completed,
                ..CorrectnessCounters::default()
            },
            environment: test_env(),
        }
    }

    #[test]
    fn summaries_use_measured_samples_and_retain_warmup_counts() {
        let spec = spec("bench");
        let samples = vec![
            sample("bench", SamplePhase::Warmup, 0, 1_000_000_000),
            sample("bench", SamplePhase::Measured, 1, 100),
            sample("bench", SamplePhase::Measured, 2, 200),
            sample("bench", SamplePhase::Cooldown, 3, 1_000_000_000),
        ];

        let summary = summarize_benchmark(&spec, &samples);

        assert_eq!(summary.warmup_samples, 1);
        assert_eq!(summary.measured_samples, 2);
        assert_eq!(summary.cooldown_samples, 1);
        assert_close(summary.stats.as_ref().expect("stats").min, 5_000_000.0);
    }

    #[test]
    fn stats_include_core_moments_and_percentiles() {
        let stats = SummaryStats::from_values(&[1.0, 2.0, 3.0, 4.0, 5.0]).expect("stats");

        assert_close(stats.mean, 3.0);
        assert_close(stats.median, 3.0);
        assert_close(stats.min, 1.0);
        assert_close(stats.max, 5.0);
        assert!((stats.std_dev - 1.581_138_830_084_189_8).abs() < 1e-12);
        assert!((stats.relative_std_dev - 0.527_046_276_694_729_9).abs() < 1e-12);
        assert_close(stats.p50, 3.0);
        assert_close(stats.p95, 5.0);
        assert_close(stats.p99, 5.0);
        assert!(stats.confidence_interval_95.lower < stats.mean);
        assert!(stats.confidence_interval_95.upper > stats.mean);
    }

    #[test]
    fn latency_primary_metric_uses_raw_latency_samples() {
        let mut spec = spec("latency");
        spec.metadata
            .insert("primary_metric".to_string(), "latency".to_string());
        let mut s = sample("latency", SamplePhase::Measured, 0, 1_000_000);
        s.latency_ns = (1_u128..=100).collect();
        let mut s2 = sample("latency", SamplePhase::Measured, 1, 1_000_000);
        s2.latency_ns = (101_u128..=200).collect();

        let summary = summarize_benchmark(&spec, &[s, s2]);

        assert_eq!(summary.primary_metric, PrimaryMetric::LatencyP95);
        assert_close(summary.stats.as_ref().expect("stats").p50, 101.0);
        assert_close(summary.stats.as_ref().expect("stats").p95, 190.0);
        assert_close(summary.stats.as_ref().expect("stats").p99, 198.0);
    }

    #[test]
    fn quality_classifies_authoritative_acceptable_noisy_and_untrustworthy() {
        let spec = spec("bench");
        let authoritative = (0..10)
            .map(|i| sample("bench", SamplePhase::Measured, i, 100 + i as u128))
            .collect::<Vec<_>>();
        assert_eq!(
            summarize_benchmark(&spec, &authoritative).quality,
            QualityClass::Authoritative
        );

        let acceptable = (0..5)
            .map(|i| sample("bench", SamplePhase::Measured, i, 100 + i as u128))
            .collect::<Vec<_>>();
        assert_eq!(
            summarize_benchmark(&spec, &acceptable).quality,
            QualityClass::Acceptable
        );

        let noisy = vec![
            sample("bench", SamplePhase::Measured, 0, 100),
            sample("bench", SamplePhase::Measured, 1, 300),
            sample("bench", SamplePhase::Measured, 2, 1000),
        ];
        assert_eq!(
            summarize_benchmark(&spec, &noisy).quality,
            QualityClass::Noisy
        );

        let untrustworthy = vec![sample("bench", SamplePhase::Measured, 0, 100)];
        assert_eq!(
            summarize_benchmark(&spec, &untrustworthy).quality,
            QualityClass::Untrustworthy
        );
    }

    #[test]
    fn correctness_errors_make_summary_untrustworthy() {
        let spec = spec("bench");
        let mut bad = sample("bench", SamplePhase::Measured, 0, 100);
        bad.counters.failures = 1;
        let good = sample("bench", SamplePhase::Measured, 1, 101);

        let summary = summarize_benchmark(&spec, &[bad, good]);

        assert!(!summary.correctness.passed);
        assert!(summary.correctness.errors.contains(&"failures".to_string()));
        assert_eq!(summary.quality, QualityClass::Untrustworthy);
    }

    #[test]
    fn comparison_requires_threshold_and_non_overlapping_confidence_intervals() {
        let mut baseline = summarize_benchmark(
            &spec("bench"),
            &(0..10)
                .map(|i| sample("bench", SamplePhase::Measured, i, 100 + i as u128))
                .collect::<Vec<_>>(),
        );
        let mut current = summarize_benchmark(
            &spec("bench"),
            &(0..10)
                .map(|i| sample("bench", SamplePhase::Measured, i, 200 + i as u128))
                .collect::<Vec<_>>(),
        );

        baseline
            .stats
            .as_mut()
            .expect("stats")
            .confidence_interval_95 = ConfidenceInterval {
            lower: 9.0,
            upper: 10.0,
        };
        current
            .stats
            .as_mut()
            .expect("stats")
            .confidence_interval_95 = ConfidenceInterval {
            lower: 4.0,
            upper: 5.0,
        };
        assert_eq!(
            compare_summaries(&[current.clone()], &[baseline.clone()], 0.05)[0].classification,
            ComparisonClass::Regression
        );

        current
            .stats
            .as_mut()
            .expect("stats")
            .confidence_interval_95 = ConfidenceInterval {
            lower: 9.5,
            upper: 10.5,
        };
        assert_eq!(
            compare_summaries(&[current], &[baseline], 0.05)[0].classification,
            ComparisonClass::Inconclusive
        );
    }

    #[test]
    fn comparison_detects_meaningful_improvement() {
        let mut baseline = summarize_benchmark(
            &spec("bench"),
            &(0..10)
                .map(|i| sample("bench", SamplePhase::Measured, i, 200 + i as u128))
                .collect::<Vec<_>>(),
        );
        let mut current = summarize_benchmark(
            &spec("bench"),
            &(0..10)
                .map(|i| sample("bench", SamplePhase::Measured, i, 100 + i as u128))
                .collect::<Vec<_>>(),
        );
        baseline
            .stats
            .as_mut()
            .expect("stats")
            .confidence_interval_95 = ConfidenceInterval {
            lower: 4.0,
            upper: 5.0,
        };
        current
            .stats
            .as_mut()
            .expect("stats")
            .confidence_interval_95 = ConfidenceInterval {
            lower: 9.0,
            upper: 10.0,
        };

        assert_eq!(
            compare_summaries(&[current], &[baseline], 0.05)[0].classification,
            ComparisonClass::Improvement
        );
    }

    #[test]
    fn v2_json_contains_schema_version() {
        let profile_config = ProfileConfig::default();
        let run = StressRun {
            schema_version: SCHEMA_VERSION.to_string(),
            tool_version: "0.3.0".to_string(),
            suite: "suite".to_string(),
            run_profile: RunProfile::Smoke,
            environment: EnvironmentInfo::unknown(profile_config),
            benchmark_specs: Vec::new(),
            samples: Vec::new(),
            summaries: Vec::new(),
            comparisons: Vec::new(),
            started_at: "123".to_string(),
            total_elapsed_ns: 0,
            metadata: BTreeMap::new(),
        };

        let json = serde_json::to_value(&run).expect("serialize");

        assert_eq!(json["schema_version"], SCHEMA_VERSION);
        assert_eq!(json["samples"].as_array().expect("samples").len(), 0);
    }

    #[test]
    fn old_aggregate_json_is_not_accepted() {
        let old = r#"{"suite": "old", "results": []}"#;

        assert!(StressRun::from_json_str(old).is_err());
    }

    #[test]
    fn wrong_schema_version_is_not_accepted() {
        let profile_config = ProfileConfig::default();
        let run = StressRun {
            schema_version: "cntryl-stress.v999".to_string(),
            tool_version: "0.3.0".to_string(),
            suite: "suite".to_string(),
            run_profile: RunProfile::Smoke,
            environment: EnvironmentInfo::unknown(profile_config),
            benchmark_specs: Vec::new(),
            samples: Vec::new(),
            summaries: Vec::new(),
            comparisons: Vec::new(),
            started_at: "123".to_string(),
            total_elapsed_ns: 0,
            metadata: BTreeMap::new(),
        };
        let json = serde_json::to_string(&run).expect("serialize");

        assert!(StressRun::from_json_str(&json).is_err());
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < f64::EPSILON,
            "expected {actual} to equal {expected}"
        );
    }
}
