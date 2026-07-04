//! Raw-sample result types and current artifact helpers.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::Path;
use std::time::Duration;

/// Authoritative JSON schema version for current cntryl-stress artifacts.
pub const SCHEMA_VERSION: &str = "cntryl-stress.v1";

/// Benchmark run profile. The default profile is the trustworthy release gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunProfile {
    /// Fast correctness-focused diagnostic runs.
    Smoke,
    /// Trustworthy runs with quality and regression gates.
    #[default]
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
    /// Calibrated batched microbenchmark samples.
    Micro,
    /// Execute a fixed number of operations for each sample.
    #[default]
    FixedOperations,
    /// Execute work until a fixed wall-clock duration has elapsed.
    FixedDuration,
}

impl fmt::Display for BenchmarkModeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Micro => f.write_str("micro"),
            Self::FixedOperations => f.write_str("fixed_operations"),
            Self::FixedDuration => f.write_str("fixed_duration"),
        }
    }
}

impl std::str::FromStr for BenchmarkModeKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "micro" => Ok(Self::Micro),
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
    /// Calibrate a batched sample to a target wall-clock duration.
    Micro {
        /// Target wall-clock duration for each measured batch.
        #[serde(with = "duration_serde")]
        target_sample_duration: Duration,
    },
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
            Self::Micro { .. } => BenchmarkModeKind::Micro,
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
    /// Net nanoseconds per completed operation. Lower is better.
    NsPerOp,
}

impl PrimaryMetric {
    /// Whether larger values are better for this metric.
    #[must_use]
    pub const fn higher_is_better(self) -> bool {
        matches!(self, Self::Throughput)
    }
}

/// Per-benchmark budget gates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct BenchmarkBudgets {
    /// Maximum net nanoseconds per operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_ns_per_op: Option<f64>,
    /// Maximum allocations per operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_allocs_per_op: Option<f64>,
    /// Maximum allocated bytes per operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes_per_op: Option<f64>,
    /// Maximum lower-is-better regression percentage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_regression_pct: Option<f64>,
    /// Maximum relative standard deviation percentage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rsd_pct: Option<f64>,
}

impl BenchmarkBudgets {
    /// Whether allocation counters are required by this budget.
    #[must_use]
    pub const fn requires_allocation_tracking(self) -> bool {
        self.max_allocs_per_op.is_some() || self.max_bytes_per_op.is_some()
    }

    /// Whether at least one budget is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.max_ns_per_op.is_none()
            && self.max_allocs_per_op.is_none()
            && self.max_bytes_per_op.is_none()
            && self.max_regression_pct.is_none()
            && self.max_rsd_pct.is_none()
    }
}

/// Result for one budget gate on one benchmark summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetResult {
    /// Budget metric name.
    pub metric: String,
    /// Configured budget limit.
    pub limit: f64,
    /// Observed value when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual: Option<f64>,
    /// Whether the observed value satisfied the budget.
    pub passed: bool,
    /// Failure detail when the value was unavailable or over budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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
    /// Construct an explicit unknown environment for fallback paths and tests.
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
    /// Target duration for calibrated micro samples.
    #[serde(with = "duration_serde")]
    pub micro_sample_duration: Duration,
    /// Report depth label.
    pub report_depth: String,
}

impl Default for ProfileConfig {
    fn default() -> Self {
        Self {
            profile: RunProfile::Release,
            measured_samples: 10,
            warmup_samples: 1,
            cooldown_samples: 0,
            min_quality: QualityClass::Acceptable,
            fail_on_quality: true,
            fail_on_regression: true,
            regression_threshold: 0.05,
            sample_duration: Duration::from_secs(1),
            operations_per_sample: 1,
            micro_sample_duration: Duration::from_millis(100),
            report_depth: "gated".to_string(),
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
    /// Numeric tier.
    pub tier: u32,
    /// Concrete execution mode.
    pub mode: BenchmarkMode,
    /// Budget gates for this benchmark.
    #[serde(default)]
    pub budgets: BenchmarkBudgets,
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
    /// Calibrated operation count for micro samples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibrated_iterations: Option<u64>,
    /// Gross measured batch duration in nanoseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gross_elapsed_ns: Option<u128>,
    /// Empty-loop overhead for the batch in nanoseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overhead_ns: Option<u128>,
    /// Net batch duration after subtracting overhead in nanoseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_elapsed_ns: Option<u128>,
    /// Gross nanoseconds per operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gross_ns_per_op: Option<f64>,
    /// Empty-loop overhead nanoseconds per operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overhead_ns_per_op: Option<f64>,
    /// Net nanoseconds per operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_ns_per_op: Option<f64>,
    /// Allocations observed in this measured operation batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocs: Option<u64>,
    /// Allocated bytes observed in this measured operation batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Allocations per operation when allocation tracking is installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocs_per_op: Option<f64>,
    /// Allocated bytes per operation when allocation tracking is installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_per_op: Option<f64>,
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
    pub fn has_valid_timing(&self) -> bool {
        self.elapsed_ns != 0
            && self
                .net_ns_per_op
                .is_none_or(|value| value.is_finite() && value > 0.0)
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
    /// Net nanoseconds per operation statistics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ns_per_op: Option<SummaryStats>,
    /// Gross nanoseconds per operation statistics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gross_ns_per_op: Option<SummaryStats>,
    /// Empty-loop overhead nanoseconds per operation statistics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overhead_ns_per_op: Option<SummaryStats>,
    /// Allocations per operation statistics when tracked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocs_per_op: Option<SummaryStats>,
    /// Allocated bytes per operation statistics when tracked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_per_op: Option<SummaryStats>,
    /// Quality classification.
    pub quality: QualityClass,
    /// Budget gates copied from the spec.
    #[serde(default)]
    pub budgets: BenchmarkBudgets,
    /// Budget results derived from measured samples.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub budget_results: Vec<BudgetResult>,
    /// Machine-readable quality and attention flags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<String>,
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
            PrimaryMetric::Throughput | PrimaryMetric::NsPerOp => Some(stats.mean),
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

/// Complete current run artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StressRun {
    /// Schema version. Always `cntryl-stress.v1` for new artifacts.
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
    /// Load a current artifact.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or does not contain valid JSON.
    pub fn load(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Self::from_json_str(&content)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    }

    /// Parse a current artifact.
    ///
    /// # Errors
    ///
    /// Returns a serde error when JSON is invalid or uses a different schema.
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

    /// Whether every measured summary passed configured budgets.
    #[must_use]
    pub fn budgets_passed(&self) -> bool {
        self.summaries.iter().all(|summary| {
            summary
                .budget_results
                .iter()
                .all(|budget_result| budget_result.passed)
        })
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
    let ns_per_op = SummaryStats::from_values(&per_op_values(&measured, |sample| {
        sample.net_ns_per_op.or_else(|| elapsed_ns_per_op(sample))
    }));
    let gross_ns_per_op =
        SummaryStats::from_values(&per_op_values(&measured, |sample| sample.gross_ns_per_op));
    let overhead_ns_per_op = SummaryStats::from_values(&per_op_values(&measured, |sample| {
        sample.overhead_ns_per_op
    }));
    let allocs_per_op =
        SummaryStats::from_values(&per_op_values(&measured, |sample| sample.allocs_per_op));
    let bytes_per_op =
        SummaryStats::from_values(&per_op_values(&measured, |sample| sample.bytes_per_op));
    let values = primary_values(primary_metric, &measured);
    let stats = SummaryStats::from_values(&values);
    let budget_results = evaluate_budgets(
        spec.budgets,
        stats.as_ref(),
        ns_per_op.as_ref(),
        allocs_per_op.as_ref(),
        bytes_per_op.as_ref(),
    );
    let flags = summary_flags(
        spec,
        &measured,
        ns_per_op.as_ref(),
        overhead_ns_per_op.as_ref(),
        &budget_results,
    );
    let quality = classify_quality(
        measured.len(),
        stats.as_ref(),
        correctness.passed,
        &measured,
        &flags,
        &budget_results,
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
        ns_per_op,
        gross_ns_per_op,
        overhead_ns_per_op,
        allocs_per_op,
        bytes_per_op,
        quality,
        budgets: spec.budgets,
        budget_results,
        flags,
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
    let threshold = current
        .budgets
        .max_regression_pct
        .map_or(threshold, |pct| pct / 100.0);
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
    if spec.mode.kind() == BenchmarkModeKind::Micro {
        return PrimaryMetric::NsPerOp;
    }
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
        PrimaryMetric::NsPerOp
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
        PrimaryMetric::NsPerOp => samples
            .iter()
            .filter_map(|sample| sample.net_ns_per_op.or_else(|| elapsed_ns_per_op(sample)))
            .collect(),
    }
}

fn per_op_values<F>(samples: &[&Sample], mut value_for: F) -> Vec<f64>
where
    F: FnMut(&Sample) -> Option<f64>,
{
    samples
        .iter()
        .filter_map(|sample| value_for(sample))
        .filter(|value| value.is_finite())
        .collect()
}

#[allow(clippy::cast_precision_loss)]
fn elapsed_ns_per_op(sample: &Sample) -> Option<f64> {
    (sample.operations_completed != 0)
        .then(|| sample.elapsed_ns as f64 / sample.operations_completed as f64)
        .filter(|value| value.is_finite())
}

fn evaluate_budgets(
    budgets: BenchmarkBudgets,
    primary_stats: Option<&SummaryStats>,
    ns_per_op: Option<&SummaryStats>,
    allocs_per_op: Option<&SummaryStats>,
    bytes_per_op: Option<&SummaryStats>,
) -> Vec<BudgetResult> {
    let mut results = Vec::new();
    push_max_budget(
        &mut results,
        "max_ns_per_op",
        budgets.max_ns_per_op,
        ns_per_op.map(|stats| stats.mean),
    );
    push_max_budget(
        &mut results,
        "max_allocs_per_op",
        budgets.max_allocs_per_op,
        allocs_per_op.map(|stats| stats.mean),
    );
    push_max_budget(
        &mut results,
        "max_bytes_per_op",
        budgets.max_bytes_per_op,
        bytes_per_op.map(|stats| stats.mean),
    );
    push_max_budget(
        &mut results,
        "max_rsd_pct",
        budgets.max_rsd_pct,
        primary_stats.map(|stats| stats.relative_std_dev * 100.0),
    );
    results
}

fn push_max_budget(
    results: &mut Vec<BudgetResult>,
    metric: &'static str,
    limit: Option<f64>,
    actual: Option<f64>,
) {
    let Some(limit) = limit else {
        return;
    };
    let passed = actual.is_some_and(|actual| actual <= limit);
    results.push(BudgetResult {
        metric: metric.to_string(),
        limit,
        actual,
        passed,
        reason: (!passed).then(|| match actual {
            Some(actual) => format!("{actual:.4} exceeds {limit:.4}"),
            None => "required measurement is unavailable".to_string(),
        }),
    });
}

fn summary_flags(
    spec: &BenchmarkSpec,
    samples: &[&Sample],
    ns_per_op: Option<&SummaryStats>,
    overhead_ns_per_op: Option<&SummaryStats>,
    budget_results: &[BudgetResult],
) -> Vec<String> {
    let mut flags = Vec::new();
    if samples.iter().any(|sample| !sample.has_valid_timing()) {
        flags.push("invalid_timing".to_string());
    }
    if samples
        .iter()
        .any(|sample| sample.operations_completed == 0)
    {
        flags.push("zero_completed_ops".to_string());
    }
    if has_overhead_dominant_sample(samples, overhead_ns_per_op) {
        flags.push("overhead_dominant".to_string());
    }
    if spec.budgets.requires_allocation_tracking()
        && samples
            .iter()
            .any(|sample| sample.allocs_per_op.is_none() || sample.bytes_per_op.is_none())
    {
        flags.push("allocation_tracking_required".to_string());
    }
    if budget_results.iter().any(|result| !result.passed) {
        flags.push("budget_failed".to_string());
    }
    if spec.mode.kind() == BenchmarkModeKind::Micro
        && !micro_is_validated(spec)
        && ns_per_op.is_some_and(|stats| stats.mean < 5.0)
    {
        flags.push("suspicious_micro".to_string());
    }
    flags
}

fn has_overhead_dominant_sample(
    samples: &[&Sample],
    overhead_stats: Option<&SummaryStats>,
) -> bool {
    samples.iter().any(|sample| {
        sample
            .overhead_ns_per_op
            .zip(sample.net_ns_per_op)
            .is_some_and(|(overhead, net)| overhead >= net)
    }) || overhead_stats.is_some_and(|stats| stats.mean > 0.0)
        && samples.iter().any(|sample| {
            sample
                .overhead_ns_per_op
                .zip(sample.gross_ns_per_op)
                .is_some_and(|(overhead, gross)| gross > 0.0 && overhead / gross >= 0.5)
        })
}

fn micro_is_validated(spec: &BenchmarkSpec) -> bool {
    spec.metadata
        .get("validated_micro")
        .or_else(|| spec.metadata.get("micro_validated"))
        .is_some_and(|value| value == "true")
}

fn classify_quality(
    measured_samples: usize,
    stats: Option<&SummaryStats>,
    correctness_passed: bool,
    samples: &[&Sample],
    flags: &[String],
    budget_results: &[BudgetResult],
) -> QualityClass {
    if !correctness_passed
        || measured_samples < 2
        || budget_results.iter().any(|result| !result.passed)
        || flags.iter().any(|flag| {
            matches!(
                flag.as_str(),
                "invalid_timing"
                    | "zero_completed_ops"
                    | "overhead_dominant"
                    | "allocation_tracking_required"
                    | "budget_failed"
            )
        })
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
            budgets: BenchmarkBudgets::default(),
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
        }
    }

    fn micro_spec(id: &str) -> BenchmarkSpec {
        BenchmarkSpec {
            id: id.to_string(),
            name: id.to_string(),
            tier: 1,
            mode: BenchmarkMode::Micro {
                target_sample_duration: Duration::from_millis(100),
            },
            budgets: BenchmarkBudgets::default(),
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
            calibrated_iterations: None,
            gross_elapsed_ns: None,
            overhead_ns: None,
            net_elapsed_ns: None,
            gross_ns_per_op: None,
            overhead_ns_per_op: None,
            net_ns_per_op: None,
            allocs: None,
            bytes: None,
            allocs_per_op: None,
            bytes_per_op: None,
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

    fn micro_sample(id: &str, sample_number: usize, net_ns_per_op: u32) -> Sample {
        let mut sample = sample(
            id,
            SamplePhase::Measured,
            sample_number,
            u128::from(net_ns_per_op),
        );
        let net_ns_per_op = f64::from(net_ns_per_op);
        sample.calibrated_iterations = Some(100);
        sample.gross_elapsed_ns = Some(1_200);
        sample.overhead_ns = Some(200);
        sample.net_elapsed_ns = Some(1_000);
        sample.gross_ns_per_op = Some(12.0);
        sample.overhead_ns_per_op = Some(2.0);
        sample.net_ns_per_op = Some(net_ns_per_op);
        sample
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
    fn micro_summary_uses_net_ns_per_op_and_flags_suspicious_rows() {
        let spec = micro_spec("hot_path");
        let samples = (0..5)
            .map(|i| micro_sample("hot_path", i, 4))
            .collect::<Vec<_>>();

        let summary = summarize_benchmark(&spec, &samples);

        assert_eq!(summary.primary_metric, PrimaryMetric::NsPerOp);
        assert_close(summary.primary_value().expect("value"), 4.0);
        assert_eq!(summary.quality, QualityClass::Acceptable);
        assert!(summary.flags.contains(&"suspicious_micro".to_string()));
        assert_close(summary.overhead_ns_per_op.expect("overhead").mean, 2.0);
    }

    #[test]
    fn failed_absolute_budget_makes_summary_untrustworthy() {
        let mut spec = micro_spec("budgeted");
        spec.budgets.max_ns_per_op = Some(10.0);
        let samples = (0..5)
            .map(|i| micro_sample("budgeted", i, 20))
            .collect::<Vec<_>>();

        let summary = summarize_benchmark(&spec, &samples);

        assert_eq!(summary.quality, QualityClass::Untrustworthy);
        assert!(summary.flags.contains(&"budget_failed".to_string()));
        assert_eq!(summary.budget_results.len(), 1);
        assert!(!summary.budget_results[0].passed);
    }

    #[test]
    fn allocation_budgets_require_allocation_tracking() {
        let mut spec = micro_spec("alloc_budget");
        spec.budgets.max_allocs_per_op = Some(0.0);
        let samples = (0..5)
            .map(|i| micro_sample("alloc_budget", i, 20))
            .collect::<Vec<_>>();

        let summary = summarize_benchmark(&spec, &samples);

        assert_eq!(summary.quality, QualityClass::Untrustworthy);
        assert!(summary
            .flags
            .contains(&"allocation_tracking_required".to_string()));
        assert!(summary
            .budget_results
            .iter()
            .any(|result| !result.passed && result.metric == "max_allocs_per_op"));
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
    fn comparison_uses_summary_regression_budget_threshold() {
        let mut baseline = summarize_benchmark(
            &micro_spec("hot_path"),
            &(0..10)
                .map(|i| micro_sample("hot_path", i, 100))
                .collect::<Vec<_>>(),
        );
        let mut current_spec = micro_spec("hot_path");
        current_spec.budgets.max_regression_pct = Some(1.0);
        let mut current = summarize_benchmark(
            &current_spec,
            &(0..10)
                .map(|i| micro_sample("hot_path", i, 102))
                .collect::<Vec<_>>(),
        );
        baseline
            .stats
            .as_mut()
            .expect("stats")
            .confidence_interval_95 = ConfidenceInterval {
            lower: 99.0,
            upper: 100.0,
        };
        current
            .stats
            .as_mut()
            .expect("stats")
            .confidence_interval_95 = ConfidenceInterval {
            lower: 102.0,
            upper: 103.0,
        };

        let comparison = compare_summaries(&[current], &[baseline], 0.05)
            .into_iter()
            .next()
            .expect("comparison");

        assert!((comparison.threshold - 0.01).abs() < f64::EPSILON);
        assert_eq!(comparison.classification, ComparisonClass::Regression);
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
    fn json_contains_schema_version() {
        let profile_config = ProfileConfig::default();
        let run = StressRun {
            schema_version: SCHEMA_VERSION.to_string(),
            tool_version: "0.3.0".to_string(),
            suite: "suite".to_string(),
            run_profile: profile_config.profile,
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
    fn wrong_schema_version_is_not_accepted() {
        let profile_config = ProfileConfig::default();
        let run = StressRun {
            schema_version: "cntryl-stress.v999".to_string(),
            tool_version: "0.3.0".to_string(),
            suite: "suite".to_string(),
            run_profile: profile_config.profile,
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
