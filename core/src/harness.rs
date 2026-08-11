//! Harness for auto-discovered stress benchmarks.

use crate::artifact::{
    BenchmarkBudgets, BenchmarkModeKind, BenchmarkSpec, ConsoleNameMode, DiagnosticSeverity,
    QualityClass, RunProfile, StressRun,
};
use crate::config::{parse_bool_env, StressRunnerConfig};
use crate::reporting::{atomic_write, Reporter};
use crate::runner::{evaluate_run_gate, RunGate, StressRunner};
use crate::{StressContext, StressResult};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::Duration;

/// A registered benchmark entry.
#[doc(hidden)]
pub struct BenchmarkEntry {
    /// Benchmark name.
    pub name: &'static str,
    /// Rust function name used for stable ids.
    pub function_name: &'static str,
    /// Benchmark function.
    pub func: fn(&mut StressContext) -> StressResult,
    /// Whether this benchmark is ignored by default.
    pub ignored: bool,
    /// Module path where the benchmark is defined.
    pub module_path: &'static str,
    /// Numeric tier.
    pub tier: u32,
    /// Static benchmark mode kind.
    pub mode: BenchmarkModeKind,
    /// Static budget gates.
    pub budgets: BenchmarkBudgets,
    /// Static descriptive metadata.
    pub metadata: &'static [(&'static str, &'static str)],
}

#[doc(hidden)]
pub use linkme;

/// Distributed slice collecting all registered benchmarks.
#[doc(hidden)]
#[linkme::distributed_slice]
pub static STRESS_BENCHMARKS: [BenchmarkEntry];

#[derive(Debug, Clone, Default)]
struct StressBinaryArgs {
    workload: Option<String>,
    profile: Option<RunProfile>,
    tier: Option<u32>,
    samples: Option<usize>,
    warmup_samples: Option<usize>,
    cooldown_samples: Option<usize>,
    timeout: Option<Duration>,
    operations_per_sample: Option<NonZeroU64>,
    sample_duration_ms: Option<NonZeroU64>,
    micro_sample_duration_ms: Option<NonZeroU64>,
    json_stdout: Option<bool>,
    include_ignored: Option<bool>,
    list: bool,
    print_config: bool,
    output_dir: Option<PathBuf>,
    baseline: Option<PathBuf>,
    baseline_dir: Option<PathBuf>,
    save_baseline: Option<bool>,
    threshold: Option<f64>,
    fail_on_issues: Option<bool>,
    deny_diagnostics: Option<DiagnosticSeverity>,
    names: Option<ConsoleNameMode>,
    no_progress: Option<bool>,
    selection_probe: bool,
}

#[derive(Debug, Clone)]
struct ResolvedStressConfig {
    config: StressRunnerConfig,
    metadata: BTreeMap<String, String>,
    warnings: Vec<String>,
    artifact_namespace: Option<String>,
    workload: Option<String>,
    include_ignored: bool,
    baseline: Option<PathBuf>,
    baseline_dir: PathBuf,
    save_baseline: bool,
    print_config: bool,
}

impl StressBinaryArgs {
    fn parse() -> Result<Self, String> {
        let args = std::env::args().collect::<Vec<_>>();
        Self::parse_from_args(&args)
    }

    #[allow(clippy::too_many_lines)]
    fn parse_from_args(args: &[String]) -> Result<Self, String> {
        let mut result = Self::default();
        let mut seen_singletons = BTreeSet::new();
        let mut index = 1;

        while index < args.len() {
            if let Some(canonical) = singleton_argument(args[index].as_str()) {
                if !seen_singletons.insert(canonical) {
                    return Err(format!(
                        "argument '{canonical}' may be specified only once (including aliases)"
                    ));
                }
            }
            match args[index].as_str() {
                "--workload" | "--filter" => {
                    let flag = args[index].as_str();
                    result.workload =
                        Some(required_flag_value(args, &mut index, flag)?.to_string());
                }
                "--profile" => {
                    let value = required_flag_value(args, &mut index, "--profile")?;
                    result.profile = Some(parse_flag_value(
                        "--profile",
                        value,
                        "default, smoke, lab, or release",
                    )?);
                }
                "--tier" => {
                    let value = required_flag_value(args, &mut index, "--tier")?;
                    result.tier =
                        Some(parse_flag_value("--tier", value, "an integer from 1 to 6")?);
                }
                "--samples" => {
                    let value = required_flag_value(args, &mut index, "--samples")?;
                    result.samples = Some(parse_flag_value(
                        "--samples",
                        value,
                        "a non-negative integer",
                    )?);
                }
                "--warmup-samples" => {
                    let value = required_flag_value(args, &mut index, "--warmup-samples")?;
                    result.warmup_samples = Some(parse_flag_value(
                        "--warmup-samples",
                        value,
                        "a non-negative integer",
                    )?);
                }
                "--cooldown-samples" => {
                    let value = required_flag_value(args, &mut index, "--cooldown-samples")?;
                    result.cooldown_samples = Some(parse_flag_value(
                        "--cooldown-samples",
                        value,
                        "a non-negative integer",
                    )?);
                }
                "--operations-per-sample" => {
                    let value = required_flag_value(args, &mut index, "--operations-per-sample")?;
                    result.operations_per_sample =
                        Some(parse_positive_flag_value("--operations-per-sample", value)?);
                }
                "--sample-duration-ms" => {
                    let value = required_flag_value(args, &mut index, "--sample-duration-ms")?;
                    result.sample_duration_ms =
                        Some(parse_positive_flag_value("--sample-duration-ms", value)?);
                }
                "--micro-sample-duration-ms" => {
                    let value =
                        required_flag_value(args, &mut index, "--micro-sample-duration-ms")?;
                    result.micro_sample_duration_ms = Some(parse_positive_flag_value(
                        "--micro-sample-duration-ms",
                        value,
                    )?);
                }
                "--timeout-secs" => {
                    let value = required_flag_value(args, &mut index, "--timeout-secs")?;
                    let seconds = parse_positive_flag_value("--timeout-secs", value)?;
                    result.timeout = Some(Duration::from_secs(seconds.get()));
                }
                "--json" => {
                    result.json_stdout = Some(true);
                }
                "--include-ignored" => {
                    result.include_ignored = Some(true);
                }
                "--list" => {
                    result.list = true;
                }
                "--print-config" | "--dry-run-config" => {
                    result.print_config = true;
                }
                "--output-dir" => {
                    let value = required_flag_value(args, &mut index, "--output-dir")?;
                    result.output_dir = Some(PathBuf::from(value));
                }
                "--baseline" => {
                    let value = required_flag_value(args, &mut index, "--baseline")?;
                    result.baseline = Some(PathBuf::from(value));
                }
                "--baseline-dir" => {
                    let value = required_flag_value(args, &mut index, "--baseline-dir")?;
                    result.baseline_dir = Some(PathBuf::from(value));
                }
                "--save-baseline" => {
                    result.save_baseline = Some(true);
                }
                "--threshold" => {
                    let value = required_flag_value(args, &mut index, "--threshold")?;
                    let expected = "a finite fraction from 0 to 1 (0.05 means 5%)";
                    let threshold = parse_flag_value("--threshold", value, expected)?;
                    if !f64::is_finite(threshold) || !(0.0..=1.0).contains(&threshold) {
                        return Err(invalid_flag_value("--threshold", value, expected));
                    }
                    result.threshold = Some(threshold);
                }
                "--fail-on-issues" => {
                    result.fail_on_issues = Some(true);
                }
                "--deny-diagnostics" => {
                    let value = required_flag_value(args, &mut index, "--deny-diagnostics")?;
                    result.deny_diagnostics = Some(parse_flag_value(
                        "--deny-diagnostics",
                        value,
                        "info, warning, or error",
                    )?);
                }
                "--names" => {
                    let value = required_flag_value(args, &mut index, "--names")?;
                    result.names = Some(parse_flag_value("--names", value, "compact or full")?);
                }
                "--no-progress" => {
                    result.no_progress = Some(true);
                }
                "--__cntryl-stress-selection-probe" => {
                    result.selection_probe = true;
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                // Cargo appends this libtest-compatible marker when it executes a
                // `harness = false` benchmark through `cargo bench`.
                "--bench" => {}
                unknown => {
                    return Err(format!(
                        "unknown argument '{unknown}'; use --help to list supported options"
                    ));
                }
            }
            index += 1;
        }

        Ok(result)
    }
}

fn singleton_argument(argument: &str) -> Option<&'static str> {
    match argument {
        "--workload" | "--filter" => Some("--workload"),
        "--profile" => Some("--profile"),
        "--tier" => Some("--tier"),
        "--samples" => Some("--samples"),
        "--warmup-samples" => Some("--warmup-samples"),
        "--cooldown-samples" => Some("--cooldown-samples"),
        "--operations-per-sample" => Some("--operations-per-sample"),
        "--sample-duration-ms" => Some("--sample-duration-ms"),
        "--micro-sample-duration-ms" => Some("--micro-sample-duration-ms"),
        "--timeout-secs" => Some("--timeout-secs"),
        "--json" => Some("--json"),
        "--include-ignored" => Some("--include-ignored"),
        "--list" => Some("--list"),
        "--print-config" | "--dry-run-config" => Some("--print-config"),
        "--output-dir" => Some("--output-dir"),
        "--baseline" => Some("--baseline"),
        "--baseline-dir" => Some("--baseline-dir"),
        "--save-baseline" => Some("--save-baseline"),
        "--threshold" => Some("--threshold"),
        "--fail-on-issues" => Some("--fail-on-issues"),
        "--deny-diagnostics" => Some("--deny-diagnostics"),
        "--names" => Some("--names"),
        "--no-progress" => Some("--no-progress"),
        "--__cntryl-stress-selection-probe" => Some("--__cntryl-stress-selection-probe"),
        _ => None,
    }
}

fn required_flag_value<'a>(
    args: &'a [String],
    index: &mut usize,
    flag: &str,
) -> Result<&'a str, String> {
    *index += 1;
    let value = args
        .get(*index)
        .filter(|value| !value.starts_with('-'))
        .ok_or_else(|| format!("missing value for {flag}"))?;
    Ok(value)
}

fn parse_flag_value<T>(flag: &str, value: &str, expected: &str) -> Result<T, String>
where
    T: std::str::FromStr,
{
    value
        .parse()
        .map_err(|_| invalid_flag_value(flag, value, expected))
}

fn parse_positive_flag_value(flag: &str, value: &str) -> Result<NonZeroU64, String> {
    let parsed = parse_flag_value::<u64>(flag, value, "a positive integer")?;
    NonZeroU64::new(parsed).ok_or_else(|| invalid_flag_value(flag, value, "a positive integer"))
}

fn invalid_flag_value(flag: &str, value: &str, expected: &str) -> String {
    format!("invalid value '{value}' for {flag}; expected {expected}")
}

fn print_help() {
    eprintln!("Stress benchmark binary");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("    <binary> [OPTIONS]");
    eprintln!();
    eprintln!("OPTIONS:");
    eprintln!(
        "    --profile <default|smoke|lab|release>  Optional profile override; default is default"
    );
    eprintln!("    --workload <PATTERN>           Filter benchmarks by name/module glob");
    eprintln!("    --tier <N>                     Run one numeric tier");
    eprintln!("    --samples <N>                  Measured samples per benchmark");
    eprintln!("    --warmup-samples <N>           Warmup samples");
    eprintln!("    --cooldown-samples <N>         Cooldown samples");
    eprintln!("    --operations-per-sample <N>    Operations in each fixed-operations sample");
    eprintln!("    --sample-duration-ms <N>       Milliseconds in each fixed-duration sample");
    eprintln!(
        "    --micro-sample-duration-ms <N> Target milliseconds for calibrated micro samples"
    );
    eprintln!("    --timeout-secs <N>             Per-benchmark deadline in seconds");
    eprintln!("    --json                         Print machine-readable JSON to stdout");
    eprintln!("    --include-ignored              Include ignored benchmarks");
    eprintln!("    --list                         List benchmarks");
    eprintln!("    --print-config                 Print resolved config");
    eprintln!("    --output-dir <PATH>            Artifact output directory");
    eprintln!("    --baseline <PATH>              Current baseline artifact");
    eprintln!("    --baseline latest              Use baseline-dir/latest/<suite>.json");
    eprintln!(
        "    --baseline-dir <PATH>          Baseline directory (default target/stress/baselines)"
    );
    eprintln!("    --save-baseline                Save passed runs under baseline-dir");
    eprintln!("    --threshold <FRACTION>         Regression fraction (0.05 means 5%)");
    eprintln!("    --fail-on-issues               Fail on warning-or-error diagnostics");
    eprintln!("    --deny-diagnostics <LEVEL>     Fail on diagnostics at info, warning, or error");
    eprintln!("    --names <compact|full>         Human console benchmark-name mode");
    eprintln!("    --no-progress                  Disable stderr progress for human output");
}

/// Entry point used by `stress_main!`.
pub fn stress_binary_main() {
    run_from_env_and_args();
}

/// Parse environment/CLI and run registered benchmarks.
pub fn run_from_env_and_args() {
    let args = StressBinaryArgs::parse().unwrap_or_else(|error| {
        eprintln!("Invalid stress arguments: {error}");
        std::process::exit(2);
    });

    if args.list {
        print_benchmark_list();
        return;
    }

    let resolved = resolve_from_binary_args(&args);
    if let Some(error) = environment_validation_error(&resolved.warnings) {
        eprintln!("{error}");
        std::process::exit(2);
    }

    exit_on_invalid_config(&resolved.config);

    if args.selection_probe {
        print_selection_probe(&resolved);
        return;
    }

    if resolved.print_config {
        print_resolved_config(&get_suite_name(), &resolved);
        return;
    }

    run_with_resolved_config(resolved);
}

fn environment_validation_error(warnings: &[String]) -> Option<String> {
    (!warnings.is_empty()).then(|| format!("Invalid stress environment: {}", warnings.join("; ")))
}

/// Options for programmatic execution of registered benchmarks.
#[derive(Debug, Clone, Default)]
pub struct StressRunnerOptions {
    /// Benchmark name/module filter.
    pub workload: Option<String>,
    /// Optional include-ignored override.
    pub include_ignored: Option<bool>,
    /// Run profile.
    pub profile: Option<RunProfile>,
    /// Exact tier filter.
    pub tier: Option<u32>,
    /// Measured samples.
    pub samples: Option<usize>,
    /// Warmup samples.
    pub warmup_samples: Option<usize>,
    /// Cooldown samples.
    pub cooldown_samples: Option<usize>,
    /// Per-benchmark deadline.
    pub timeout: Option<Duration>,
    /// Optional machine-readable JSON stdout override.
    pub json_stdout: Option<bool>,
    /// Artifact output directory.
    pub output_dir: Option<PathBuf>,
    /// Baseline artifact.
    pub baseline: Option<PathBuf>,
    /// Baseline directory for latest/save conventions.
    pub baseline_dir: Option<PathBuf>,
    /// Optional save-passed-runs override.
    pub save_baseline: Option<bool>,
    /// Regression threshold in percentage points (`5.0` means five percent).
    pub threshold_percent: Option<f64>,
    /// Strict diagnostic gate threshold.
    pub deny_diagnostics: Option<DiagnosticSeverity>,
    /// Human console benchmark-name mode.
    pub names: Option<ConsoleNameMode>,
    /// Whether human runs emit stderr progress.
    pub progress: Option<bool>,
}

impl StressRunnerOptions {
    /// Create default options.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set workload filter.
    #[must_use]
    pub fn workload(mut self, pattern: impl Into<String>) -> Self {
        self.workload = Some(pattern.into());
        self
    }

    /// Include ignored benchmarks.
    #[must_use]
    pub fn include_ignored(mut self, value: bool) -> Self {
        self.include_ignored = Some(value);
        self
    }

    /// Set profile.
    #[must_use]
    pub const fn profile(mut self, profile: RunProfile) -> Self {
        self.profile = Some(profile);
        self
    }

    /// Set exact tier filter.
    #[must_use]
    pub const fn tier(mut self, tier: u32) -> Self {
        self.tier = Some(tier);
        self
    }

    /// Set measured samples.
    #[must_use]
    pub const fn samples(mut self, samples: usize) -> Self {
        self.samples = Some(samples);
        self
    }

    /// Set warmup samples.
    #[must_use]
    pub const fn warmup_samples(mut self, warmup_samples: usize) -> Self {
        self.warmup_samples = Some(warmup_samples);
        self
    }

    /// Set cooldown samples.
    #[must_use]
    pub const fn cooldown_samples(mut self, cooldown_samples: usize) -> Self {
        self.cooldown_samples = Some(cooldown_samples);
        self
    }

    /// Set the per-benchmark deadline.
    #[must_use]
    pub const fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Print machine-readable JSON to stdout.
    #[must_use]
    pub const fn json_stdout(mut self, value: bool) -> Self {
        self.json_stdout = Some(value);
        self
    }

    /// Set the artifact output directory.
    #[must_use]
    pub fn output_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.output_dir = Some(path.into());
        self
    }

    /// Set baseline artifact.
    #[must_use]
    pub fn baseline(mut self, path: impl Into<PathBuf>) -> Self {
        self.baseline = Some(path.into());
        self
    }

    /// Set baseline directory for latest/save conventions.
    #[must_use]
    pub fn baseline_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.baseline_dir = Some(path.into());
        self
    }

    /// Save passed runs as baselines.
    #[must_use]
    pub const fn save_baseline(mut self, value: bool) -> Self {
        self.save_baseline = Some(value);
        self
    }

    /// Set the regression threshold in percentage points (`5.0` means 5%).
    ///
    /// # Panics
    ///
    /// Panics unless `threshold_percent` is finite and between 0 and 100.
    #[must_use]
    pub fn threshold_percent(mut self, threshold_percent: f64) -> Self {
        assert!(
            threshold_percent.is_finite() && (0.0..=100.0).contains(&threshold_percent),
            "regression threshold percent must be between 0 and 100"
        );
        self.threshold_percent = Some(threshold_percent);
        self
    }

    /// Set strict diagnostic gate threshold.
    #[must_use]
    pub const fn deny_diagnostics(mut self, threshold: DiagnosticSeverity) -> Self {
        self.deny_diagnostics = Some(threshold);
        self
    }

    /// Alias for warning-or-higher diagnostic gating.
    #[must_use]
    pub const fn fail_on_issues(mut self, value: bool) -> Self {
        self.deny_diagnostics = if value {
            Some(DiagnosticSeverity::Warning)
        } else {
            None
        };
        self
    }

    /// Set human console benchmark-name mode.
    #[must_use]
    pub const fn names(mut self, mode: ConsoleNameMode) -> Self {
        self.names = Some(mode);
        self
    }

    /// Set whether human runs emit stderr progress.
    #[must_use]
    pub const fn progress(mut self, value: bool) -> Self {
        self.progress = Some(value);
        self
    }
}

/// Run all registered benchmarks with default options.
#[allow(dead_code)]
pub fn run_registered_benchmarks() {
    run_with_options(StressRunnerOptions::new());
}

/// Run all registered benchmarks with programmatic options.
#[allow(dead_code)]
pub fn run_with_options(options: StressRunnerOptions) {
    let args = binary_args_from_options(options);
    run_with_resolved_config(resolve_from_binary_args(&args));
}

fn binary_args_from_options(options: StressRunnerOptions) -> StressBinaryArgs {
    StressBinaryArgs {
        workload: options.workload,
        profile: options.profile,
        tier: options.tier,
        samples: options.samples,
        warmup_samples: options.warmup_samples,
        cooldown_samples: options.cooldown_samples,
        timeout: options.timeout,
        json_stdout: options.json_stdout,
        output_dir: options.output_dir,
        include_ignored: options.include_ignored,
        baseline: options.baseline,
        baseline_dir: options.baseline_dir,
        save_baseline: options.save_baseline,
        threshold: options
            .threshold_percent
            .map(|threshold_percent| threshold_percent / 100.0),
        deny_diagnostics: options.deny_diagnostics,
        names: options.names,
        no_progress: options.progress.map(|progress| !progress),
        ..StressBinaryArgs::default()
    }
}

fn resolve_from_binary_args(args: &StressBinaryArgs) -> ResolvedStressConfig {
    resolve_from_binary_args_with(args, |key| std::env::var(key).ok())
}

#[allow(clippy::too_many_lines)]
fn resolve_from_binary_args_with<F>(args: &StressBinaryArgs, get_var: F) -> ResolvedStressConfig
where
    F: Fn(&str) -> Option<String>,
{
    let env_resolution = StressRunnerConfig::resolve_from_env_with_profile_override(
        &get_var,
        args.profile.map(|profile| (profile, "cli --profile")),
    );
    let mut config = env_resolution.config;
    let mut metadata = env_resolution
        .metadata
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    let mut warnings = env_resolution.warnings;

    let mut include_ignored = get_var("STRESS_INCLUDE_IGNORED")
        .and_then(|value| {
            if let Some(value) = parse_bool_env(&value) {
                Some(value)
            } else {
                warnings.push("invalid STRESS_INCLUDE_IGNORED; expected true or false".to_string());
                None
            }
        })
        .unwrap_or(false);
    metadata.insert(
        "include_ignored_src".to_string(),
        source_for(&get_var, "STRESS_INCLUDE_IGNORED"),
    );

    let mut baseline = get_var("STRESS_BASELINE").map(PathBuf::from);
    metadata.insert(
        "baseline_src".to_string(),
        source_for(&get_var, "STRESS_BASELINE"),
    );
    let mut baseline_dir =
        get_var("STRESS_BASELINE_DIR").map_or_else(default_baseline_dir, PathBuf::from);
    metadata.insert(
        "baseline_dir_src".to_string(),
        source_for(&get_var, "STRESS_BASELINE_DIR"),
    );
    let mut save_baseline = get_var("STRESS_SAVE_BASELINE")
        .and_then(|value| {
            if let Some(value) = parse_bool_env(&value) {
                Some(value)
            } else {
                warnings.push("invalid STRESS_SAVE_BASELINE; expected true or false".to_string());
                None
            }
        })
        .unwrap_or(false);
    metadata.insert(
        "save_baseline_src".to_string(),
        source_for(&get_var, "STRESS_SAVE_BASELINE"),
    );

    if let Some(tier) = args.tier {
        config.tier = Some(tier);
        metadata.insert("tier_src".to_string(), "cli --tier".to_string());
    }
    if let Some(samples) = args.samples {
        config.samples = samples;
        metadata.insert("samples_src".to_string(), "cli --samples".to_string());
    }
    if let Some(warmup_samples) = args.warmup_samples {
        config.warmup_samples = warmup_samples;
        metadata.insert(
            "warmup_samples_src".to_string(),
            "cli --warmup-samples".to_string(),
        );
    }
    if let Some(cooldown_samples) = args.cooldown_samples {
        config.cooldown_samples = cooldown_samples;
        metadata.insert(
            "cooldown_samples_src".to_string(),
            "cli --cooldown-samples".to_string(),
        );
    }
    if let Some(operations_per_sample) = args.operations_per_sample {
        config.operations_per_sample = operations_per_sample.get();
        metadata.insert(
            "operations_per_sample_src".to_string(),
            "cli --operations-per-sample".to_string(),
        );
    }
    if let Some(sample_duration_ms) = args.sample_duration_ms {
        config.sample_duration = Duration::from_millis(sample_duration_ms.get());
        metadata.insert(
            "sample_duration_src".to_string(),
            "cli --sample-duration-ms".to_string(),
        );
    }
    if let Some(micro_sample_duration_ms) = args.micro_sample_duration_ms {
        config.micro_sample_duration = Duration::from_millis(micro_sample_duration_ms.get());
        metadata.insert(
            "micro_sample_duration_src".to_string(),
            "cli --micro-sample-duration-ms".to_string(),
        );
    }
    if let Some(timeout) = args.timeout {
        config.timeout = Some(timeout);
        metadata.insert(
            "timeout_secs_src".to_string(),
            "cli --timeout-secs".to_string(),
        );
    }
    if let Some(json_stdout) = args.json_stdout {
        config.json_stdout = json_stdout;
        metadata.insert("json_stdout_src".to_string(), "cli --json".to_string());
    }
    if let Some(output_dir) = &args.output_dir {
        config.output_dir.clone_from(output_dir);
        metadata.insert("output_dir_src".to_string(), "cli --output-dir".to_string());
    }
    if let Some(workload) = &args.workload {
        config.filter = Some(workload.clone());
        metadata.insert("filter_src".to_string(), "cli --workload".to_string());
    }
    if let Some(value) = args.include_ignored {
        include_ignored = value;
        metadata.insert(
            "include_ignored_src".to_string(),
            "cli --include-ignored".to_string(),
        );
    }
    if let Some(path) = &args.baseline {
        baseline = Some(path.clone());
        metadata.insert("baseline_src".to_string(), "cli --baseline".to_string());
    }
    if let Some(path) = &args.baseline_dir {
        baseline_dir.clone_from(path);
        metadata.insert(
            "baseline_dir_src".to_string(),
            "cli --baseline-dir".to_string(),
        );
    }
    if let Some(value) = args.save_baseline {
        save_baseline = value;
        metadata.insert(
            "save_baseline_src".to_string(),
            "cli --save-baseline".to_string(),
        );
    }
    if let Some(threshold) = args.threshold {
        config.threshold = threshold;
        metadata.insert("threshold_src".to_string(), "cli --threshold".to_string());
    }
    if let Some(value) = args.fail_on_issues {
        config.deny_diagnostics = value.then_some(DiagnosticSeverity::Warning);
        metadata.insert(
            "deny_diagnostics_src".to_string(),
            "cli --fail-on-issues".to_string(),
        );
    }
    if let Some(threshold) = args.deny_diagnostics {
        config.deny_diagnostics = Some(threshold);
        metadata.insert(
            "deny_diagnostics_src".to_string(),
            "cli --deny-diagnostics".to_string(),
        );
    }
    if let Some(mode) = args.names {
        config.console_names = mode;
        metadata.insert("console_names_src".to_string(), "cli --names".to_string());
    }
    if args.no_progress == Some(true) {
        config.progress = false;
        metadata.insert("progress_src".to_string(), "cli --no-progress".to_string());
    }
    let artifact_namespace = get_var("STRESS_ARTIFACT_NAMESPACE")
        .filter(|namespace| !namespace.trim().is_empty())
        .map(|namespace| portable_artifact_namespace(&namespace));
    if let Some(namespace) = &artifact_namespace {
        config.output_dir.push(namespace);
        metadata.insert("artifact_namespace".to_string(), namespace.clone());
        metadata.insert(
            "artifact_namespace_src".to_string(),
            "env STRESS_ARTIFACT_NAMESPACE".to_string(),
        );
    }

    let workload = config.filter.take();
    ResolvedStressConfig {
        workload,
        config,
        metadata,
        warnings,
        artifact_namespace,
        include_ignored,
        baseline,
        baseline_dir,
        save_baseline,
        print_config: args.print_config,
    }
}

fn exit_on_invalid_config(config: &StressRunnerConfig) {
    let errors = config.validation_errors();
    if errors.is_empty() {
        return;
    }

    eprintln!("Invalid stress config: {}", errors.join("; "));
    std::process::exit(1);
}

#[derive(Debug)]
enum SpecRunError {
    Timeout {
        benchmark_id: String,
        timeout: Duration,
    },
    Spawn {
        benchmark_id: String,
        reason: String,
    },
    Panicked {
        benchmark_id: String,
    },
}

impl SpecRunError {
    const fn exit_code(&self) -> i32 {
        if matches!(self, Self::Timeout { .. }) {
            124
        } else {
            1
        }
    }
}

impl fmt::Display for SpecRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout {
                benchmark_id,
                timeout,
            } => write!(
                f,
                "benchmark {benchmark_id:?} exceeded its {:.3}s deadline",
                timeout.as_secs_f64()
            ),
            Self::Spawn {
                benchmark_id,
                reason,
            } => write!(
                f,
                "could not start isolated benchmark {benchmark_id:?}: {reason}"
            ),
            Self::Panicked { benchmark_id } => {
                write!(f, "benchmark {benchmark_id:?} panicked")
            }
        }
    }
}

fn run_spec_with_timeout(
    mut runner: StressRunner,
    spec: BenchmarkSpec,
    func: fn(&mut StressContext) -> StressResult,
    timeout: Duration,
) -> Result<StressRunner, SpecRunError> {
    let benchmark_id = spec.id.clone();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let handle = std::thread::Builder::new()
        .name(format!("stress-{}", sanitize_thread_name(&benchmark_id)))
        .spawn(move || {
            runner.run_spec(&spec, func);
            let _ = sender.send(runner);
        })
        .map_err(|error| SpecRunError::Spawn {
            benchmark_id: benchmark_id.clone(),
            reason: error.to_string(),
        })?;

    match receiver.recv_timeout(timeout) {
        Ok(runner) => {
            handle.join().map_err(|_| SpecRunError::Panicked {
                benchmark_id: benchmark_id.clone(),
            })?;
            Ok(runner)
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(SpecRunError::Timeout {
            benchmark_id,
            timeout,
        }),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            let _ = handle.join();
            Err(SpecRunError::Panicked { benchmark_id })
        }
    }
}

fn sanitize_thread_name(benchmark_id: &str) -> String {
    benchmark_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .take(48)
        .collect()
}

fn run_with_resolved_config(resolved: ResolvedStressConfig) {
    exit_on_invalid_config(&resolved.config);

    let benchmarks = required_benchmarks(&resolved).unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(1);
    });

    let suite_name = get_suite_name();
    let baseline = resolved.baseline.as_ref().map(|path| {
        resolve_baseline_path(
            path,
            &resolved.baseline_dir,
            &suite_name,
            resolved.artifact_namespace.as_deref(),
        )
    });
    if let Some(baseline_path) = baseline.as_deref() {
        if resolved
            .baseline
            .as_deref()
            .is_some_and(|configured| configured.as_os_str() != "latest")
            && baseline_aliases_output_latest(
                baseline_path,
                &resolved.config.output_dir,
                &suite_name,
            )
        {
            eprintln!(
                "Stress run rejected: explicit baseline {} is also this run's output latest.json and would be overwritten. Keep accepted baselines under a separate --baseline-dir, create them with --save-baseline, and compare with --baseline latest.",
                baseline_path.display()
            );
            std::process::exit(2);
        }
    }
    let config_for_specs = resolved.config.clone();
    let mut runner =
        StressRunner::with_config_and_metadata(&suite_name, resolved.config, resolved.metadata);
    if resolved.save_baseline {
        runner.add_reporter(Box::new(BaselineReporter::new(
            &resolved.baseline_dir,
            resolved.artifact_namespace.as_deref(),
        )));
    }

    for entry in benchmarks {
        let stable_name = format!("{}::{}", entry.module_path, entry.function_name);
        let display_name = format!("{}::{}", entry.module_path, entry.name);
        let id = format!("{suite_name}/{stable_name}");
        let metadata = entry
            .metadata
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect::<BTreeMap<_, _>>();
        let spec = BenchmarkSpec {
            id,
            name: display_name,
            tier: entry.tier,
            mode: config_for_specs.mode_for_kind(entry.mode),
            intent: crate::artifact::MeasurementIntent::General,
            budgets: entry.budgets,
            parameters: BTreeMap::new(),
            metadata,
        };
        if let Some(timeout) = config_for_specs.timeout {
            runner =
                run_spec_with_timeout(runner, spec, entry.func, timeout).unwrap_or_else(|error| {
                    eprintln!("Stress run failed: {error}");
                    std::process::exit(error.exit_code());
                });
        } else {
            runner.run_spec(&spec, entry.func);
        }
    }

    let run_result = if let Some(baseline_path) = baseline {
        runner.finish_with_baseline(baseline_path)
    } else {
        Ok(runner.finish())
    };

    match run_result {
        Ok(run) => {
            let gate = evaluate_run_gate(&run);
            if gate != RunGate::Passed {
                eprintln!("Stress run failed: {gate:?}");
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("Stress run failed to load baseline: {error}");
            std::process::exit(1);
        }
    }
}

fn empty_selection_error(resolved: &ResolvedStressConfig) -> String {
    if let Some(workload) = &resolved.workload {
        let suggestions = workload_suggestions_for_entries(workload, candidate_entries(resolved));
        if suggestions.is_empty() {
            format!("No benchmarks matched workload pattern '{workload}'")
        } else {
            format!(
                "No benchmarks matched workload pattern '{workload}'. Did you mean: {}?",
                suggestions.join(", ")
            )
        }
    } else {
        "No benchmarks registered. Add #[stress] to benchmark functions.".to_string()
    }
}

fn required_benchmarks(
    resolved: &ResolvedStressConfig,
) -> Result<Vec<&'static BenchmarkEntry>, String> {
    let benchmarks = selected_benchmarks(resolved);
    if benchmarks.is_empty() {
        Err(empty_selection_error(resolved))
    } else {
        Ok(benchmarks)
    }
}

#[derive(serde::Serialize)]
struct SelectionProbe<'a> {
    workload: Option<&'a str>,
    tier: Option<u32>,
    include_ignored: bool,
    selected: Vec<SelectionProbeBenchmark<'a>>,
    registered: Vec<SelectionProbeBenchmark<'a>>,
}

#[derive(serde::Serialize)]
struct SelectionProbeBenchmark<'a> {
    name: &'a str,
    function_name: &'a str,
    module_path: &'a str,
    tier: u32,
    ignored: bool,
}

fn print_selection_probe(resolved: &ResolvedStressConfig) {
    let selected = selected_benchmarks(resolved)
        .into_iter()
        .map(selection_probe_benchmark)
        .collect();
    let registered = STRESS_BENCHMARKS
        .iter()
        .map(selection_probe_benchmark)
        .collect();
    let probe = SelectionProbe {
        workload: resolved.workload.as_deref(),
        tier: resolved.config.tier,
        include_ignored: resolved.include_ignored,
        selected,
        registered,
    };
    let output = serde_json::to_string(&probe).unwrap_or_else(|error| {
        eprintln!("Could not serialize stress selection probe: {error}");
        std::process::exit(2);
    });
    println!("{output}");
}

fn selection_probe_benchmark(entry: &BenchmarkEntry) -> SelectionProbeBenchmark<'_> {
    SelectionProbeBenchmark {
        name: entry.name,
        function_name: entry.function_name,
        module_path: entry.module_path,
        tier: entry.tier,
        ignored: entry.ignored,
    }
}

fn selected_benchmarks(resolved: &ResolvedStressConfig) -> Vec<&'static BenchmarkEntry> {
    candidate_entries(resolved)
        .filter(|entry| {
            if let Some(pattern) = &resolved.workload {
                entry_matches_workload(entry, pattern)
            } else {
                true
            }
        })
        .collect()
}

fn candidate_entries(
    resolved: &ResolvedStressConfig,
) -> impl Iterator<Item = &'static BenchmarkEntry> + '_ {
    STRESS_BENCHMARKS.iter().filter(|entry| {
        if entry.ignored && !resolved.include_ignored {
            return false;
        }
        if let Some(tier) = resolved.config.tier {
            if entry.tier != tier {
                return false;
            }
        }
        true
    })
}

fn entry_matches_workload(entry: &BenchmarkEntry, pattern: &str) -> bool {
    workload_candidates(entry)
        .iter()
        .any(|candidate| matches_glob(candidate, pattern))
}

fn workload_candidates(entry: &BenchmarkEntry) -> Vec<String> {
    let mut candidates = vec![
        entry.name.to_string(),
        entry.function_name.to_string(),
        entry.module_path.to_string(),
        format!("{}::{}", entry.module_path, entry.function_name),
        format!("{}::{}", entry.module_path, entry.name),
    ];
    candidates.sort();
    candidates.dedup();
    candidates
}

fn workload_suggestions_for_entries<'a>(
    pattern: &str,
    entries: impl IntoIterator<Item = &'a BenchmarkEntry>,
) -> Vec<String> {
    let normalized_pattern = normalize_workload_pattern(pattern);
    if normalized_pattern.is_empty() {
        return Vec::new();
    }

    let mut scored = entries
        .into_iter()
        .flat_map(workload_candidates)
        .map(|candidate| {
            let score = levenshtein_distance(&normalized_pattern, &candidate.to_lowercase());
            (score, candidate)
        })
        .filter(|(score, candidate)| {
            let threshold = normalized_pattern.len().max(candidate.len()) / 2;
            *score <= threshold.max(3)
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    scored.dedup_by(|left, right| left.1 == right.1);
    scored
        .into_iter()
        .map(|(_, candidate)| candidate)
        .take(5)
        .collect()
}

fn normalize_workload_pattern(pattern: &str) -> String {
    pattern
        .chars()
        .filter(|char| *char != '*')
        .collect::<String>()
        .to_lowercase()
}

fn levenshtein_distance(left: &str, right: &str) -> usize {
    let right_chars = right.chars().collect::<Vec<_>>();
    let mut costs = (0..=right_chars.len()).collect::<Vec<_>>();
    for (left_index, left_char) in left.chars().enumerate() {
        let mut previous_diagonal = left_index;
        costs[0] = left_index + 1;
        for (right_index, right_char) in right_chars.iter().enumerate() {
            let substitution = previous_diagonal + usize::from(left_char != *right_char);
            previous_diagonal = costs[right_index + 1];
            costs[right_index + 1] = (costs[right_index] + 1)
                .min(costs[right_index + 1] + 1)
                .min(substitution);
        }
    }
    costs[right_chars.len()]
}

fn print_benchmark_list() {
    let benchmarks = list_benchmarks();
    if benchmarks.is_empty() {
        println!("No benchmarks registered.");
    } else {
        println!("Registered benchmarks ({}):", benchmarks.len());
        for benchmark in benchmarks {
            println!("  {benchmark}");
        }
    }
}

fn get_suite_name() -> String {
    if let Some(suite) = std::env::var("STRESS_SUITE")
        .ok()
        .filter(|suite| !suite.trim().is_empty())
    {
        return suite;
    }
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
        })
        .map_or_else(
            || "stress".to_string(),
            |name| suite_name_from_executable(&name),
        )
}

/// Normalize Cargo's target spelling into the stable suite identity.
#[doc(hidden)]
#[must_use]
pub fn canonical_suite_name(name: &str) -> String {
    name.replace('_', "-")
}

fn suite_name_from_executable(name: &str) -> String {
    let clean_name = if let Some(dash_pos) = name.rfind('-') {
        let potential_hash = &name[dash_pos + 1..];
        if potential_hash.len() == 16 && potential_hash.chars().all(|char| char.is_ascii_hexdigit())
        {
            &name[..dash_pos]
        } else {
            name
        }
    } else {
        name
    };
    canonical_suite_name(clean_name)
}

fn portable_artifact_namespace(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect()
}

#[allow(clippy::too_many_lines)]
fn print_resolved_config(suite: &str, resolved: &ResolvedStressConfig) {
    println!("Benchmark Suite: {suite}");
    println!(
        "Profile: {} ({})",
        resolved.config.profile,
        resolved
            .metadata
            .get("profile_src")
            .map_or("unknown", String::as_str)
    );
    println!(
        "Samples: {} ({})",
        resolved.config.samples,
        resolved
            .metadata
            .get("samples_src")
            .map_or("unknown", String::as_str)
    );
    println!(
        "Warmup samples: {} ({})",
        resolved.config.warmup_samples,
        resolved
            .metadata
            .get("warmup_samples_src")
            .map_or("unknown", String::as_str)
    );
    println!(
        "Cooldown samples: {} ({})",
        resolved.config.cooldown_samples,
        resolved
            .metadata
            .get("cooldown_samples_src")
            .map_or("unknown", String::as_str)
    );
    println!(
        "Output: {} ({})",
        resolved.config.output_dir.display(),
        resolved
            .metadata
            .get("output_dir_src")
            .map_or("unknown", String::as_str)
    );
    println!(
        "Filter: {} ({})",
        resolved.workload.as_deref().unwrap_or("<none>"),
        resolved
            .metadata
            .get("filter_src")
            .map_or("unknown", String::as_str)
    );
    println!(
        "Tier: {} ({})",
        resolved
            .config
            .tier
            .map_or_else(|| "<any>".to_string(), |tier| tier.to_string()),
        resolved
            .metadata
            .get("tier_src")
            .map_or("unknown", String::as_str)
    );
    println!("JSON stdout: {}", resolved.config.json_stdout);
    println!("Include ignored: {}", resolved.include_ignored);
    println!(
        "Baseline: {} ({})",
        resolved
            .baseline
            .as_ref()
            .map_or_else(|| "<none>".to_string(), |path| path.display().to_string()),
        resolved
            .metadata
            .get("baseline_src")
            .map_or("unknown", String::as_str)
    );
    println!(
        "Baseline dir: {} ({})",
        resolved.baseline_dir.display(),
        resolved
            .metadata
            .get("baseline_dir_src")
            .map_or("unknown", String::as_str)
    );
    println!(
        "Save baseline: {} ({})",
        resolved.save_baseline,
        resolved
            .metadata
            .get("save_baseline_src")
            .map_or("unknown", String::as_str)
    );
    println!(
        "Threshold: {} ({})",
        resolved.config.threshold,
        resolved
            .metadata
            .get("threshold_src")
            .map_or("unknown", String::as_str)
    );
    println!(
        "Deny diagnostics: {} ({})",
        resolved
            .config
            .deny_diagnostics
            .map_or_else(|| "<none>".to_string(), |severity| severity.to_string()),
        resolved
            .metadata
            .get("deny_diagnostics_src")
            .map_or("unknown", String::as_str)
    );
    println!(
        "Names: {} ({})",
        resolved.config.console_names,
        resolved
            .metadata
            .get("console_names_src")
            .map_or("unknown", String::as_str)
    );
    println!("Progress: {}", resolved.config.progress);
}

fn source_for<F>(get_var: &F, env_key: &'static str) -> String
where
    F: Fn(&str) -> Option<String>,
{
    if get_var(env_key).is_some() {
        format!("env {env_key}")
    } else {
        "default".to_string()
    }
}

fn default_baseline_dir() -> PathBuf {
    PathBuf::from("target/stress/baselines")
}

fn resolve_baseline_path(
    path: &std::path::Path,
    baseline_dir: &std::path::Path,
    suite: &str,
    artifact_namespace: Option<&str>,
) -> PathBuf {
    if path.as_os_str() == "latest" {
        baseline_path(baseline_dir, artifact_namespace, "latest", suite)
    } else {
        path.to_path_buf()
    }
}

fn baseline_aliases_output_latest(
    baseline: &std::path::Path,
    output_dir: &std::path::Path,
    suite: &str,
) -> bool {
    let output_latest = output_dir.join(suite).join("latest.json");
    match (
        std::fs::canonicalize(baseline),
        std::fs::canonicalize(&output_latest),
    ) {
        (Ok(baseline), Ok(output)) => baseline == output,
        _ => absolute_lexical_path(baseline) == absolute_lexical_path(&output_latest),
    }
}

fn absolute_lexical_path(path: &std::path::Path) -> PathBuf {
    let mut normalized = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().unwrap_or_default()
    };
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn save_baseline_artifacts(
    run: &StressRun,
    baseline_dir: &std::path::Path,
    artifact_namespace: Option<&str>,
) -> std::io::Result<()> {
    ensure_baseline_save_eligible(run)?;
    let timestamped = baseline_path(
        baseline_dir,
        artifact_namespace,
        &run.started_at,
        &run.suite,
    );
    let latest = baseline_path(baseline_dir, artifact_namespace, "latest", &run.suite);
    let json = serde_json::to_string_pretty(run).map_err(std::io::Error::other)?;

    if let Some(parent) = timestamped.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write(&timestamped, json.as_bytes())?;
    if let Some(parent) = latest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write(&latest, json.as_bytes())?;
    Ok(())
}

struct BaselineReporter {
    baseline_dir: PathBuf,
    artifact_namespace: Option<String>,
}

impl BaselineReporter {
    fn new(baseline_dir: impl Into<PathBuf>, artifact_namespace: Option<&str>) -> Self {
        Self {
            baseline_dir: baseline_dir.into(),
            artifact_namespace: artifact_namespace.map(str::to_string),
        }
    }
}

impl Reporter for BaselineReporter {
    fn suite_end(&self, run: &StressRun) -> std::io::Result<()> {
        if evaluate_run_gate(run) != RunGate::Passed {
            return Ok(());
        }
        save_baseline_artifacts(run, &self.baseline_dir, self.artifact_namespace.as_deref())
            .map_err(|error| {
                std::io::Error::new(error.kind(), format!("failed to save baseline: {error}"))
            })
    }
}

fn ensure_baseline_save_eligible(run: &StressRun) -> std::io::Result<()> {
    let gate = evaluate_run_gate(run);
    if gate != RunGate::Passed {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "baseline was not saved because the run gate is {gate:?}; fix the failed gate and rerun --save-baseline"
            ),
        ));
    }
    if run.meets_min_quality(QualityClass::Acceptable) {
        return Ok(());
    }

    let intended_gates = run
        .summaries
        .iter()
        .filter(|summary| summary.is_intended_gate())
        .map(|summary| {
            format!(
                "{} (quality={}, trust={})",
                summary.benchmark_id, summary.quality, summary.trust_class
            )
        })
        .collect::<Vec<_>>();
    let evidence = if intended_gates.is_empty() {
        "no intended gate rows were selected".to_string()
    } else {
        format!("intended gate rows: {}", intended_gates.join(", "))
    };
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!(
            "baseline was not saved: --save-baseline requires at least one intended gate and every intended gate must retain gate trust with acceptable-or-better quality; {evidence}. Collect stable evidence with enough measured samples, then rerun --save-baseline"
        ),
    ))
}

fn baseline_path(
    baseline_dir: &std::path::Path,
    artifact_namespace: Option<&str>,
    bucket: &str,
    suite: &str,
) -> PathBuf {
    let mut path = baseline_dir.to_path_buf();
    if let Some(namespace) = artifact_namespace {
        path.push(namespace);
    }
    path.join(bucket)
        .join(format!("{}.json", baseline_suite_name(suite)))
}

fn baseline_suite_name(suite: &str) -> String {
    suite.replace(['/', '\\'], "_")
}

fn matches_glob(text: &str, pattern: &str) -> bool {
    let pattern = pattern.to_lowercase();
    let text = text.to_lowercase();

    if pattern.contains('*') {
        let parts = pattern.split('*').collect::<Vec<_>>();
        let mut remaining = text.as_str();

        for (index, part) in parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }
            if index == 0 {
                if !remaining.starts_with(part) {
                    return false;
                }
                remaining = &remaining[part.len()..];
            } else if index == parts.len() - 1 && !pattern.ends_with('*') {
                return remaining.ends_with(part);
            } else if let Some(pos) = remaining.find(part) {
                remaining = &remaining[pos + part.len()..];
            } else {
                return false;
            }
        }
        true
    } else {
        text.contains(&pattern)
    }
}

/// Get a list of registered benchmark names.
#[must_use]
pub fn list_benchmarks() -> Vec<&'static str> {
    STRESS_BENCHMARKS.iter().map(|entry| entry.name).collect()
}

/// Get the number of registered benchmarks.
#[must_use]
#[allow(dead_code)]
pub fn benchmark_count() -> usize {
    STRESS_BENCHMARKS.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_suite_identity_normalizes_targets_without_mistaking_names_for_hashes() {
        assert_eq!(canonical_suite_name("storage_stress"), "storage-stress");
        assert_eq!(
            canonical_suite_name("storage_stress-0123456789abcdef"),
            "storage-stress-0123456789abcdef"
        );
        assert_eq!(
            suite_name_from_executable("storage_stress-0123456789abcdef"),
            "storage-stress"
        );
    }

    #[test]
    fn glob_matches_substring_and_wildcards() {
        assert!(matches_glob("foo_bar_baz", "bar"));
        assert!(matches_glob("foo_bar_baz", "foo*baz"));
        assert!(matches_glob("foo_bar_baz", "*bar*"));
        assert!(!matches_glob("foo_bar_baz", "qux*"));
    }

    #[allow(clippy::unnecessary_wraps)]
    fn noop(_: &mut StressContext) -> StressResult {
        Ok(())
    }

    fn benchmark_entry() -> BenchmarkEntry {
        BenchmarkEntry {
            name: "Header Parse",
            function_name: "parse_header",
            func: noop,
            ignored: false,
            module_path: "parser::hot_path",
            tier: 1,
            mode: BenchmarkModeKind::Micro,
            budgets: BenchmarkBudgets::default(),
            metadata: &[],
        }
    }

    #[test]
    fn workload_matching_uses_rust_function_and_qualified_names() {
        let entry = benchmark_entry();

        assert!(entry_matches_workload(&entry, "parse_header"));
        assert!(entry_matches_workload(&entry, "parser::hot_path"));
        assert!(entry_matches_workload(
            &entry,
            "parser::hot_path::parse_header"
        ));
        assert!(entry_matches_workload(
            &entry,
            "parser::hot_path::Header Parse"
        ));
        assert!(entry_matches_workload(&entry, "header parse"));
        assert!(!entry_matches_workload(&entry, "serializer"));
    }

    #[test]
    fn workload_suggestions_include_registered_candidates() {
        let entry = benchmark_entry();
        let suggestions = workload_suggestions_for_entries("parse_headr", [&entry]);

        assert!(suggestions.iter().any(|item| item == "parse_header"));
    }

    #[test]
    fn parse_args_uses_new_sample_names() {
        let args = vec![
            "stress-demo".to_string(),
            "--profile".to_string(),
            "release".to_string(),
            "--samples".to_string(),
            "4".to_string(),
            "--warmup-samples".to_string(),
            "2".to_string(),
            "--timeout-secs".to_string(),
            "30".to_string(),
        ];
        let parsed = StressBinaryArgs::parse_from_args(&args).expect("valid arguments");

        assert_eq!(parsed.profile, Some(RunProfile::Release));
        assert_eq!(parsed.samples, Some(4));
        assert_eq!(parsed.warmup_samples, Some(2));
        assert_eq!(parsed.timeout, Some(Duration::from_secs(30)));
    }

    #[test]
    fn stabilization_cli_controls_override_environment() {
        let args = vec![
            "stress-demo".to_string(),
            "--operations-per-sample".to_string(),
            "32".to_string(),
            "--sample-duration-ms".to_string(),
            "250".to_string(),
            "--micro-sample-duration-ms".to_string(),
            "15".to_string(),
        ];
        let parsed = StressBinaryArgs::parse_from_args(&args)
            .expect("positive stabilization controls should parse");
        let env = BTreeMap::from([
            ("STRESS_OPERATIONS_PER_SAMPLE", "2".to_string()),
            ("STRESS_SAMPLE_DURATION_MS", "500".to_string()),
            ("STRESS_MICRO_SAMPLE_DURATION_MS", "25".to_string()),
        ]);

        let resolved = resolve_from_binary_args_with(&parsed, |key| env.get(key).cloned());

        assert_eq!(resolved.config.operations_per_sample, 32);
        assert_eq!(resolved.config.sample_duration, Duration::from_millis(250));
        assert_eq!(
            resolved.config.micro_sample_duration,
            Duration::from_millis(15)
        );
        for (key, source) in [
            ("operations_per_sample_src", "cli --operations-per-sample"),
            ("sample_duration_src", "cli --sample-duration-ms"),
            (
                "micro_sample_duration_src",
                "cli --micro-sample-duration-ms",
            ),
        ] {
            assert_eq!(resolved.metadata.get(key).map(String::as_str), Some(source));
        }
    }

    #[test]
    fn malformed_policy_environment_is_fatal_instead_of_falling_back() {
        let resolved = resolve_from_binary_args_with(&StressBinaryArgs::default(), |key| {
            (key == "STRESS_PROFILE").then(|| "relese".to_string())
        });

        assert_eq!(resolved.config.profile, RunProfile::Default);
        assert_eq!(
            environment_validation_error(&resolved.warnings).as_deref(),
            Some(
                "Invalid stress environment: invalid STRESS_PROFILE; expected default, smoke, lab, or release"
            )
        );
    }

    #[test]
    fn parse_args_accepts_gate_name_progress_and_baseline_flags() {
        let args = vec![
            "stress-demo".to_string(),
            "--fail-on-issues".to_string(),
            "--deny-diagnostics".to_string(),
            "error".to_string(),
            "--names".to_string(),
            "full".to_string(),
            "--no-progress".to_string(),
            "--baseline".to_string(),
            "latest".to_string(),
            "--baseline-dir".to_string(),
            "target/custom-baselines".to_string(),
            "--save-baseline".to_string(),
        ];

        let parsed = StressBinaryArgs::parse_from_args(&args).expect("valid arguments");

        assert_eq!(parsed.fail_on_issues, Some(true));
        assert_eq!(parsed.deny_diagnostics, Some(DiagnosticSeverity::Error));
        assert_eq!(parsed.names, Some(ConsoleNameMode::Full));
        assert_eq!(parsed.no_progress, Some(true));
        assert_eq!(parsed.baseline, Some(PathBuf::from("latest")));
        assert_eq!(
            parsed.baseline_dir,
            Some(PathBuf::from("target/custom-baselines"))
        );
        assert_eq!(parsed.save_baseline, Some(true));
    }

    #[test]
    fn parse_args_rejects_unknown_flags_and_positionals() {
        for unknown in ["--profle", "--nocapture", "parse_header"] {
            let args = vec!["stress-demo".to_string(), unknown.to_string()];
            let error = StressBinaryArgs::parse_from_args(&args)
                .expect_err("unknown arguments must be rejected");

            assert!(error.contains(unknown), "unexpected error: {error}");
        }
    }

    #[test]
    fn parse_args_rejects_missing_flag_values() {
        for args in [
            vec!["stress-demo".to_string(), "--samples".to_string()],
            vec![
                "stress-demo".to_string(),
                "--samples".to_string(),
                "--json".to_string(),
            ],
        ] {
            let error = StressBinaryArgs::parse_from_args(&args)
                .expect_err("missing values must be rejected");

            assert!(error.contains("--samples"), "unexpected error: {error}");
        }
    }

    #[test]
    fn parse_args_rejects_duplicate_singletons_and_aliases() {
        for tail in [
            vec!["--profile", "release", "--profile", "smoke"],
            vec!["--workload", "alpha", "--filter", "beta"],
            vec!["--threshold", "0.05", "--threshold", "0.10"],
            vec!["--sample-duration-ms", "10", "--sample-duration-ms", "20"],
            vec!["--json", "--json"],
            vec!["--include-ignored", "--include-ignored"],
            vec!["--print-config", "--dry-run-config"],
        ] {
            let args = std::iter::once("stress-demo".to_string())
                .chain(tail.into_iter().map(str::to_string))
                .collect::<Vec<_>>();
            let error = StressBinaryArgs::parse_from_args(&args)
                .expect_err("singleton flags must not use last-value-wins parsing");

            assert!(error.contains("only once"), "unexpected error: {error}");
        }

        let cargo_markers = vec![
            "stress-demo".to_string(),
            "--bench".to_string(),
            "--bench".to_string(),
        ];
        StressBinaryArgs::parse_from_args(&cargo_markers)
            .expect("Cargo's libtest-compatible marker remains exempt");
    }

    #[test]
    fn parse_args_rejects_malformed_typed_values() {
        for (flag, value) in [
            ("--profile", "relase"),
            ("--tier", "three"),
            ("--samples", "many"),
            ("--warmup-samples", "some"),
            ("--cooldown-samples", "some"),
            ("--timeout-secs", "0"),
            ("--timeout-secs", "later"),
            ("--operations-per-sample", "0"),
            ("--operations-per-sample", "many"),
            ("--sample-duration-ms", "0"),
            ("--sample-duration-ms", "later"),
            ("--micro-sample-duration-ms", "0"),
            ("--micro-sample-duration-ms", "later"),
            ("--threshold", "five"),
            ("--threshold", "NaN"),
            ("--threshold", "inf"),
            ("--threshold", "1.01"),
            ("--deny-diagnostics", "severe"),
            ("--names", "short"),
        ] {
            let args = vec![
                "stress-demo".to_string(),
                flag.to_string(),
                value.to_string(),
            ];
            let error = StressBinaryArgs::parse_from_args(&args)
                .expect_err("malformed values must be rejected");

            assert!(error.contains(flag), "unexpected error: {error}");
            assert!(error.contains(value), "unexpected error: {error}");
        }
    }

    #[test]
    fn parse_args_accepts_cargo_bench_marker_only() {
        let args = vec![
            "stress-demo".to_string(),
            "--profile".to_string(),
            "release".to_string(),
            "--bench".to_string(),
        ];

        let parsed = StressBinaryArgs::parse_from_args(&args).expect("cargo bench arguments");

        assert_eq!(parsed.profile, Some(RunProfile::Release));
    }

    #[test]
    fn resolved_workload_is_consumed_by_glob_selection_only() {
        let args = StressBinaryArgs {
            workload: Some("parser::*".to_string()),
            ..StressBinaryArgs::default()
        };

        let resolved = resolve_from_binary_args_with(&args, |_| None);

        assert_eq!(resolved.workload.as_deref(), Some("parser::*"));
        assert!(
            resolved.config.filter.is_none(),
            "the runner must not reapply the glob as a literal filter"
        );

        let resolved = resolve_from_binary_args_with(&StressBinaryArgs::default(), |key| {
            (key == "STRESS_FILTER").then(|| "parser::*".to_string())
        });

        assert_eq!(resolved.workload.as_deref(), Some("parser::*"));
        assert!(resolved.config.filter.is_none());
    }

    #[test]
    fn cli_timeout_overrides_environment_timeout() {
        let args = StressBinaryArgs {
            timeout: Some(Duration::from_secs(3)),
            ..StressBinaryArgs::default()
        };
        let resolved = resolve_from_binary_args_with(&args, |key| {
            (key == "STRESS_TIMEOUT_SECS").then(|| "20".to_string())
        });

        assert_eq!(resolved.config.timeout, Some(Duration::from_secs(3)));
        assert_eq!(
            resolved.metadata.get("timeout_secs_src"),
            Some(&"cli --timeout-secs".to_string())
        );
    }

    #[test]
    fn benchmark_deadline_returns_a_typed_timeout() {
        #[allow(clippy::unnecessary_wraps)]
        fn slow(ctx: &mut StressContext) -> StressResult {
            ctx.measure("slow", || std::thread::sleep(Duration::from_millis(20)));
            Ok(())
        }

        let config = StressRunnerConfig::new()
            .samples(1)
            .warmup_samples(0)
            .cooldown_samples(0)
            .operations_per_sample(1)
            .progress(false);
        let runner = StressRunner::with_config("timeout-suite", config);
        let spec = BenchmarkSpec {
            id: "timeout-suite/slow".to_string(),
            name: "slow".to_string(),
            tier: 2,
            mode: crate::artifact::BenchmarkMode::FixedOperations {
                operations_per_sample: 1,
            },
            intent: crate::artifact::MeasurementIntent::General,
            budgets: BenchmarkBudgets::default(),
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
        };

        let Err(error) = run_spec_with_timeout(runner, spec, slow, Duration::from_millis(1)) else {
            panic!("slow benchmark should time out");
        };

        assert!(matches!(error, SpecRunError::Timeout { .. }));
        assert_eq!(error.exit_code(), 124);
    }

    #[test]
    fn resolve_from_binary_args_uses_stress_env() {
        let args = StressBinaryArgs::default();
        let env = BTreeMap::from([
            ("STRESS_PROFILE", "release".to_string()),
            ("STRESS_SAMPLES", "3".to_string()),
            ("STRESS_WARMUP_SAMPLES", "1".to_string()),
            ("STRESS_INCLUDE_IGNORED", "true".to_string()),
        ]);

        let resolved = resolve_from_binary_args_with(&args, |key| env.get(key).cloned());

        assert_eq!(resolved.config.profile, RunProfile::Release);
        assert_eq!(resolved.config.samples, 3);
        assert_eq!(resolved.config.warmup_samples, 1);
        assert!(resolved.include_ignored);
    }

    #[test]
    fn resolve_from_binary_args_uses_new_env_controls() {
        let args = StressBinaryArgs::default();
        let env = BTreeMap::from([
            ("STRESS_FAIL_ON_ISSUES", "true".to_string()),
            ("STRESS_DENY_DIAGNOSTICS", "error".to_string()),
            ("STRESS_CONSOLE_NAMES", "full".to_string()),
            ("STRESS_PROGRESS", "false".to_string()),
            ("STRESS_BASELINE_DIR", "target/env-baselines".to_string()),
            ("STRESS_SAVE_BASELINE", "true".to_string()),
            ("STRESS_ARTIFACT_NAMESPACE", "package:a".to_string()),
        ]);

        let resolved = resolve_from_binary_args_with(&args, |key| env.get(key).cloned());

        assert_eq!(
            resolved.config.deny_diagnostics,
            Some(DiagnosticSeverity::Error)
        );
        assert_eq!(resolved.config.console_names, ConsoleNameMode::Full);
        assert!(!resolved.config.progress);
        assert_eq!(resolved.baseline_dir, PathBuf::from("target/env-baselines"));
        assert!(resolved.save_baseline);
        assert_eq!(resolved.artifact_namespace.as_deref(), Some("package-a"));
        assert_eq!(
            resolved.config.output_dir,
            PathBuf::from("target/stress/package-a")
        );
        assert_eq!(
            resolved.metadata.get("deny_diagnostics_src"),
            Some(&"env STRESS_DENY_DIAGNOSTICS".to_string())
        );
    }

    #[test]
    fn cli_overrides_env() {
        let args = StressBinaryArgs {
            profile: Some(RunProfile::Lab),
            samples: Some(5),
            fail_on_issues: Some(true),
            names: Some(ConsoleNameMode::Compact),
            no_progress: Some(true),
            ..StressBinaryArgs::default()
        };
        let env = BTreeMap::from([
            ("STRESS_PROFILE", "release".to_string()),
            ("STRESS_SAMPLES", "3".to_string()),
            ("STRESS_DENY_DIAGNOSTICS", "error".to_string()),
            ("STRESS_CONSOLE_NAMES", "full".to_string()),
            ("STRESS_PROGRESS", "true".to_string()),
        ]);

        let resolved = resolve_from_binary_args_with(&args, |key| env.get(key).cloned());

        assert_eq!(resolved.config.profile, RunProfile::Lab);
        assert_eq!(resolved.config.samples, 5);
        assert_eq!(
            resolved.metadata.get("samples_src"),
            Some(&"cli --samples".to_string())
        );
        assert_eq!(
            resolved.config.deny_diagnostics,
            Some(DiagnosticSeverity::Warning)
        );
        assert_eq!(resolved.config.console_names, ConsoleNameMode::Compact);
        assert!(!resolved.config.progress);
    }

    #[test]
    fn cli_profile_is_base_before_env_overrides() {
        let args = StressBinaryArgs {
            profile: Some(RunProfile::Lab),
            ..StressBinaryArgs::default()
        };
        let env = BTreeMap::from([
            ("STRESS_PROFILE", "release".to_string()),
            ("STRESS_SAMPLES", "3".to_string()),
            ("STRESS_THRESHOLD", "0.20".to_string()),
        ]);

        let resolved = resolve_from_binary_args_with(&args, |key| env.get(key).cloned());

        assert_eq!(resolved.config.profile, RunProfile::Lab);
        assert_eq!(resolved.config.samples, 3);
        assert!((resolved.config.threshold - 0.20).abs() < f64::EPSILON);
        assert_eq!(
            resolved.metadata.get("profile_src"),
            Some(&"cli --profile".to_string())
        );
        assert_eq!(
            resolved.metadata.get("samples_src"),
            Some(&"env STRESS_SAMPLES".to_string())
        );
        assert_eq!(
            resolved.metadata.get("threshold_src"),
            Some(&"env STRESS_THRESHOLD".to_string())
        );
    }

    #[test]
    fn empty_workload_selection_has_fatal_error_message() {
        let args = StressBinaryArgs {
            workload: Some("definitely_no_such_benchmark".to_string()),
            ..StressBinaryArgs::default()
        };
        let resolved = resolve_from_binary_args_with(&args, |_| None);

        let error = required_benchmarks(&resolved)
            .err()
            .expect("an unmatched workload must be fatal");

        assert_eq!(
            error,
            "No benchmarks matched workload pattern 'definitely_no_such_benchmark'"
        );
    }

    #[test]
    fn baseline_latest_resolves_under_baseline_dir() {
        let baseline_dir = PathBuf::from("target/stress/baselines");

        assert_eq!(
            resolve_baseline_path(
                std::path::Path::new("latest"),
                &baseline_dir,
                "suite-name",
                None,
            ),
            baseline_dir.join("latest").join("suite-name.json")
        );
        assert_eq!(
            resolve_baseline_path(
                std::path::Path::new("latest"),
                &baseline_dir,
                "suite-name",
                Some("package-a"),
            ),
            baseline_dir
                .join("package-a")
                .join("latest")
                .join("suite-name.json")
        );
        assert_eq!(
            resolve_baseline_path(
                std::path::Path::new("target/explicit/latest.json"),
                &baseline_dir,
                "suite",
                Some("package-a"),
            ),
            PathBuf::from("target/explicit/latest.json")
        );
    }

    #[test]
    fn explicit_baseline_cannot_alias_the_current_output_latest_artifact() {
        let root = unique_temp_dir("stress-baseline-self-alias");
        let output_dir = root.join("output");
        let output_latest = output_dir.join("suite-name/latest.json");
        std::fs::create_dir_all(output_latest.parent().expect("artifact parent"))
            .expect("create artifact parent");
        std::fs::write(&output_latest, "{}").expect("write artifact placeholder");

        assert!(baseline_aliases_output_latest(
            &output_latest,
            &output_dir,
            "suite-name"
        ));
        assert!(!baseline_aliases_output_latest(
            &root.join("baselines/latest/suite-name.json"),
            &output_dir,
            "suite-name"
        ));

        let _ = std::fs::remove_dir_all(root);
    }

    fn eligible_baseline_run(suite: &str) -> StressRun {
        let config = StressRunnerConfig::for_profile(RunProfile::Default)
            .samples(10)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config(suite, config);
        runner.reporters(Vec::new());
        runner.run("bench", |ctx| {
            ctx.record_external("work", Duration::from_millis(10), 500);
        });
        let run = runner.finish();
        assert!(run.meets_min_quality(QualityClass::Acceptable));
        run
    }

    #[test]
    fn save_baseline_writes_timestamped_and_latest_paths() {
        let baseline_dir = unique_temp_dir("stress-baseline-save");
        let run = eligible_baseline_run("suite-name");

        save_baseline_artifacts(&run, &baseline_dir, None).expect("save baseline");

        let timestamped = baseline_path(&baseline_dir, None, &run.started_at, &run.suite);
        let latest = baseline_path(&baseline_dir, None, "latest", &run.suite);
        assert!(timestamped.exists());
        assert!(latest.exists());
        let latest_run = StressRun::load(&latest).expect("latest baseline");
        assert_eq!(latest_run.suite, "suite-name");

        let _ = std::fs::remove_dir_all(&baseline_dir);
    }

    #[test]
    fn save_baseline_rejects_ineligible_evidence_without_touching_existing_paths() {
        let baseline_dir = unique_temp_dir("stress-baseline-ineligible");
        let config = StressRunnerConfig::for_profile(RunProfile::Smoke)
            .samples(1)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite-name", config);
        runner.reporters(Vec::new());
        runner.run("bench", |ctx| {
            ctx.record_external("work", Duration::from_millis(10), 1);
        });
        let run = runner.finish();
        assert_eq!(evaluate_run_gate(&run), RunGate::Passed);
        assert!(!run.meets_min_quality(crate::artifact::QualityClass::Acceptable));

        let latest = baseline_path(&baseline_dir, None, "latest", &run.suite);
        std::fs::create_dir_all(latest.parent().expect("latest parent"))
            .expect("create latest parent");
        std::fs::write(&latest, b"accepted previous baseline").expect("write previous latest");

        let error = save_baseline_artifacts(&run, &baseline_dir, None)
            .expect_err("smoke-quality evidence cannot become a baseline");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("acceptable"));
        assert_eq!(
            std::fs::read(&latest).expect("read unchanged latest"),
            b"accepted previous baseline"
        );
        assert!(!baseline_path(&baseline_dir, None, &run.started_at, &run.suite).exists());

        let _ = std::fs::remove_dir_all(&baseline_dir);
    }

    #[test]
    fn baseline_reporter_attaches_save_failures_before_finish_returns() {
        let blocking_baseline_dir = unique_temp_dir("stress-baseline-reporter-failure");
        std::fs::write(&blocking_baseline_dir, b"not a directory")
            .expect("create blocking baseline path");
        let config = StressRunnerConfig::for_profile(RunProfile::Default)
            .samples(10)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite-name", config);
        runner.reporters(vec![Box::new(BaselineReporter::new(
            &blocking_baseline_dir,
            None,
        ))]);
        runner.run("bench", |ctx| {
            ctx.record_external("work", Duration::from_millis(10), 500);
        });

        let run = runner.finish();

        assert_eq!(evaluate_run_gate(&run), RunGate::ArtifactFailed);
        assert!(run
            .metadata
            .get("reporter_errors")
            .is_some_and(|error| error.contains("failed to save baseline")));
        let _ = std::fs::remove_file(blocking_baseline_dir);
    }

    #[test]
    fn baseline_reporter_does_not_mask_an_earlier_run_gate() {
        let baseline_dir = unique_temp_dir("stress-baseline-reporter-skip");
        let config = StressRunnerConfig::for_profile(RunProfile::Default)
            .samples(2)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config("suite-name", config);
        runner.reporters(vec![Box::new(BaselineReporter::new(&baseline_dir, None))]);
        runner.run("bench", |ctx| {
            ctx.measure("work", || {});
            let _ = ctx.correctness().attempted(1).completed(0).failures(1);
        });

        let run = runner.finish();

        assert_eq!(evaluate_run_gate(&run), RunGate::CorrectnessFailed);
        assert!(!run.metadata.contains_key("reporter_errors"));
        assert!(!baseline_dir.exists());
    }

    #[test]
    fn package_namespaces_isolate_same_suite_baseline_save_and_latest_read_paths() {
        let baseline_dir = unique_temp_dir("stress-baseline-package-isolation");
        let run = eligible_baseline_run("shared-suite");
        let mut alpha_run = run.clone();
        alpha_run
            .metadata
            .insert("fixture_package".to_string(), "alpha".to_string());
        let mut beta_run = run;
        beta_run
            .metadata
            .insert("fixture_package".to_string(), "beta".to_string());

        save_baseline_artifacts(&alpha_run, &baseline_dir, Some("alpha"))
            .expect("save alpha baseline");
        save_baseline_artifacts(&beta_run, &baseline_dir, Some("beta"))
            .expect("save beta baseline");

        let alpha_latest = resolve_baseline_path(
            std::path::Path::new("latest"),
            &baseline_dir,
            &alpha_run.suite,
            Some("alpha"),
        );
        let beta_latest = resolve_baseline_path(
            std::path::Path::new("latest"),
            &baseline_dir,
            &beta_run.suite,
            Some("beta"),
        );
        assert_ne!(alpha_latest, beta_latest);
        assert_eq!(
            alpha_latest,
            baseline_dir
                .join("alpha")
                .join("latest")
                .join("shared-suite.json")
        );
        assert_eq!(
            beta_latest,
            baseline_dir
                .join("beta")
                .join("latest")
                .join("shared-suite.json")
        );

        let loaded_alpha = StressRun::load(alpha_latest).expect("load alpha latest baseline");
        let loaded_beta = StressRun::load(beta_latest).expect("load beta latest baseline");
        assert_eq!(loaded_alpha.metadata["fixture_package"], "alpha");
        assert_eq!(loaded_beta.metadata["fixture_package"], "beta");

        let _ = std::fs::remove_dir_all(&baseline_dir);
    }

    #[test]
    fn programmatic_options_use_percentage_points_and_all_sample_phases() {
        let args = binary_args_from_options(
            StressRunnerOptions::new()
                .samples(7)
                .warmup_samples(2)
                .cooldown_samples(1)
                .threshold_percent(5.0)
                .output_dir("target/custom-stress"),
        );

        assert_eq!(args.samples, Some(7));
        assert_eq!(args.warmup_samples, Some(2));
        assert_eq!(args.cooldown_samples, Some(1));
        assert!(args
            .threshold
            .is_some_and(|threshold| (threshold - 0.05).abs() < f64::EPSILON));
        assert_eq!(args.output_dir, Some(PathBuf::from("target/custom-stress")));
    }

    #[test]
    fn programmatic_false_booleans_override_true_environment_values() {
        let args = binary_args_from_options(
            StressRunnerOptions::new()
                .json_stdout(false)
                .save_baseline(false)
                .include_ignored(false),
        );
        assert_eq!(args.json_stdout, Some(false));
        assert_eq!(args.save_baseline, Some(false));
        assert_eq!(args.include_ignored, Some(false));

        let env = BTreeMap::from([
            ("STRESS_JSON", "true".to_string()),
            ("STRESS_SAVE_BASELINE", "true".to_string()),
            ("STRESS_INCLUDE_IGNORED", "true".to_string()),
        ]);
        let resolved = resolve_from_binary_args_with(&args, |key| env.get(key).cloned());

        assert!(!resolved.config.json_stdout);
        assert!(!resolved.save_baseline);
        assert!(!resolved.include_ignored);
    }

    #[test]
    fn programmatic_boolean_defaults_preserve_environment_fallback() {
        let args = binary_args_from_options(StressRunnerOptions::new());
        assert_eq!(args.json_stdout, None);
        assert_eq!(args.save_baseline, None);
        assert_eq!(args.include_ignored, None);

        let env = BTreeMap::from([
            ("STRESS_JSON", "true".to_string()),
            ("STRESS_SAVE_BASELINE", "true".to_string()),
            ("STRESS_INCLUDE_IGNORED", "true".to_string()),
        ]);
        let resolved = resolve_from_binary_args_with(&args, |key| env.get(key).cloned());

        assert!(resolved.config.json_stdout);
        assert!(resolved.save_baseline);
        assert!(resolved.include_ignored);
    }

    #[test]
    #[should_panic(expected = "regression threshold percent must be between 0 and 100")]
    fn programmatic_threshold_percent_rejects_fraction_confusion() {
        let _ = StressRunnerOptions::new().threshold_percent(500.0);
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
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
}
