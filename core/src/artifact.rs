//! Raw-sample result types and current artifact helpers.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::Path;
use std::time::Duration;

/// Authoritative JSON schema version for current cntryl-stress artifacts.
pub const SCHEMA_VERSION: &str = "cntryl-stress.v2";

/// Highest defined benchmark tier.
pub const MAX_TIER: u32 = 6;

/// Benchmark run profile. The default profile is a moderate day-to-day run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunProfile {
    /// Moderate day-to-day run with useful per-tier signal.
    #[default]
    Default,
    /// Fast correctness-focused diagnostic runs.
    Smoke,
    /// Deep exploratory runs. Correctness still fails, quality is reported.
    Lab,
    /// Trustworthy runs with quality and regression gates.
    Release,
}

impl fmt::Display for RunProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => f.write_str("default"),
            Self::Smoke => f.write_str("smoke"),
            Self::Lab => f.write_str("lab"),
            Self::Release => f.write_str("release"),
        }
    }
}

impl std::str::FromStr for RunProfile {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "default" => Ok(Self::Default),
            "smoke" => Ok(Self::Smoke),
            "lab" => Ok(Self::Lab),
            "release" => Ok(Self::Release),
            other => Err(format!("unknown run profile '{other}'")),
        }
    }
}

/// Static mode family derived from `#[stress(tier = N)]`.
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

impl BenchmarkModeKind {
    /// Return the mode family implied by a tier.
    #[must_use]
    pub const fn for_tier(tier: u32) -> Option<Self> {
        match tier {
            1 => Some(Self::Micro),
            2 => Some(Self::FixedOperations),
            3..=MAX_TIER => Some(Self::FixedDuration),
            _ => None,
        }
    }

    /// Validate that this mode family matches the tier-derived mode.
    ///
    /// # Errors
    ///
    /// Returns an error when the tier is undefined or the mode does not match
    /// the tier-derived mode.
    pub fn validate_for_tier(self, tier: u32) -> Result<(), String> {
        let Some(expected) = Self::for_tier(tier) else {
            return Err(format!("tier must be between 1 and {MAX_TIER}"));
        };
        if self == expected {
            Ok(())
        } else {
            Err(tier_mode_mismatch_message(tier, expected, self))
        }
    }
}

fn tier_mode_mismatch_message(
    tier: u32,
    expected: BenchmarkModeKind,
    actual: BenchmarkModeKind,
) -> String {
    format!(
        "Tier {tier} uses {expected}; remove mode or use {} for {actual}.",
        tier_hint_for_mode(actual)
    )
}

const fn tier_hint_for_mode(mode: BenchmarkModeKind) -> &'static str {
    match mode {
        BenchmarkModeKind::Micro => "tier = 1",
        BenchmarkModeKind::FixedOperations => "tier = 2",
        BenchmarkModeKind::FixedDuration => "tier = 3",
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

/// Benchmark authoring intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementIntent {
    /// General measured work.
    #[default]
    General,
    /// Batched logical operations.
    Batch,
    /// Async work awaited by the benchmark.
    Async,
    /// Threaded or parallel work.
    Threaded,
    /// Pipeline-style work.
    Pipeline,
    /// I/O style work.
    Io,
    /// Externally timed work.
    External,
}

impl fmt::Display for MeasurementIntent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::General => f.write_str("general"),
            Self::Batch => f.write_str("batch"),
            Self::Async => f.write_str("async"),
            Self::Threaded => f.write_str("threaded"),
            Self::Pipeline => f.write_str("pipeline"),
            Self::Io => f.write_str("io"),
            Self::External => f.write_str("external"),
        }
    }
}

/// Diagnostic severity for benchmark summaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    /// Informational guidance.
    Info,
    /// Actionable warning that does not necessarily fail the row.
    Warning,
    /// Error that makes the row untrustworthy or fails an explicit gate.
    Error,
}

impl DiagnosticSeverity {
    /// Return the severity ordering used by strict diagnostic gates.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Info => 0,
            Self::Warning => 1,
            Self::Error => 2,
        }
    }

    /// Return whether this severity is at or above `threshold`.
    #[must_use]
    pub const fn at_least(self, threshold: Self) -> bool {
        self.rank() >= threshold.rank()
    }
}

impl fmt::Display for DiagnosticSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Info => f.write_str("info"),
            Self::Warning => f.write_str("warning"),
            Self::Error => f.write_str("error"),
        }
    }
}

impl std::str::FromStr for DiagnosticSeverity {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "info" => Ok(Self::Info),
            "warning" => Ok(Self::Warning),
            "error" => Ok(Self::Error),
            other => Err(format!("unknown diagnostic severity '{other}'")),
        }
    }
}

/// Whether a benchmark row should participate in performance gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TrustClass {
    /// The row is semantically valid and can drive quality/regression gates.
    #[default]
    Gate,
    /// The row is intentionally non-gating but still useful for diagnostics.
    Diagnostic,
    /// The row still needs follow-up validation before it should be treated as stable.
    Experimental,
    /// The row has known-bad semantics or blocking trust issues.
    Invalid,
}

impl TrustClass {
    /// Relative trust ordering used for override downgrades.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Invalid => 0,
            Self::Experimental => 1,
            Self::Diagnostic => 2,
            Self::Gate => 3,
        }
    }

    /// Return the less-trusted of two classes.
    #[must_use]
    pub const fn min(self, other: Self) -> Self {
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }
}

impl fmt::Display for TrustClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gate => f.write_str("gate"),
            Self::Diagnostic => f.write_str("diagnostic"),
            Self::Experimental => f.write_str("experimental"),
            Self::Invalid => f.write_str("invalid"),
        }
    }
}

impl std::str::FromStr for TrustClass {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "gate" => Ok(Self::Gate),
            "diagnostic" => Ok(Self::Diagnostic),
            "experimental" => Ok(Self::Experimental),
            "invalid" => Ok(Self::Invalid),
            other => Err(format!("unknown trust class '{other}'")),
        }
    }
}

/// Console benchmark-name presentation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleNameMode {
    /// Compact table names with suffix preservation and parameter hints.
    #[default]
    Compact,
    /// Full names with a dynamically widened benchmark column.
    Full,
}

impl fmt::Display for ConsoleNameMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compact => f.write_str("compact"),
            Self::Full => f.write_str("full"),
        }
    }
}

impl std::str::FromStr for ConsoleNameMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "compact" => Ok(Self::Compact),
            "full" => Ok(Self::Full),
            other => Err(format!("unknown console name mode '{other}'")),
        }
    }
}

/// Structured benchmark diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchmarkDiagnostic {
    /// Stable diagnostic code.
    pub code: String,
    /// Diagnostic severity.
    pub severity: DiagnosticSeverity,
    /// Human-readable reason.
    pub reason: String,
    /// Machine-readable evidence.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub evidence: BTreeMap<String, String>,
    /// Concrete next actions.
    pub suggestions: Vec<String>,
}

/// Query-friendly row in the run-level diagnostic ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticSummary {
    /// Suite that emitted the diagnostic.
    pub suite: String,
    /// Stable benchmark id.
    pub benchmark_id: String,
    /// Display name.
    pub name: String,
    /// Numeric tier.
    pub tier: u32,
    /// Stable diagnostic code.
    pub code: String,
    /// Diagnostic severity.
    pub severity: DiagnosticSeverity,
    /// Human-readable reason.
    pub reason: String,
    /// Machine-readable evidence.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub evidence: BTreeMap<String, String>,
    /// Concrete next actions.
    pub suggestions: Vec<String>,
    /// Summary quality for the row that emitted the diagnostic.
    pub quality: QualityClass,
    /// Trust classification for the row that emitted the diagnostic.
    #[serde(default)]
    pub trust_class: TrustClass,
    /// Structured parameters for the row that emitted the diagnostic.
    pub parameters: BTreeMap<String, String>,
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
    #[serde(with = "nullable_f64_serde")]
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
    /// Optional strict diagnostic gate threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deny_diagnostics: Option<DiagnosticSeverity>,
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
    /// Human console benchmark-name mode.
    #[serde(default)]
    pub console_names: ConsoleNameMode,
    /// Whether human runs emit stderr progress.
    #[serde(default = "default_progress")]
    pub progress: bool,
}

impl Default for ProfileConfig {
    fn default() -> Self {
        Self {
            profile: RunProfile::Default,
            measured_samples: 5,
            warmup_samples: 1,
            cooldown_samples: 0,
            min_quality: QualityClass::Noisy,
            fail_on_quality: false,
            fail_on_regression: false,
            deny_diagnostics: None,
            regression_threshold: 0.05,
            sample_duration: Duration::from_millis(500),
            operations_per_sample: 1,
            micro_sample_duration: Duration::from_millis(25),
            report_depth: "default".to_string(),
            console_names: ConsoleNameMode::Compact,
            progress: true,
        }
    }
}

const fn default_progress() -> bool {
    true
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
    /// Authoring intent for this measurement.
    #[serde(default)]
    pub intent: MeasurementIntent,
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
    /// Authoring intent for this sample.
    #[serde(default)]
    pub intent: MeasurementIntent,
    /// Zero-based sample number within the benchmark.
    pub sample_number: usize,
    /// Sample phase.
    pub phase: SamplePhase,
    /// Measured workload duration in nanoseconds.
    pub elapsed_ns: u128,
    /// Wall-clock time spent executing the benchmark function for this sample.
    ///
    /// This includes framework work done inside the benchmark method, such as
    /// Tier 1 calibration and overhead measurement.
    #[serde(default)]
    pub wall_clock_ns: u128,
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
    /// Authoring intent for this measurement.
    #[serde(default)]
    pub intent: MeasurementIntent,
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
    /// Wall-clock statistics from measured samples only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_clock: Option<SummaryStats>,
    /// Total wall-clock time across warmup, measured, and cooldown samples.
    #[serde(default)]
    pub total_wall_clock_ns: u128,
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
    /// Whether the row should participate in performance gates.
    #[serde(default)]
    pub trust_class: TrustClass,
    /// Budget gates copied from the spec.
    #[serde(default)]
    pub budgets: BenchmarkBudgets,
    /// Budget results derived from measured samples.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub budget_results: Vec<BudgetResult>,
    /// Structured diagnostics and next actions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BenchmarkDiagnostic>,
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

    /// Whether this row participates in quality and regression gates.
    #[must_use]
    pub const fn is_gate(&self) -> bool {
        matches!(self.trust_class, TrustClass::Gate)
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
    /// Human-readable reason when the comparison is intentionally inconclusive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Complete current run artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StressRun {
    /// Schema version. Always [`SCHEMA_VERSION`] for new artifacts.
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
    /// Query-friendly ledger of all benchmark diagnostics.
    #[serde(default)]
    pub diagnostics_summary: Vec<DiagnosticSummary>,
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
            .filter(|summary| summary.is_gate())
            .all(|summary| quality_rank(summary.quality) >= quality_rank(min_quality))
    }

    /// Meaningful regression rows.
    #[must_use]
    pub fn regressions(&self) -> Vec<&ComparisonResult> {
        let gate_rows = self
            .summaries
            .iter()
            .filter(|summary| summary.is_gate())
            .map(|summary| summary.benchmark_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        self.comparisons
            .iter()
            .filter(|comparison| comparison.classification == ComparisonClass::Regression)
            .filter(|comparison| gate_rows.contains(comparison.benchmark_id.as_str()))
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

    /// Whether every diagnostic is below the configured strict threshold.
    #[must_use]
    pub fn diagnostics_passed(&self, threshold: DiagnosticSeverity) -> bool {
        self.diagnostics_summary
            .iter()
            .all(|diagnostic| !diagnostic.severity.at_least(threshold))
    }
}

pub(crate) fn diagnostic_summary_for_run(
    suite: &str,
    summaries: &[BenchmarkSummary],
) -> Vec<DiagnosticSummary> {
    summaries
        .iter()
        .flat_map(|summary| {
            summary
                .diagnostics
                .iter()
                .map(|diagnostic| DiagnosticSummary {
                    suite: suite.to_string(),
                    benchmark_id: summary.benchmark_id.clone(),
                    name: summary.name.clone(),
                    tier: summary.tier,
                    code: diagnostic.code.clone(),
                    severity: diagnostic.severity,
                    reason: diagnostic.reason.clone(),
                    evidence: diagnostic.evidence.clone(),
                    suggestions: diagnostic.suggestions.clone(),
                    quality: summary.quality,
                    trust_class: summary.trust_class,
                    parameters: summary.parameters.clone(),
                })
        })
        .collect()
}

/// Summarize one benchmark from raw samples.
#[must_use]
pub(crate) fn summarize_benchmark(spec: &BenchmarkSpec, samples: &[Sample]) -> BenchmarkSummary {
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
    let wall_clock = SummaryStats::from_values(&wall_clock_values(&measured));
    let completed_operations = completed_operation_stats(&measured);
    let total_wall_clock_ns = samples
        .iter()
        .filter(|sample| sample.benchmark_id == spec.id)
        .fold(0_u128, |total, sample| {
            total.saturating_add(sample.wall_clock_ns)
        });
    let budget_results = evaluate_budgets(
        spec.budgets,
        stats.as_ref(),
        ns_per_op.as_ref(),
        allocs_per_op.as_ref(),
        bytes_per_op.as_ref(),
    );
    let diagnostics = summary_diagnostics(DiagnosticInputs {
        spec,
        samples: &measured,
        stats: stats.as_ref(),
        wall_clock: wall_clock.as_ref(),
        ns_per_op: ns_per_op.as_ref(),
        overhead_ns_per_op: overhead_ns_per_op.as_ref(),
        allocs_per_op: allocs_per_op.as_ref(),
        bytes_per_op: bytes_per_op.as_ref(),
        budget_results: &budget_results,
        correctness_passed: correctness.passed,
    });
    let quality = classify_quality(
        measured.len(),
        stats.as_ref(),
        correctness.passed,
        &measured,
        &diagnostics,
        &budget_results,
    );
    let trust_class = derive_trust_class(
        spec,
        &diagnostics,
        quality,
        primary_metric,
        measured.len(),
        wall_clock.as_ref(),
        completed_operations.as_ref(),
    );

    BenchmarkSummary {
        benchmark_id: spec.id.clone(),
        name: spec.name.clone(),
        tier: spec.tier,
        intent: spec.intent,
        primary_metric,
        measured_samples: measured.len(),
        warmup_samples,
        cooldown_samples,
        stats,
        wall_clock,
        total_wall_clock_ns,
        ns_per_op,
        gross_ns_per_op,
        overhead_ns_per_op,
        allocs_per_op,
        bytes_per_op,
        quality,
        trust_class,
        budgets: spec.budgets,
        budget_results,
        diagnostics,
        correctness,
        parameters: merged_parameters(spec, &measured),
        metadata: spec.metadata.clone(),
    }
}

/// Compare current summaries to baseline summaries.
#[must_use]
pub(crate) fn compare_summaries(
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

/// Add regression diagnostics to summaries after baseline comparison.
pub(crate) fn attach_regression_diagnostics(
    summaries: &mut [BenchmarkSummary],
    comparisons: &[ComparisonResult],
) {
    let regressions = comparisons
        .iter()
        .filter(|comparison| comparison.classification == ComparisonClass::Regression)
        .collect::<Vec<_>>();
    for summary in &mut *summaries {
        let Some(comparison) = regressions
            .iter()
            .find(|comparison| comparison.benchmark_id == summary.benchmark_id)
        else {
            continue;
        };
        summary.diagnostics.push(BenchmarkDiagnostic {
            code: "regression".to_string(),
            severity: DiagnosticSeverity::Error,
            reason: "The row regressed against the selected baseline.".to_string(),
            evidence: BTreeMap::from([
                (
                    "change_percent".to_string(),
                    comparison
                        .change_percent
                        .map_or_else(|| "unknown".to_string(), |value| format!("{value:.4}")),
                ),
                ("threshold".to_string(), comparison.threshold.to_string()),
            ]),
            suggestions: vec![
                "Compare the same benchmark row before updating baselines.".to_string()
            ],
        });
    }

    for comparison in comparisons.iter().filter(|comparison| {
        comparison.classification == ComparisonClass::Inconclusive && comparison.reason.is_some()
    }) {
        let Some(summary) = summaries
            .iter_mut()
            .find(|summary| summary.benchmark_id == comparison.benchmark_id)
        else {
            continue;
        };
        summary.diagnostics.push(BenchmarkDiagnostic {
            code: "baseline_semantics_changed".to_string(),
            severity: DiagnosticSeverity::Warning,
            reason: comparison
                .reason
                .clone()
                .unwrap_or_else(|| "The row semantics changed relative to baseline.".to_string()),
            evidence: BTreeMap::new(),
            suggestions: vec![
                "Refresh the baseline after confirming the semantic change is intentional."
                    .to_string(),
            ],
        });
    }
}

pub(crate) fn attach_measurement_mode_mismatch_diagnostics(summaries: &mut [BenchmarkSummary]) {
    let mut families = BTreeMap::<String, BTreeMap<String, Vec<usize>>>::new();
    for (index, summary) in summaries.iter().enumerate() {
        if summary.primary_metric != PrimaryMetric::Throughput {
            continue;
        }
        let family = measurement_family(summary);
        let mode = measurement_mode(summary).to_string();
        families
            .entry(family)
            .or_default()
            .entry(mode)
            .or_default()
            .push(index);
    }

    for (family, modes) in families {
        if modes.len() < 2 {
            continue;
        }
        let mode_names = modes.keys().cloned().collect::<Vec<_>>().join(", ");
        for indices in modes.values() {
            for index in indices {
                let summary = &mut summaries[*index];
                if summary
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == "measurement_mode_mismatch")
                {
                    continue;
                }
                summary.diagnostics.push(BenchmarkDiagnostic {
                    code: "measurement_mode_mismatch".to_string(),
                    severity: DiagnosticSeverity::Warning,
                    reason: "Sibling rows in the same workload family mix throughput measurement semantics."
                        .to_string(),
                    evidence: BTreeMap::from([
                        ("family".to_string(), family.clone()),
                        (
                            "measurement_modes".to_string(),
                            mode_names.clone(),
                        ),
                    ]),
                    suggestions: vec![
                        "Use one measurement_mode per workload family, or split fixed-op probes into explicit diagnostic rows."
                            .to_string(),
                    ],
                });
                summary.trust_class = derive_trust_class_from_summary(summary);
            }
        }
    }
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
            reason: None,
        };
    };
    let baseline_value = baseline.primary_value();
    let current_value = current.primary_value();
    let change_percent = baseline_value
        .zip(current_value)
        .map(|(base, current)| ((current / base) - 1.0) * 100.0);
    if let Some(reason) = ns_per_op_basis_change_reason(current, baseline) {
        return ComparisonResult {
            benchmark_id: current.benchmark_id.clone(),
            current_quality: current.quality,
            baseline_quality: Some(baseline.quality),
            primary_metric: current.primary_metric,
            baseline_value,
            current_value,
            change_percent,
            threshold,
            confidence_intervals_overlap: None,
            classification: ComparisonClass::Inconclusive,
            reason: Some(reason),
        };
    }
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
        reason: None,
    }
}

fn ns_per_op_basis_change_reason(
    current: &BenchmarkSummary,
    baseline: &BenchmarkSummary,
) -> Option<String> {
    let current_basis = current.metadata.get("ns_per_op_basis");
    let baseline_basis = baseline.metadata.get("ns_per_op_basis");
    (current_basis != baseline_basis).then(|| {
        format!(
            "ns_per_op_basis changed from {} to {}; refresh the baseline after confirming the row semantics.",
            baseline_basis.map_or("<unset>", String::as_str),
            current_basis.map_or("<unset>", String::as_str)
        )
    })
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
fn wall_clock_values(samples: &[&Sample]) -> Vec<f64> {
    samples
        .iter()
        .map(|sample| sample.wall_clock_ns)
        .filter(|wall_clock_ns| *wall_clock_ns != 0)
        .map(|wall_clock_ns| wall_clock_ns as f64)
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

#[derive(Clone, Copy)]
struct DiagnosticInputs<'a> {
    spec: &'a BenchmarkSpec,
    samples: &'a [&'a Sample],
    stats: Option<&'a SummaryStats>,
    wall_clock: Option<&'a SummaryStats>,
    ns_per_op: Option<&'a SummaryStats>,
    overhead_ns_per_op: Option<&'a SummaryStats>,
    allocs_per_op: Option<&'a SummaryStats>,
    bytes_per_op: Option<&'a SummaryStats>,
    budget_results: &'a [BudgetResult],
    correctness_passed: bool,
}

#[allow(clippy::too_many_lines)]
fn summary_diagnostics(input: DiagnosticInputs<'_>) -> Vec<BenchmarkDiagnostic> {
    let mut diagnostics = Vec::new();
    let DiagnosticInputs {
        spec,
        samples,
        stats,
        wall_clock,
        ns_per_op,
        overhead_ns_per_op,
        allocs_per_op,
        bytes_per_op,
        budget_results,
        correctness_passed,
    } = input;
    if samples.iter().any(|sample| !sample.has_valid_timing()) {
        diagnostics.push(diagnostic(
            "invalid_timing",
            DiagnosticSeverity::Error,
            "At least one measured sample recorded zero or invalid timing.",
            [("invalid_samples", invalid_timing_count(samples).to_string())],
            ["Measure exactly one non-empty workload for this row."],
        ));
    }
    if samples
        .iter()
        .any(|sample| sample.operations_completed == 0)
    {
        diagnostics.push(diagnostic(
            "zero_completed_ops",
            DiagnosticSeverity::Error,
            "At least one measured sample completed zero logical operations.",
            [(
                "zero_completed_samples",
                zero_completed_count(samples).to_string(),
            )],
            ["Record completed logical work with measure_batch, operations, or record_external."],
        ));
    }
    if !correctness_passed {
        diagnostics.push(diagnostic(
            "correctness_failure",
            DiagnosticSeverity::Error,
            "Correctness counters did not pass for this benchmark row.",
            [],
            ["Inspect correctness counters before using this performance number."],
        ));
    }
    if samples.len() < 5 {
        diagnostics.push(diagnostic(
            "too_few_samples",
            if samples.len() < 2 {
                DiagnosticSeverity::Error
            } else {
                DiagnosticSeverity::Warning
            },
            "The row has too few measured samples to make a stable decision.",
            [("measured_samples", samples.len().to_string())],
            ["Collect at least five measured samples, or use the release profile for gate-quality rows."],
        ));
    }
    if stats.is_some_and(|stats| stats.relative_std_dev > 0.10) {
        diagnostics.push(diagnostic_with_evidence(
            "high_variance",
            DiagnosticSeverity::Warning,
            "Measured samples varied enough that this row needs attention.",
            high_variance_evidence(spec, samples, stats, wall_clock),
            high_variance_suggestions(spec, samples, stats, wall_clock),
        ));
    }
    if too_fast_sample(samples, ns_per_op, spec.mode.kind()) {
        diagnostics.push(diagnostic(
            "too_fast",
            DiagnosticSeverity::Warning,
            "The measured work is too small for a useful timing sample.",
            [(
                "min_elapsed_ns",
                samples
                    .iter()
                    .map(|sample| sample.elapsed_ns)
                    .min()
                    .unwrap_or_default()
                    .to_string(),
            )],
            ["Batch more logical work per measurement or use Tier 1 for hot-path micro timing."],
        ));
    }
    if has_overhead_dominant_sample(samples, overhead_ns_per_op) {
        diagnostics.push(diagnostic(
            "setup_dominates_measurement",
            DiagnosticSeverity::Error,
            "Timing overhead or setup dominates the measured work.",
            overhead_evidence(overhead_ns_per_op),
            ["Increase measured work per iteration and keep setup outside the measurement closure."],
        ));
    }
    if spec.budgets.requires_allocation_tracking()
        && samples
            .iter()
            .any(|sample| sample.allocs_per_op.is_none() || sample.bytes_per_op.is_none())
    {
        diagnostics.push(diagnostic(
            "budget_failure",
            DiagnosticSeverity::Error,
            "An allocation budget was configured but allocation tracking is unavailable.",
            [("budget", "allocation".to_string())],
            ["Install cntryl_stress::stress_allocator!() in the benchmark crate."],
        ));
    }
    if budget_results.iter().any(|result| !result.passed) {
        diagnostics.push(diagnostic_with_evidence(
            "budget_failure",
            DiagnosticSeverity::Error,
            "One or more explicit benchmark budgets failed.",
            budget_failure_evidence(budget_results),
            vec!["Inspect the failing budget, then either reduce measured cost or intentionally update the budget.".to_string()],
        ));
    }
    if high_allocation_stats(allocs_per_op, bytes_per_op) {
        diagnostics.push(diagnostic_with_evidence(
            "high_allocations",
            high_allocation_severity(spec),
            "The benchmark allocated during measured work.",
            allocation_evidence(spec, allocs_per_op, bytes_per_op),
            high_allocation_suggestions(spec),
        ));
    }
    if should_flag_likely_optimized_away(spec, ns_per_op) {
        diagnostics.push(diagnostic(
            "likely_optimized_away",
            DiagnosticSeverity::Warning,
            "Tier 1 timing is below 5 ns/op without explicit validation.",
            [(
                "mean_ns_per_op",
                ns_per_op
                    .map(|stats| stats.mean.to_string())
                    .unwrap_or_default(),
            )],
            ["Vary inputs, accumulate observable outputs, and use validated_micro=true only after anti-DCE is explicit."],
        ));
    } else if should_flag_tiny_micro_timing(spec, ns_per_op) {
        diagnostics.push(diagnostic(
            "tiny_micro_timing",
            DiagnosticSeverity::Warning,
            "Tier 1 timing is below 15 ns/op without explicit validation.",
            [(
                "mean_ns_per_op",
                ns_per_op
                    .map(|stats| stats.mean.to_string())
                    .unwrap_or_default(),
            )],
            ["Batch more logical work per sample, or keep the row as a validated diagnostic with an explicit trust_class downgrade."],
        ));
    }
    if batch_unit_ambiguous(spec) {
        diagnostics.push(diagnostic_with_evidence(
            "batch_unit_ambiguous",
            DiagnosticSeverity::Warning,
            "Batched work is missing explicit logical-unit normalization metadata.",
            batch_unit_ambiguity_evidence(spec),
            vec![
                "Add logical_unit and any *_per_logical_operation parameter so the report can state the measured question directly."
                    .to_string(),
            ],
        ));
    }
    if fixed_ops_throughput(spec, samples) {
        diagnostics.push(diagnostic_with_evidence(
            "fixed_ops_throughput",
            DiagnosticSeverity::Warning,
            "A throughput row is using fixed-op timing semantics instead of a fixed-duration window.",
            BTreeMap::from([
                (
                    "measurement_mode".to_string(),
                    measurement_mode_for_spec(spec).to_string(),
                ),
                ("tier".to_string(), spec.tier.to_string()),
            ]),
            vec![
                "Use duration-based throughput for main rows, or split the fixed-op probe into an explicit diagnostic row."
                    .to_string(),
            ],
        ));
    }
    if flat_or_capped_throughput(spec, stats, wall_clock, samples) {
        diagnostics.push(diagnostic_with_evidence(
            "flat_or_capped_throughput",
            DiagnosticSeverity::Warning,
            "Throughput is near-perfectly flat while completed logical work is effectively fixed across samples.",
            flat_or_capped_throughput_evidence(stats, wall_clock, samples),
            vec![
                "Confirm whether this row is an intentional capped-capacity probe; otherwise inspect local bottlenecks or move it out of the gate set."
                    .to_string(),
            ],
        ));
    }
    if (3..=MAX_TIER).contains(&spec.tier)
        && !samples.is_empty()
        && samples
            .iter()
            .all(|sample| sample.operations_completed == 1)
    {
        diagnostics.push(diagnostic(
            "single_op_throughput",
            DiagnosticSeverity::Warning,
            "A throughput-tier row completed only one operation per sample.",
            [("tier", spec.tier.to_string())],
            ["Use measure_batch or record_external for throughput work, or move a single-operation row to Tier 2."],
        ));
    }
    if spec.intent == MeasurementIntent::Async
        && samples
            .iter()
            .all(|sample| sample.wall_clock_ns <= sample.elapsed_ns.saturating_add(100))
    {
        diagnostics.push(diagnostic(
            "async_misuse",
            DiagnosticSeverity::Info,
            "Async measurement did not show observable scheduling or await overhead.",
            [("intent", spec.intent.to_string())],
            ["Make sure the measured future awaits the real async operation instead of spawning detached work."],
        ));
    }
    diagnostics
}

fn diagnostic<const E: usize, const S: usize>(
    code: &'static str,
    severity: DiagnosticSeverity,
    reason: &'static str,
    evidence: [(&'static str, String); E],
    suggestions: [&'static str; S],
) -> BenchmarkDiagnostic {
    BenchmarkDiagnostic {
        code: code.to_string(),
        severity,
        reason: reason.to_string(),
        evidence: evidence
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
        suggestions: suggestions.into_iter().map(str::to_string).collect(),
    }
}

fn diagnostic_with_evidence(
    code: &'static str,
    severity: DiagnosticSeverity,
    reason: &'static str,
    evidence: BTreeMap<String, String>,
    suggestions: Vec<String>,
) -> BenchmarkDiagnostic {
    BenchmarkDiagnostic {
        code: code.to_string(),
        severity,
        reason: reason.to_string(),
        evidence,
        suggestions,
    }
}

fn invalid_timing_count(samples: &[&Sample]) -> usize {
    samples
        .iter()
        .filter(|sample| !sample.has_valid_timing())
        .count()
}

fn zero_completed_count(samples: &[&Sample]) -> usize {
    samples
        .iter()
        .filter(|sample| sample.operations_completed == 0)
        .count()
}

#[allow(clippy::cast_precision_loss)]
fn high_variance_evidence(
    spec: &BenchmarkSpec,
    samples: &[&Sample],
    stats: Option<&SummaryStats>,
    wall_clock: Option<&SummaryStats>,
) -> BTreeMap<String, String> {
    let mut evidence = BTreeMap::new();
    if let Some(stats) = stats {
        evidence.insert(
            "relative_std_dev".to_string(),
            stats.relative_std_dev.to_string(),
        );
        evidence.insert("max_to_median".to_string(), ratio(stats.max, stats.median));
        evidence.insert("min_to_median".to_string(), ratio(stats.min, stats.median));
    }
    if let Some(elapsed) = elapsed_stats(samples) {
        evidence.insert(
            "median_elapsed_ns".to_string(),
            elapsed.median.round().to_string(),
        );
    }
    if let Some(wall_clock) = wall_clock {
        evidence.insert(
            "wall_clock_rsd".to_string(),
            wall_clock.relative_std_dev.to_string(),
        );
    }
    if let Some(operations) = completed_operation_stats(samples) {
        evidence.insert(
            "completed_operations_rsd".to_string(),
            operations.relative_std_dev.to_string(),
        );
    }
    if scheduler_sensitive_reason(spec, samples).is_some() {
        evidence.insert("scheduler_sensitive".to_string(), "true".to_string());
    }
    evidence
}

fn high_variance_suggestions(
    spec: &BenchmarkSpec,
    samples: &[&Sample],
    stats: Option<&SummaryStats>,
    wall_clock: Option<&SummaryStats>,
) -> Vec<String> {
    let mut suggestions = Vec::new();
    if elapsed_stats(samples).is_some_and(|stats| stats.median < 10_000_000.0) {
        suggestions.push(
            "Increase the measured window with STRESS_SAMPLE_DURATION_MS or batch more logical work per sample."
                .to_string(),
        );
    }
    if stats.is_some_and(has_single_outlier_shape) {
        suggestions.push(
            "Inspect one-off setup, cache, I/O, or background interference before updating a baseline."
                .to_string(),
        );
    }
    if stable_wall_clock_with_variable_work(samples, wall_clock) {
        suggestions.push(
            "Wall-clock windows are stable but completed operations vary; inspect contention, work shedding, or per-sample operation accounting."
                .to_string(),
        );
    }
    if scheduler_sensitive_reason(spec, samples).is_some() {
        suggestions.push(
            "Isolate CPU scheduling or pin and limit worker concurrency before using this row as a regression gate."
                .to_string(),
        );
    }
    if suggestions.is_empty() {
        suggestions.push(
            "Use deterministic fixtures and move setup outside the measured work.".to_string(),
        );
    }
    suggestions
}

fn has_single_outlier_shape(stats: &SummaryStats) -> bool {
    stats.median > 0.0 && (stats.max / stats.median >= 3.0 || stats.min / stats.median <= 0.33)
}

fn stable_wall_clock_with_variable_work(
    samples: &[&Sample],
    wall_clock: Option<&SummaryStats>,
) -> bool {
    wall_clock.is_some_and(|stats| stats.relative_std_dev <= 0.05)
        && completed_operation_stats(samples).is_some_and(|stats| stats.relative_std_dev > 0.10)
}

fn scheduler_sensitive_reason(spec: &BenchmarkSpec, samples: &[&Sample]) -> Option<String> {
    if matches!(
        spec.intent,
        MeasurementIntent::Async | MeasurementIntent::Threaded
    ) {
        return Some(spec.intent.to_string());
    }
    spec.parameters
        .keys()
        .chain(spec.metadata.keys())
        .chain(samples.iter().flat_map(|sample| sample.parameters.keys()))
        .find(|key| {
            let key = key.as_str();
            key.contains("thread")
                || key.contains("worker")
                || key.contains("concurrency")
                || key.contains("client")
                || key.contains("task")
                || key.contains("parallel")
        })
        .cloned()
}

#[allow(clippy::cast_precision_loss)]
fn completed_operation_stats(samples: &[&Sample]) -> Option<SummaryStats> {
    SummaryStats::from_values(
        &samples
            .iter()
            .map(|sample| sample.operations_completed as f64)
            .collect::<Vec<_>>(),
    )
}

#[allow(clippy::cast_precision_loss)]
fn elapsed_stats(samples: &[&Sample]) -> Option<SummaryStats> {
    SummaryStats::from_values(
        &samples
            .iter()
            .map(|sample| sample.elapsed_ns as f64)
            .collect::<Vec<_>>(),
    )
}

fn ratio(value: f64, baseline: f64) -> String {
    if baseline == 0.0 {
        "unknown".to_string()
    } else {
        format!("{:.4}", value / baseline)
    }
}

fn too_fast_sample(
    samples: &[&Sample],
    ns_per_op: Option<&SummaryStats>,
    mode: BenchmarkModeKind,
) -> bool {
    mode != BenchmarkModeKind::Micro
        && samples
            .iter()
            .any(|sample| sample.elapsed_ns < 1_000 && sample.operations_completed <= 1)
        || mode == BenchmarkModeKind::Micro && ns_per_op.is_some_and(|stats| stats.mean < 0.5)
}

fn overhead_evidence(overhead_ns_per_op: Option<&SummaryStats>) -> [(&'static str, String); 1] {
    [(
        "overhead_ns_per_op",
        overhead_ns_per_op.map_or_else(|| "unknown".to_string(), |stats| stats.mean.to_string()),
    )]
}

fn budget_failure_evidence(budget_results: &[BudgetResult]) -> BTreeMap<String, String> {
    budget_results
        .iter()
        .filter(|result| !result.passed)
        .map(|result| {
            (
                result.metric.clone(),
                result
                    .actual
                    .map_or_else(|| "unavailable".to_string(), |actual| actual.to_string()),
            )
        })
        .collect()
}

fn high_allocation_stats(
    allocs_per_op: Option<&SummaryStats>,
    bytes_per_op: Option<&SummaryStats>,
) -> bool {
    allocs_per_op.is_some_and(|stats| stats.mean > 0.0)
        || bytes_per_op.is_some_and(|stats| stats.mean > 0.0)
}

fn high_allocation_severity(spec: &BenchmarkSpec) -> DiagnosticSeverity {
    if is_allocation_context(spec) {
        DiagnosticSeverity::Info
    } else {
        DiagnosticSeverity::Warning
    }
}

fn high_allocation_suggestions(spec: &BenchmarkSpec) -> Vec<String> {
    if is_allocation_context(spec) {
        vec![
            "Treat this as allocation context unless an explicit allocation budget fails."
                .to_string(),
        ]
    } else {
        vec![
            "Move reusable allocations into setup or make the allocation budget explicit."
                .to_string(),
        ]
    }
}

fn is_allocation_context(spec: &BenchmarkSpec) -> bool {
    spec.metadata
        .get("row_class")
        .is_some_and(|value| matches!(value.as_str(), "construction" | "parsing" | "allocation"))
}

fn allocation_evidence(
    spec: &BenchmarkSpec,
    allocs_per_op: Option<&SummaryStats>,
    bytes_per_op: Option<&SummaryStats>,
) -> BTreeMap<String, String> {
    let mut evidence = BTreeMap::new();
    if let Some(stats) = allocs_per_op {
        evidence.insert("allocs_per_op".to_string(), stats.mean.to_string());
    }
    if let Some(stats) = bytes_per_op {
        evidence.insert("bytes_per_op".to_string(), stats.mean.to_string());
    }
    if let Some(row_class) = spec.metadata.get("row_class") {
        evidence.insert("row_class".to_string(), row_class.clone());
    }
    evidence
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

fn should_flag_likely_optimized_away(
    spec: &BenchmarkSpec,
    ns_per_op: Option<&SummaryStats>,
) -> bool {
    spec.mode.kind() == BenchmarkModeKind::Micro
        && !micro_is_validated(spec)
        && ns_per_op.is_some_and(|stats| stats.mean < 5.0)
}

fn should_flag_tiny_micro_timing(spec: &BenchmarkSpec, ns_per_op: Option<&SummaryStats>) -> bool {
    spec.mode.kind() == BenchmarkModeKind::Micro
        && !micro_is_validated(spec)
        && ns_per_op.is_some_and(|stats| (5.0..15.0).contains(&stats.mean))
}

fn batch_unit_ambiguous(spec: &BenchmarkSpec) -> bool {
    if spec.intent != MeasurementIntent::Batch {
        return false;
    }
    let Some(logical_unit) = spec.parameters.get("logical_unit") else {
        return true;
    };
    logical_unit.contains("batch") && batch_normalization_basis(spec).is_none()
}

fn batch_unit_ambiguity_evidence(spec: &BenchmarkSpec) -> BTreeMap<String, String> {
    let mut evidence = BTreeMap::new();
    evidence.insert("intent".to_string(), spec.intent.to_string());
    evidence.insert(
        "logical_unit".to_string(),
        spec.parameters
            .get("logical_unit")
            .cloned()
            .unwrap_or_else(|| "<missing>".to_string()),
    );
    evidence.insert(
        "measurement_mode".to_string(),
        measurement_mode_for_spec(spec).to_string(),
    );
    evidence
}

fn fixed_ops_throughput(spec: &BenchmarkSpec, samples: &[&Sample]) -> bool {
    spec.tier >= 3
        && !samples.is_empty()
        && infer_primary_metric(spec, samples) == PrimaryMetric::Throughput
        && measurement_mode_for_spec(spec) == MeasurementMode::FixedOps
}

fn flat_or_capped_throughput(
    spec: &BenchmarkSpec,
    stats: Option<&SummaryStats>,
    wall_clock: Option<&SummaryStats>,
    samples: &[&Sample],
) -> bool {
    measurement_mode_for_spec(spec) == MeasurementMode::Duration
        && infer_primary_metric(spec, samples) == PrimaryMetric::Throughput
        && stats.is_some_and(|stats| stats.relative_std_dev <= 0.02)
        && wall_clock.is_some_and(|stats| stats.relative_std_dev <= 0.02)
        && completed_operation_stats(samples).is_some_and(|stats| stats.relative_std_dev <= 0.005)
}

fn flat_or_capped_throughput_evidence(
    stats: Option<&SummaryStats>,
    wall_clock: Option<&SummaryStats>,
    samples: &[&Sample],
) -> BTreeMap<String, String> {
    let mut evidence = BTreeMap::new();
    if let Some(stats) = stats {
        evidence.insert(
            "throughput_rsd".to_string(),
            stats.relative_std_dev.to_string(),
        );
    }
    if let Some(wall_clock) = wall_clock {
        evidence.insert(
            "wall_clock_rsd".to_string(),
            wall_clock.relative_std_dev.to_string(),
        );
    }
    if let Some(completed) = completed_operation_stats(samples) {
        evidence.insert(
            "completed_operations_rsd".to_string(),
            completed.relative_std_dev.to_string(),
        );
    }
    evidence
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MeasurementMode {
    Micro,
    FixedOps,
    Duration,
}

impl fmt::Display for MeasurementMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Micro => f.write_str("micro"),
            Self::FixedOps => f.write_str("fixed_ops"),
            Self::Duration => f.write_str("duration"),
        }
    }
}

fn measurement_mode_for_spec(spec: &BenchmarkSpec) -> MeasurementMode {
    spec.parameters
        .get("measurement_mode")
        .and_then(|value| match value.as_str() {
            "micro" => Some(MeasurementMode::Micro),
            "fixed_ops" => Some(MeasurementMode::FixedOps),
            "duration" => Some(MeasurementMode::Duration),
            _ => None,
        })
        .unwrap_or_else(|| measurement_mode_for_kind(spec.mode.kind()))
}

fn measurement_mode(summary: &BenchmarkSummary) -> MeasurementMode {
    summary
        .parameters
        .get("measurement_mode")
        .and_then(|value| match value.as_str() {
            "micro" => Some(MeasurementMode::Micro),
            "fixed_ops" => Some(MeasurementMode::FixedOps),
            "duration" => Some(MeasurementMode::Duration),
            _ => None,
        })
        .unwrap_or_else(|| match summary.tier {
            1 => MeasurementMode::Micro,
            2 => MeasurementMode::FixedOps,
            _ => MeasurementMode::Duration,
        })
}

const fn measurement_mode_for_kind(kind: BenchmarkModeKind) -> MeasurementMode {
    match kind {
        BenchmarkModeKind::Micro => MeasurementMode::Micro,
        BenchmarkModeKind::FixedOperations => MeasurementMode::FixedOps,
        BenchmarkModeKind::FixedDuration => MeasurementMode::Duration,
    }
}

fn batch_normalization_basis(spec: &BenchmarkSpec) -> Option<(String, String)> {
    normalization_basis_from_parameters(&spec.parameters)
}

fn normalization_basis_from_parameters(
    parameters: &BTreeMap<String, String>,
) -> Option<(String, String)> {
    parameters.iter().find_map(|(key, value)| {
        key.strip_suffix("_per_logical_operation")
            .map(|unit| (unit.to_string(), value.clone()))
    })
}

fn measurement_family(summary: &BenchmarkSummary) -> String {
    let tokens = summary.name.split('_').collect::<Vec<_>>();
    if tokens.len() >= 4 {
        let last = tokens[tokens.len() - 1];
        let penultimate = tokens[tokens.len() - 2];
        let antepenultimate = tokens[tokens.len() - 3];
        if matches!(last, "client" | "clients") && penultimate.parse::<usize>().is_ok() {
            return tokens[..tokens.len() - 3].join("_");
        }
        if summary.parameters.contains_key("storage_profile")
            && summary.parameters.contains_key("clients")
            && antepenultimate.parse::<usize>().is_ok()
        {
            return tokens[..tokens.len() - 2].join("_");
        }
    }
    summary.name.clone()
}

fn derive_trust_class(
    spec: &BenchmarkSpec,
    diagnostics: &[BenchmarkDiagnostic],
    quality: QualityClass,
    primary_metric: PrimaryMetric,
    measured_samples: usize,
    wall_clock: Option<&SummaryStats>,
    completed_operations: Option<&SummaryStats>,
) -> TrustClass {
    let derived = derive_trust_class_inner(
        spec,
        diagnostics,
        quality,
        primary_metric,
        measured_samples,
        wall_clock,
        completed_operations,
    );
    apply_trust_class_override(spec.metadata.get("trust_class"), derived)
}

fn derive_trust_class_from_summary(summary: &BenchmarkSummary) -> TrustClass {
    let derived = derive_trust_class_inner_from_summary(summary);
    apply_trust_class_override(summary.metadata.get("trust_class"), derived)
}

fn derive_trust_class_inner(
    spec: &BenchmarkSpec,
    diagnostics: &[BenchmarkDiagnostic],
    quality: QualityClass,
    primary_metric: PrimaryMetric,
    measured_samples: usize,
    wall_clock: Option<&SummaryStats>,
    completed_operations: Option<&SummaryStats>,
) -> TrustClass {
    if quality == QualityClass::Untrustworthy
        || diagnostics
            .iter()
            .any(|diagnostic| blocking_trust_diagnostic(diagnostic.code.as_str()))
    {
        return TrustClass::Invalid;
    }
    if diagnostics.iter().any(|diagnostic| {
        matches!(
            diagnostic.code.as_str(),
            "fixed_ops_throughput" | "measurement_mode_mismatch"
        )
    }) {
        return TrustClass::Invalid;
    }
    let mut trust = TrustClass::Gate;
    if quality == QualityClass::Noisy
        || diagnostics.iter().any(|diagnostic| {
            matches!(
                diagnostic.code.as_str(),
                "tiny_micro_timing" | "flat_or_capped_throughput" | "high_variance"
            )
        })
    {
        trust = trust.min(TrustClass::Diagnostic);
    }
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "batch_unit_ambiguous")
    {
        trust = trust.min(TrustClass::Experimental);
    }
    if primary_metric == PrimaryMetric::Throughput
        && measurement_mode_for_spec(spec) == MeasurementMode::Duration
        && measured_samples >= 5
        && wall_clock.is_some()
        && completed_operations.is_some()
    {
        return trust;
    }
    trust
}

fn derive_trust_class_inner_from_summary(summary: &BenchmarkSummary) -> TrustClass {
    if summary.quality == QualityClass::Untrustworthy
        || summary
            .diagnostics
            .iter()
            .any(|diagnostic| blocking_trust_diagnostic(diagnostic.code.as_str()))
    {
        return TrustClass::Invalid;
    }
    if summary.diagnostics.iter().any(|diagnostic| {
        matches!(
            diagnostic.code.as_str(),
            "fixed_ops_throughput" | "measurement_mode_mismatch"
        )
    }) {
        return TrustClass::Invalid;
    }
    let mut trust = TrustClass::Gate;
    if summary.quality == QualityClass::Noisy
        || summary.diagnostics.iter().any(|diagnostic| {
            matches!(
                diagnostic.code.as_str(),
                "tiny_micro_timing" | "flat_or_capped_throughput" | "high_variance"
            )
        })
    {
        trust = trust.min(TrustClass::Diagnostic);
    }
    if summary
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "batch_unit_ambiguous")
    {
        trust = trust.min(TrustClass::Experimental);
    }
    trust
}

fn blocking_trust_diagnostic(code: &str) -> bool {
    matches!(
        code,
        "invalid_timing"
            | "zero_completed_ops"
            | "correctness_failure"
            | "budget_failure"
            | "setup_dominates_measurement"
            | "likely_optimized_away"
    )
}

fn apply_trust_class_override(override_value: Option<&String>, derived: TrustClass) -> TrustClass {
    override_value
        .and_then(|value| value.parse::<TrustClass>().ok())
        .map_or(derived, |override_class| derived.min(override_class))
}

fn classify_quality(
    measured_samples: usize,
    stats: Option<&SummaryStats>,
    correctness_passed: bool,
    samples: &[&Sample],
    diagnostics: &[BenchmarkDiagnostic],
    budget_results: &[BudgetResult],
) -> QualityClass {
    if !correctness_passed
        || measured_samples < 2
        || budget_results.iter().any(|result| !result.passed)
        || diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
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
        let nanos = u64::deserialize(deserializer)?;
        Ok(Duration::from_nanos(nanos))
    }
}

mod nullable_f64_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub fn serialize<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
        if value.is_finite() {
            value.serialize(serializer)
        } else {
            None::<f64>.serialize(serializer)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        Ok(Option::<f64>::deserialize(deserializer)?.unwrap_or(f64::NAN))
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
            intent: MeasurementIntent::General,
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
            intent: MeasurementIntent::General,
            budgets: BenchmarkBudgets::default(),
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
        }
    }

    fn tier3_spec(id: &str) -> BenchmarkSpec {
        BenchmarkSpec {
            id: id.to_string(),
            name: id.to_string(),
            tier: 3,
            mode: BenchmarkMode::FixedDuration {
                sample_duration: Duration::from_millis(100),
            },
            intent: MeasurementIntent::General,
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
            intent: MeasurementIntent::General,
            sample_number,
            phase,
            elapsed_ns,
            wall_clock_ns: elapsed_ns,
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

    #[allow(clippy::cast_precision_loss)]
    fn completed_sample(
        id: &str,
        sample_number: usize,
        elapsed_ns: u128,
        completed: u64,
    ) -> Sample {
        let mut sample = sample(id, SamplePhase::Measured, sample_number, elapsed_ns);
        sample.operations_attempted = completed;
        sample.operations_completed = completed;
        sample.throughput = if elapsed_ns == 0 {
            0.0
        } else {
            completed as f64 / (elapsed_ns as f64 / 1_000_000_000.0)
        };
        sample.counters.attempted = completed;
        sample.counters.completed = completed;
        sample
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
        assert_eq!(summary.total_wall_clock_ns, 2_000_000_300);
        assert_close(summary.wall_clock.as_ref().expect("wall").min, 100.0);
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
    fn stats_relative_std_dev_round_trips_null_when_non_finite() {
        let stats = SummaryStats::from_values(&[0.0]).expect("stats");

        let json = serde_json::to_string(&stats).expect("serialize");
        let parsed = serde_json::from_str::<SummaryStats>(&json).expect("deserialize");

        assert!(json.contains(r#""relative_std_dev":null"#));
        assert!(parsed.relative_std_dev.is_nan());
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
    fn micro_summary_uses_net_ns_per_op_and_flags_likely_optimized_rows() {
        let spec = micro_spec("hot_path");
        let samples = (0..5)
            .map(|i| micro_sample("hot_path", i, 4))
            .collect::<Vec<_>>();

        let summary = summarize_benchmark(&spec, &samples);

        assert_eq!(summary.primary_metric, PrimaryMetric::NsPerOp);
        assert_close(summary.primary_value().expect("value"), 4.0);
        assert_eq!(summary.quality, QualityClass::Acceptable);
        assert!(summary
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "likely_optimized_away"));
        assert_close(summary.overhead_ns_per_op.expect("overhead").mean, 2.0);
    }

    #[test]
    fn row_class_downgrades_high_allocation_diagnostic_to_info() {
        let mut spec = spec("parser");
        spec.metadata
            .insert("row_class".to_string(), "parsing".to_string());
        let samples = (0..5)
            .map(|i| {
                let mut sample = completed_sample("parser", i, 1_000, 1);
                sample.allocs_per_op = Some(1.0);
                sample.bytes_per_op = Some(64.0);
                sample
            })
            .collect::<Vec<_>>();

        let summary = summarize_benchmark(&spec, &samples);
        let diagnostic = summary
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "high_allocations")
            .expect("allocation diagnostic");

        assert_eq!(diagnostic.severity, DiagnosticSeverity::Info);
        assert_eq!(
            diagnostic.evidence.get("row_class"),
            Some(&"parsing".to_string())
        );
    }

    #[test]
    fn benchmark_mode_kind_is_derived_from_tier() {
        assert_eq!(
            BenchmarkModeKind::for_tier(1),
            Some(BenchmarkModeKind::Micro)
        );
        assert_eq!(
            BenchmarkModeKind::for_tier(2),
            Some(BenchmarkModeKind::FixedOperations)
        );
        for tier in 3..=MAX_TIER {
            assert_eq!(
                BenchmarkModeKind::for_tier(tier),
                Some(BenchmarkModeKind::FixedDuration)
            );
        }
        assert_eq!(BenchmarkModeKind::for_tier(0), None);
        assert_eq!(BenchmarkModeKind::for_tier(MAX_TIER + 1), None);
    }

    #[test]
    fn run_profile_parses_all_named_profiles() {
        assert_eq!("default".parse::<RunProfile>(), Ok(RunProfile::Default));
        assert_eq!("smoke".parse::<RunProfile>(), Ok(RunProfile::Smoke));
        assert_eq!("lab".parse::<RunProfile>(), Ok(RunProfile::Lab));
        assert_eq!("release".parse::<RunProfile>(), Ok(RunProfile::Release));
    }

    #[test]
    fn benchmark_mode_kind_rejects_tier_mismatches_with_guidance() {
        assert_eq!(
            BenchmarkModeKind::FixedOperations.validate_for_tier(3),
            Err(
                "Tier 3 uses fixed_duration; remove mode or use tier = 2 for fixed_operations."
                    .to_string()
            )
        );
        assert_eq!(
            BenchmarkModeKind::FixedDuration.validate_for_tier(1),
            Err("Tier 1 uses micro; remove mode or use tier = 3 for fixed_duration.".to_string())
        );
    }

    #[test]
    fn tier3_to_tier6_single_operation_samples_are_flagged() {
        let spec = tier3_spec("system");
        let samples = (0..5)
            .map(|i| sample("system", SamplePhase::Measured, i, 100 + i as u128))
            .collect::<Vec<_>>();

        let summary = summarize_benchmark(&spec, &samples);

        assert_eq!(summary.quality, QualityClass::Acceptable);
        assert!(summary
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "single_op_throughput"));
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
        assert!(summary
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "budget_failure"));
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
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "budget_failure"));
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
    fn noisy_rows_default_to_diagnostic_trust() {
        let spec = spec("noisy");
        let samples = vec![
            sample("noisy", SamplePhase::Measured, 0, 100),
            sample("noisy", SamplePhase::Measured, 1, 300),
            sample("noisy", SamplePhase::Measured, 2, 1_000),
        ];

        let summary = summarize_benchmark(&spec, &samples);

        assert_eq!(summary.quality, QualityClass::Noisy);
        assert_eq!(summary.trust_class, TrustClass::Diagnostic);
        assert!(summary
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "high_variance"));
    }

    #[test]
    fn trust_class_override_can_downgrade_but_not_promote() {
        let mut downgrade_spec = spec("downgrade");
        downgrade_spec
            .metadata
            .insert("trust_class".to_string(), "diagnostic".to_string());
        let downgraded = summarize_benchmark(
            &downgrade_spec,
            &(0..5)
                .map(|i| sample("downgrade", SamplePhase::Measured, i, 100 + i as u128))
                .collect::<Vec<_>>(),
        );
        assert_eq!(downgraded.trust_class, TrustClass::Diagnostic);

        let mut promote_spec = micro_spec("promote");
        promote_spec
            .metadata
            .insert("trust_class".to_string(), "gate".to_string());
        let promoted = summarize_benchmark(
            &promote_spec,
            &(0..5)
                .map(|i| micro_sample("promote", i, 4))
                .collect::<Vec<_>>(),
        );
        assert_eq!(promoted.trust_class, TrustClass::Invalid);
        assert!(promoted
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "likely_optimized_away"));
    }

    #[test]
    fn run_filters_quality_and_regressions_by_gate_trust() {
        let gate = summarize_benchmark(
            &spec("gate"),
            &(0..5)
                .map(|i| sample("gate", SamplePhase::Measured, i, 100 + i as u128))
                .collect::<Vec<_>>(),
        );
        let mut diagnostic = summarize_benchmark(
            &spec("diagnostic"),
            &[
                sample("diagnostic", SamplePhase::Measured, 0, 100),
                sample("diagnostic", SamplePhase::Measured, 1, 300),
                sample("diagnostic", SamplePhase::Measured, 2, 1_000),
            ],
        );
        diagnostic.trust_class = TrustClass::Diagnostic;

        let mut run = StressRun {
            schema_version: SCHEMA_VERSION.to_string(),
            tool_version: "0.3.0".to_string(),
            suite: "suite".to_string(),
            run_profile: RunProfile::Default,
            environment: EnvironmentInfo::unknown(ProfileConfig::default()),
            benchmark_specs: Vec::new(),
            samples: Vec::new(),
            summaries: vec![gate, diagnostic],
            comparisons: vec![
                ComparisonResult {
                    benchmark_id: "gate".to_string(),
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
                },
                ComparisonResult {
                    benchmark_id: "diagnostic".to_string(),
                    current_quality: QualityClass::Noisy,
                    baseline_quality: Some(QualityClass::Acceptable),
                    primary_metric: PrimaryMetric::Throughput,
                    baseline_value: Some(100.0),
                    current_value: Some(50.0),
                    change_percent: Some(-50.0),
                    threshold: 0.05,
                    confidence_intervals_overlap: Some(false),
                    classification: ComparisonClass::Regression,
                    reason: None,
                },
            ],
            diagnostics_summary: Vec::new(),
            started_at: "123".to_string(),
            total_elapsed_ns: 0,
            metadata: BTreeMap::new(),
        };

        assert!(run.meets_min_quality(QualityClass::Acceptable));
        assert_eq!(run.regressions().len(), 1);
        assert_eq!(run.regressions()[0].benchmark_id, "gate");

        run.summaries[0].quality = QualityClass::Noisy;
        assert!(!run.meets_min_quality(QualityClass::Acceptable));
    }

    #[test]
    fn high_variance_suggestions_include_short_window_and_outlier_shape() {
        let spec = spec("bench");
        let samples = vec![
            completed_sample("bench", 0, 100, 1),
            completed_sample("bench", 1, 100, 1),
            completed_sample("bench", 2, 100, 1),
            completed_sample("bench", 3, 10_000, 1),
            completed_sample("bench", 4, 100, 1),
        ];

        let summary = summarize_benchmark(&spec, &samples);
        let diagnostic = summary
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "high_variance")
            .expect("high variance diagnostic");

        assert!(diagnostic
            .suggestions
            .iter()
            .any(|suggestion| suggestion.contains("Increase the measured window")));
        assert!(diagnostic
            .suggestions
            .iter()
            .any(|suggestion| suggestion.contains("one-off setup")));
        assert!(diagnostic.evidence.contains_key("median_elapsed_ns"));
        assert!(diagnostic.evidence.contains_key("max_to_median"));
    }

    #[test]
    fn high_variance_suggestions_include_variable_work_and_scheduler_shape() {
        let mut spec = tier3_spec("system");
        spec.parameters
            .insert("worker_threads".to_string(), "8".to_string());
        let samples = vec![
            completed_sample("system", 0, 1_000_000_000, 100),
            completed_sample("system", 1, 1_000_000_000, 100),
            completed_sample("system", 2, 1_000_000_000, 1000),
            completed_sample("system", 3, 1_000_000_000, 100),
            completed_sample("system", 4, 1_000_000_000, 100),
        ];

        let summary = summarize_benchmark(&spec, &samples);
        let diagnostic = summary
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "high_variance")
            .expect("high variance diagnostic");

        assert!(diagnostic
            .suggestions
            .iter()
            .any(|suggestion| { suggestion.contains("completed operations vary") }));
        assert!(diagnostic
            .suggestions
            .iter()
            .any(|suggestion| suggestion.contains("pin and limit worker concurrency")));
        assert_eq!(
            diagnostic.evidence.get("scheduler_sensitive"),
            Some(&"true".to_string())
        );
        assert!(diagnostic.evidence.contains_key("completed_operations_rsd"));
        assert!(diagnostic.evidence.contains_key("wall_clock_rsd"));
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
    fn comparison_treats_ns_per_op_basis_mismatch_as_semantic_change() {
        let baseline = summarize_benchmark(
            &micro_spec("hot_path"),
            &(0..10)
                .map(|i| micro_sample("hot_path", i, 100))
                .collect::<Vec<_>>(),
        );
        let mut current = summarize_benchmark(
            &micro_spec("hot_path"),
            &(0..10)
                .map(|i| micro_sample("hot_path", i, 200))
                .collect::<Vec<_>>(),
        );
        current.metadata.insert(
            "ns_per_op_basis".to_string(),
            "logical_completed_operation".to_string(),
        );

        let comparison = compare_summaries(&[current], &[baseline], 0.05)
            .into_iter()
            .next()
            .expect("comparison");

        assert_eq!(comparison.classification, ComparisonClass::Inconclusive);
        assert!(comparison
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("ns_per_op_basis changed")));
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
            diagnostics_summary: Vec::new(),
            started_at: "123".to_string(),
            total_elapsed_ns: 0,
            metadata: BTreeMap::new(),
        };

        let json = serde_json::to_value(&run).expect("serialize");

        assert_eq!(json["schema_version"], SCHEMA_VERSION);
        assert_eq!(json["samples"].as_array().expect("samples").len(), 0);
    }

    #[test]
    fn json_duration_fields_round_trip_from_emitted_numbers() {
        let profile_config = ProfileConfig::default();
        let run = StressRun {
            schema_version: SCHEMA_VERSION.to_string(),
            tool_version: "0.3.0".to_string(),
            suite: "suite".to_string(),
            run_profile: profile_config.profile,
            environment: EnvironmentInfo::unknown(profile_config),
            benchmark_specs: vec![micro_spec("micro"), tier3_spec("duration")],
            samples: Vec::new(),
            summaries: Vec::new(),
            comparisons: Vec::new(),
            diagnostics_summary: Vec::new(),
            started_at: "123".to_string(),
            total_elapsed_ns: 0,
            metadata: BTreeMap::new(),
        };

        let json = serde_json::to_string(&run).expect("serialize");
        let parsed = StressRun::from_json_str(&json).expect("round trip");

        assert_eq!(parsed.benchmark_specs, run.benchmark_specs);
        assert_eq!(
            parsed.environment.profile_config.sample_duration,
            run.environment.profile_config.sample_duration
        );
    }

    #[test]
    fn old_v2_json_without_new_defaulted_fields_still_parses() {
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
            diagnostics_summary: Vec::new(),
            started_at: "123".to_string(),
            total_elapsed_ns: 0,
            metadata: BTreeMap::new(),
        };
        let mut json = serde_json::to_value(&run).expect("serialize");
        json.as_object_mut()
            .expect("run object")
            .remove("diagnostics_summary");
        let profile = json
            .get_mut("environment")
            .and_then(|environment| environment.get_mut("profile_config"))
            .and_then(serde_json::Value::as_object_mut)
            .expect("profile config");
        profile.remove("deny_diagnostics");
        profile.remove("console_names");
        profile.remove("progress");

        let parsed =
            StressRun::from_json_str(&serde_json::to_string(&json).expect("json")).expect("parse");

        assert!(parsed.diagnostics_summary.is_empty());
        assert_eq!(parsed.environment.profile_config.deny_diagnostics, None);
        assert_eq!(
            parsed.environment.profile_config.console_names,
            ConsoleNameMode::Compact
        );
        assert!(parsed.environment.profile_config.progress);
    }

    #[test]
    fn summary_and_diagnostic_trust_class_default_when_missing_from_json() {
        let run = StressRun {
            schema_version: SCHEMA_VERSION.to_string(),
            tool_version: "0.3.0".to_string(),
            suite: "suite".to_string(),
            run_profile: RunProfile::Default,
            environment: EnvironmentInfo::unknown(ProfileConfig::default()),
            benchmark_specs: Vec::new(),
            samples: Vec::new(),
            summaries: vec![summarize_benchmark(
                &spec("bench"),
                &(0..5)
                    .map(|i| sample("bench", SamplePhase::Measured, i, 100 + i as u128))
                    .collect::<Vec<_>>(),
            )],
            comparisons: Vec::new(),
            diagnostics_summary: vec![DiagnosticSummary {
                suite: "suite".to_string(),
                benchmark_id: "bench".to_string(),
                name: "bench".to_string(),
                tier: 1,
                code: "high_variance".to_string(),
                severity: DiagnosticSeverity::Warning,
                reason: "high variance".to_string(),
                evidence: BTreeMap::new(),
                suggestions: vec!["rerun".to_string()],
                quality: QualityClass::Noisy,
                trust_class: TrustClass::Diagnostic,
                parameters: BTreeMap::new(),
            }],
            started_at: "123".to_string(),
            total_elapsed_ns: 0,
            metadata: BTreeMap::new(),
        };
        let mut json = serde_json::to_value(&run).expect("serialize");
        json["summaries"][0]
            .as_object_mut()
            .expect("summary object")
            .remove("trust_class");
        json["diagnostics_summary"][0]
            .as_object_mut()
            .expect("diagnostic summary object")
            .remove("trust_class");

        let parsed =
            StressRun::from_json_str(&serde_json::to_string(&json).expect("json")).expect("parse");

        assert_eq!(parsed.summaries[0].trust_class, TrustClass::Gate);
        assert_eq!(parsed.diagnostics_summary[0].trust_class, TrustClass::Gate);
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
            diagnostics_summary: Vec::new(),
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
