//! Configuration and profile resolution for stress runs.

use crate::artifact::{
    BenchmarkMode, BenchmarkModeKind, ConsoleNameMode, DiagnosticSeverity, ProfileConfig,
    QualityClass, RunProfile, MAX_TIER,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub(crate) struct EnvConfigResolution {
    pub config: StressRunnerConfig,
    pub metadata: HashMap<String, String>,
    pub warnings: Vec<String>,
}

impl EnvConfigResolution {
    fn new(config: StressRunnerConfig) -> Self {
        Self {
            config,
            metadata: HashMap::new(),
            warnings: Vec::new(),
        }
    }
}

/// Configuration for one stress suite run.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct StressRunnerConfig {
    /// Selected run profile.
    pub profile: RunProfile,
    /// Measured samples per benchmark.
    pub samples: usize,
    /// Warmup samples retained but excluded from summaries.
    pub warmup_samples: usize,
    /// Cooldown samples retained but excluded from summaries.
    pub cooldown_samples: usize,
    /// Output directory for artifacts.
    pub output_dir: PathBuf,
    /// Emit the current run JSON to stdout instead of the human console table.
    pub json_stdout: bool,
    /// Filter benchmarks by name/module pattern.
    pub filter: Option<String>,
    /// Exact tier filter.
    pub tier: Option<u32>,
    /// Git SHA to include in artifacts.
    pub git_sha: Option<String>,
    /// Per-benchmark deadline enforced by the generated `stress_main!` harness.
    pub timeout: Option<Duration>,
    /// Fixed-duration sample budget.
    pub sample_duration: Duration,
    /// Fixed-operations sample size.
    pub operations_per_sample: u64,
    /// Target duration for calibrated micro samples.
    pub micro_sample_duration: Duration,
    /// Minimum quality required by the active profile.
    pub min_quality: QualityClass,
    /// Whether quality below `min_quality` fails the run.
    pub fail_on_quality: bool,
    /// Whether meaningful regressions fail the run.
    pub fail_on_regression: bool,
    /// Optional strict diagnostic gate threshold.
    pub deny_diagnostics: Option<DiagnosticSeverity>,
    /// Regression/improvement threshold as a fraction (`0.05` means 5%).
    pub threshold: f64,
    /// Human-readable report depth label.
    pub report_depth: String,
    /// Human console benchmark-name mode.
    pub console_names: ConsoleNameMode,
    /// Whether human runs emit stderr progress.
    pub progress: bool,
}

impl Default for StressRunnerConfig {
    fn default() -> Self {
        Self::for_profile(RunProfile::default())
    }
}

impl StressRunnerConfig {
    /// Create a default trustworthy config.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a config initialized from a profile.
    #[must_use]
    pub fn for_profile(profile: RunProfile) -> Self {
        let profile_config = match profile {
            RunProfile::Default => ProfileConfig {
                profile,
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
            },
            RunProfile::Smoke => ProfileConfig {
                profile,
                measured_samples: 1,
                warmup_samples: 0,
                cooldown_samples: 0,
                min_quality: QualityClass::Untrustworthy,
                fail_on_quality: false,
                fail_on_regression: false,
                deny_diagnostics: None,
                regression_threshold: 0.05,
                sample_duration: Duration::from_millis(10),
                operations_per_sample: 1,
                micro_sample_duration: Duration::from_millis(5),
                report_depth: "summary".to_string(),
                console_names: ConsoleNameMode::Compact,
                progress: true,
            },
            RunProfile::Release => ProfileConfig {
                profile,
                measured_samples: 10,
                warmup_samples: 1,
                cooldown_samples: 0,
                min_quality: QualityClass::Acceptable,
                fail_on_quality: true,
                fail_on_regression: true,
                deny_diagnostics: None,
                regression_threshold: 0.05,
                sample_duration: Duration::from_secs(1),
                operations_per_sample: 1,
                micro_sample_duration: Duration::from_millis(100),
                report_depth: "gated".to_string(),
                console_names: ConsoleNameMode::Compact,
                progress: true,
            },
            RunProfile::Lab => ProfileConfig {
                profile,
                measured_samples: 30,
                warmup_samples: 2,
                cooldown_samples: 1,
                min_quality: QualityClass::Noisy,
                fail_on_quality: false,
                fail_on_regression: false,
                deny_diagnostics: None,
                regression_threshold: 0.05,
                sample_duration: Duration::from_secs(5),
                operations_per_sample: 1,
                micro_sample_duration: Duration::from_millis(200),
                report_depth: "deep".to_string(),
                console_names: ConsoleNameMode::Compact,
                progress: true,
            },
        };
        Self::from_profile_config(profile_config)
    }

    fn from_profile_config(profile_config: ProfileConfig) -> Self {
        Self {
            profile: profile_config.profile,
            samples: profile_config.measured_samples,
            warmup_samples: profile_config.warmup_samples,
            cooldown_samples: profile_config.cooldown_samples,
            output_dir: PathBuf::from("target/stress"),
            json_stdout: false,
            filter: None,
            tier: None,
            git_sha: None,
            timeout: None,
            sample_duration: profile_config.sample_duration,
            operations_per_sample: profile_config.operations_per_sample,
            micro_sample_duration: profile_config.micro_sample_duration,
            min_quality: profile_config.min_quality,
            fail_on_quality: profile_config.fail_on_quality,
            fail_on_regression: profile_config.fail_on_regression,
            deny_diagnostics: profile_config.deny_diagnostics,
            threshold: profile_config.regression_threshold,
            report_depth: profile_config.report_depth,
            console_names: profile_config.console_names,
            progress: profile_config.progress,
        }
    }

    /// Resolve config from canonical `STRESS_*` environment variables.
    ///
    /// # Panics
    ///
    /// Panics when a configured environment value is malformed. Invalid
    /// policy input must not silently fall back to a weaker profile.
    #[must_use]
    pub fn from_env() -> Self {
        let resolution = Self::resolve_from_env();
        assert!(
            resolution.warnings.is_empty(),
            "invalid stress environment: {}",
            resolution.warnings.join("; ")
        );
        resolution.config
    }

    pub(crate) fn resolve_from_env() -> EnvConfigResolution {
        Self::resolve_from_env_with(|key| std::env::var(key).ok())
    }

    pub(crate) fn resolve_from_env_with<F>(get_var: F) -> EnvConfigResolution
    where
        F: Fn(&str) -> Option<String>,
    {
        Self::resolve_from_env_with_profile_override(get_var, None)
    }

    pub(crate) fn resolve_from_env_with_profile_override<F>(
        get_var: F,
        profile_override: Option<(RunProfile, &'static str)>,
    ) -> EnvConfigResolution
    where
        F: Fn(&str) -> Option<String>,
    {
        let (profile, profile_source, profile_warning) =
            resolve_profile(&get_var, "STRESS_PROFILE", profile_override);
        let mut resolution = EnvConfigResolution::new(Self::for_profile(profile));
        if let Some(warning) = profile_warning {
            resolution.warnings.push(warning);
        }
        resolution
            .metadata
            .insert("profile_src".to_string(), profile_source);
        apply_env_overrides(&get_var, &mut resolution);

        if resolution.config.git_sha.is_none() {
            resolution.config.git_sha = detect_git_sha();
            if resolution.config.git_sha.is_some() {
                resolution
                    .metadata
                    .insert("git_sha_src".to_string(), "auto_detect".to_string());
            }
        }

        apply_default_sources(&mut resolution.metadata);
        resolution
    }

    /// Return validation errors that would make a run invalid or perform no useful work.
    #[must_use]
    pub fn validation_errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.samples == 0 {
            errors.push("samples must be greater than 0".to_string());
        }
        if self.operations_per_sample == 0 {
            errors.push("operations_per_sample must be greater than 0".to_string());
        }
        if self.sample_duration.is_zero() {
            errors.push("sample_duration must be greater than 0".to_string());
        }
        if self.micro_sample_duration.is_zero() {
            errors.push("micro_sample_duration must be greater than 0".to_string());
        }
        if self.timeout.is_some_and(|timeout| timeout.is_zero()) {
            errors.push("timeout must be greater than 0 when configured".to_string());
        }
        if self.tier.is_some_and(|tier| tier == 0 || tier > MAX_TIER) {
            errors.push(format!("tier must be between 1 and {MAX_TIER}"));
        }
        if self
            .filter
            .as_deref()
            .is_some_and(|filter| filter.trim().is_empty())
        {
            errors.push("filter must not be empty".to_string());
        }
        if !self.threshold.is_finite() || !(0.0..=1.0).contains(&self.threshold) {
            errors.push("threshold must be finite and between 0 and 1".to_string());
        }
        errors
    }

    /// Return the profile config embedded in artifacts.
    #[must_use]
    pub fn profile_config(&self) -> ProfileConfig {
        ProfileConfig {
            profile: self.profile,
            measured_samples: self.samples,
            warmup_samples: self.warmup_samples,
            cooldown_samples: self.cooldown_samples,
            min_quality: self.min_quality,
            fail_on_quality: self.fail_on_quality,
            fail_on_regression: self.fail_on_regression,
            deny_diagnostics: self.deny_diagnostics,
            regression_threshold: self.threshold,
            sample_duration: self.sample_duration,
            operations_per_sample: self.operations_per_sample,
            micro_sample_duration: self.micro_sample_duration,
            report_depth: self.report_depth.clone(),
            console_names: self.console_names,
            progress: self.progress,
        }
    }

    /// Build a concrete mode from a static mode kind.
    #[must_use]
    pub fn mode_for_kind(&self, kind: BenchmarkModeKind) -> BenchmarkMode {
        match kind {
            BenchmarkModeKind::Micro => BenchmarkMode::Micro {
                target_sample_duration: self.micro_sample_duration,
            },
            BenchmarkModeKind::FixedDuration => BenchmarkMode::FixedDuration {
                sample_duration: self.sample_duration,
            },
            BenchmarkModeKind::FixedOperations => BenchmarkMode::FixedOperations {
                operations_per_sample: self.operations_per_sample,
            },
        }
    }

    /// Build the concrete mode implied by a tier.
    #[must_use]
    pub fn mode_for_tier(&self, tier: u32) -> Option<BenchmarkMode> {
        BenchmarkModeKind::for_tier(tier).map(|kind| self.mode_for_kind(kind))
    }

    /// Set profile and profile-derived defaults.
    #[must_use]
    pub fn profile(self, profile: RunProfile) -> Self {
        let mut next = Self::for_profile(profile);
        next.output_dir = self.output_dir;
        next.json_stdout = self.json_stdout;
        next.filter = self.filter;
        next.tier = self.tier;
        next.git_sha = self.git_sha;
        next.timeout = self.timeout;
        next.deny_diagnostics = self.deny_diagnostics;
        next.console_names = self.console_names;
        next.progress = self.progress;
        next
    }

    /// Set measured sample count.
    #[must_use]
    pub fn samples(mut self, value: usize) -> Self {
        self.samples = value;
        self
    }

    /// Set warmup sample count.
    #[must_use]
    pub fn warmup_samples(mut self, value: usize) -> Self {
        self.warmup_samples = value;
        self
    }

    /// Set cooldown sample count.
    #[must_use]
    pub fn cooldown_samples(mut self, value: usize) -> Self {
        self.cooldown_samples = value;
        self
    }

    /// Set output directory.
    #[must_use]
    pub fn output_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.output_dir = path.into();
        self
    }

    /// Emit machine-readable JSON to stdout instead of the human console table.
    #[must_use]
    pub const fn json_stdout(mut self, value: bool) -> Self {
        self.json_stdout = value;
        self
    }

    /// Set name/module filter.
    #[must_use]
    pub fn filter(mut self, pattern: impl Into<String>) -> Self {
        self.filter = Some(pattern.into());
        self
    }

    /// Clear name/module filter.
    #[must_use]
    pub fn no_filter(mut self) -> Self {
        self.filter = None;
        self
    }

    /// Set exact tier filter.
    #[must_use]
    pub fn tier(mut self, tier: u32) -> Self {
        self.tier = Some(tier);
        self
    }

    /// Clear tier filter.
    #[must_use]
    pub fn no_tier(mut self) -> Self {
        self.tier = None;
        self
    }

    /// Set git SHA.
    #[must_use]
    pub fn git_sha(mut self, sha: impl Into<String>) -> Self {
        self.git_sha = Some(sha.into());
        self
    }

    /// Set the deadline enforced by the generated `stress_main!` harness.
    #[must_use]
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(duration);
        self
    }

    /// Set fixed-duration sample budget.
    #[must_use]
    pub fn sample_duration(mut self, duration: Duration) -> Self {
        self.sample_duration = duration;
        self
    }

    /// Set fixed-operations sample size.
    #[must_use]
    pub fn operations_per_sample(mut self, value: u64) -> Self {
        self.operations_per_sample = value;
        self
    }

    /// Set calibrated micro sample target duration.
    #[must_use]
    pub fn micro_sample_duration(mut self, duration: Duration) -> Self {
        self.micro_sample_duration = duration;
        self
    }

    /// Set the regression threshold as a fraction (`0.05` means 5%).
    ///
    /// # Panics
    ///
    /// Panics unless `value` is finite and between 0 and 1.
    #[must_use]
    pub fn threshold_fraction(mut self, value: f64) -> Self {
        assert!(
            value.is_finite() && (0.0..=1.0).contains(&value),
            "regression threshold fraction must be between 0 and 1"
        );
        self.threshold = value;
        self
    }

    /// Set the regression threshold in percentage points (`5.0` means 5%).
    ///
    /// # Panics
    ///
    /// Panics unless `value` is finite and between 0 and 100.
    #[must_use]
    pub fn threshold_percent(mut self, value: f64) -> Self {
        assert!(
            value.is_finite() && (0.0..=100.0).contains(&value),
            "regression threshold percent must be between 0 and 100"
        );
        self.threshold = value / 100.0;
        self
    }

    /// Set strict diagnostic gate threshold.
    #[must_use]
    pub const fn deny_diagnostics(mut self, threshold: DiagnosticSeverity) -> Self {
        self.deny_diagnostics = Some(threshold);
        self
    }

    /// Clear strict diagnostic gating.
    #[must_use]
    pub const fn allow_diagnostics(mut self) -> Self {
        self.deny_diagnostics = None;
        self
    }

    /// Alias for denying warning-or-higher diagnostics.
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
    pub const fn console_names(mut self, mode: ConsoleNameMode) -> Self {
        self.console_names = mode;
        self
    }

    /// Set whether human runs emit stderr progress.
    #[must_use]
    pub const fn progress(mut self, value: bool) -> Self {
        self.progress = value;
        self
    }
}

fn resolve_profile<F>(
    get_var: &F,
    env_key: &'static str,
    override_profile: Option<(RunProfile, &'static str)>,
) -> (RunProfile, String, Option<String>)
where
    F: Fn(&str) -> Option<String>,
{
    if let Some((profile, source)) = override_profile {
        return (profile, source.to_string(), None);
    }

    match get_var(env_key) {
        Some(value) => match value.parse::<RunProfile>() {
            Ok(profile) => (profile, format!("env {env_key}"), None),
            Err(_) => (
                RunProfile::default(),
                "default".to_string(),
                Some(
                    "invalid STRESS_PROFILE; expected default, smoke, lab, or release".to_string(),
                ),
            ),
        },
        None => (RunProfile::default(), "default".to_string(), None),
    }
}

fn apply_env_overrides<F>(get_var: &F, resolution: &mut EnvConfigResolution)
where
    F: Fn(&str) -> Option<String>,
{
    apply_sample_env_overrides(get_var, resolution);
    apply_selection_env_overrides(get_var, resolution);
    apply_execution_env_overrides(get_var, resolution);
    apply_diagnostic_env_overrides(get_var, resolution);
}

fn apply_sample_env_overrides<F>(get_var: &F, resolution: &mut EnvConfigResolution)
where
    F: Fn(&str) -> Option<String>,
{
    parse_env(
        &get_var,
        resolution,
        "STRESS_SAMPLES",
        "samples",
        |value| value.parse::<usize>().ok(),
        |config, value| config.samples = value,
    );
    parse_env(
        &get_var,
        resolution,
        "STRESS_WARMUP_SAMPLES",
        "warmup_samples",
        |value| value.parse::<usize>().ok(),
        |config, value| config.warmup_samples = value,
    );
    parse_env(
        &get_var,
        resolution,
        "STRESS_COOLDOWN_SAMPLES",
        "cooldown_samples",
        |value| value.parse::<usize>().ok(),
        |config, value| config.cooldown_samples = value,
    );
}

fn apply_selection_env_overrides<F>(get_var: &F, resolution: &mut EnvConfigResolution)
where
    F: Fn(&str) -> Option<String>,
{
    parse_env(
        &get_var,
        resolution,
        "STRESS_JSON",
        "json_stdout",
        parse_bool_env,
        |config, value| config.json_stdout = value,
    );
    parse_env(
        &get_var,
        resolution,
        "STRESS_OUTPUT_DIR",
        "output_dir",
        |value| Some(value.to_string()),
        |config, value| config.output_dir = PathBuf::from(value),
    );
    parse_env(
        &get_var,
        resolution,
        "STRESS_FILTER",
        "filter",
        |value| Some(value.to_string()),
        |config, value| config.filter = Some(value),
    );
    parse_env(
        &get_var,
        resolution,
        "STRESS_GIT_SHA",
        "git_sha",
        |value| Some(value.to_string()),
        |config, value| config.git_sha = Some(value),
    );
    parse_env(
        &get_var,
        resolution,
        "STRESS_TIER",
        "tier",
        |value| value.parse::<u32>().ok(),
        |config, value| config.tier = Some(value),
    );
    parse_env(
        &get_var,
        resolution,
        "STRESS_CONSOLE_NAMES",
        "console_names",
        |value| value.parse::<ConsoleNameMode>().ok(),
        |config, value| config.console_names = value,
    );
    parse_env(
        &get_var,
        resolution,
        "STRESS_PROGRESS",
        "progress",
        parse_bool_env,
        |config, value| config.progress = value,
    );
}

fn apply_execution_env_overrides<F>(get_var: &F, resolution: &mut EnvConfigResolution)
where
    F: Fn(&str) -> Option<String>,
{
    parse_env(
        &get_var,
        resolution,
        "STRESS_TIMEOUT_SECS",
        "timeout_secs",
        |value| value.parse::<u64>().ok().filter(|value| *value != 0),
        |config, value| config.timeout = Some(Duration::from_secs(value)),
    );
    parse_env(
        &get_var,
        resolution,
        "STRESS_SAMPLE_DURATION_MS",
        "sample_duration",
        |value| value.parse::<u64>().ok().filter(|value| *value != 0),
        |config, value| config.sample_duration = Duration::from_millis(value),
    );
    parse_env(
        &get_var,
        resolution,
        "STRESS_OPERATIONS_PER_SAMPLE",
        "operations_per_sample",
        |value| value.parse::<u64>().ok().filter(|value| *value != 0),
        |config, value| config.operations_per_sample = value,
    );
    parse_env(
        &get_var,
        resolution,
        "STRESS_MICRO_SAMPLE_DURATION_MS",
        "micro_sample_duration",
        |value| value.parse::<u64>().ok().filter(|value| *value != 0),
        |config, value| config.micro_sample_duration = Duration::from_millis(value),
    );
    parse_env(
        &get_var,
        resolution,
        "STRESS_THRESHOLD",
        "threshold",
        |value| {
            value
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
        },
        |config, value| config.threshold = value,
    );
}

fn apply_diagnostic_env_overrides<F>(get_var: &F, resolution: &mut EnvConfigResolution)
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(value) = get_var("STRESS_FAIL_ON_ISSUES") {
        match parse_bool_env(&value) {
            Some(true) => {
                resolution.config.deny_diagnostics = Some(DiagnosticSeverity::Warning);
                resolution.metadata.insert(
                    "deny_diagnostics_src".to_string(),
                    "env STRESS_FAIL_ON_ISSUES".to_string(),
                );
            }
            Some(false) => {
                resolution.config.deny_diagnostics = None;
                resolution.metadata.insert(
                    "deny_diagnostics_src".to_string(),
                    "env STRESS_FAIL_ON_ISSUES".to_string(),
                );
            }
            None => resolution
                .warnings
                .push("invalid STRESS_FAIL_ON_ISSUES; expected true or false".to_string()),
        }
    }

    if let Some(value) = get_var("STRESS_DENY_DIAGNOSTICS") {
        match value.parse::<DiagnosticSeverity>() {
            Ok(threshold) => {
                resolution.config.deny_diagnostics = Some(threshold);
                resolution.metadata.insert(
                    "deny_diagnostics_src".to_string(),
                    "env STRESS_DENY_DIAGNOSTICS".to_string(),
                );
            }
            Err(_) => resolution.warnings.push(
                "invalid STRESS_DENY_DIAGNOSTICS; expected info, warning, or error".to_string(),
            ),
        }
    }
}

fn parse_env<F, T, P, A>(
    get_var: &F,
    resolution: &mut EnvConfigResolution,
    env_key: &'static str,
    metadata_key: &'static str,
    parse: P,
    apply: A,
) where
    F: Fn(&str) -> Option<String>,
    P: FnOnce(&str) -> Option<T>,
    A: FnOnce(&mut StressRunnerConfig, T),
{
    let Some(value) = get_var(env_key) else {
        return;
    };
    match parse(&value) {
        Some(parsed) => {
            apply(&mut resolution.config, parsed);
            resolution
                .metadata
                .insert(format!("{metadata_key}_src"), format!("env {env_key}"));
        }
        None => resolution.warnings.push(format!("invalid {env_key}")),
    }
}

fn apply_default_sources(metadata: &mut HashMap<String, String>) {
    for key in [
        "profile_src",
        "samples_src",
        "warmup_samples_src",
        "cooldown_samples_src",
        "output_dir_src",
        "json_stdout_src",
        "filter_src",
        "tier_src",
        "git_sha_src",
        "deny_diagnostics_src",
        "console_names_src",
        "progress_src",
        "timeout_secs_src",
        "sample_duration_src",
        "operations_per_sample_src",
        "micro_sample_duration_src",
        "threshold_src",
    ] {
        metadata
            .entry(key.to_string())
            .or_insert_with(|| "default".to_string());
    }
}

pub(crate) fn parse_bool_env(value: &str) -> Option<bool> {
    if value == "1" || value.eq_ignore_ascii_case("true") {
        Some(true)
    } else if value == "0" || value.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

fn detect_git_sha() -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|sha| sha.trim().to_string())
            } else {
                None
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_is_moderate_day_to_day_run() {
        let cfg = StressRunnerConfig::default();

        assert_eq!(cfg.profile, RunProfile::Default);
        assert_eq!(cfg.samples, 5);
        assert_eq!(cfg.warmup_samples, 1);
        assert_eq!(cfg.cooldown_samples, 0);
        assert_eq!(cfg.sample_duration, Duration::from_millis(500));
        assert_eq!(cfg.micro_sample_duration, Duration::from_millis(25));
        assert_eq!(cfg.min_quality, QualityClass::Noisy);
        assert!(!cfg.fail_on_quality);
        assert!(!cfg.fail_on_regression);
    }

    #[test]
    fn smoke_profile_is_an_explicit_fast_correctness_run() {
        let cfg = StressRunnerConfig::for_profile(RunProfile::Smoke);

        assert_eq!(cfg.profile, RunProfile::Smoke);
        assert_eq!(cfg.samples, 1);
        assert_eq!(cfg.warmup_samples, 0);
        assert_eq!(cfg.sample_duration, Duration::from_millis(10));
        assert_eq!(cfg.micro_sample_duration, Duration::from_millis(5));
        assert_eq!(cfg.min_quality, QualityClass::Untrustworthy);
        assert!(!cfg.fail_on_quality);
        assert!(!cfg.fail_on_regression);
    }

    #[test]
    fn lab_profile_is_exhaustive_exploration() {
        let cfg = StressRunnerConfig::for_profile(RunProfile::Lab);

        assert_eq!(cfg.profile, RunProfile::Lab);
        assert_eq!(cfg.samples, 30);
        assert_eq!(cfg.warmup_samples, 2);
        assert_eq!(cfg.cooldown_samples, 1);
        assert_eq!(cfg.sample_duration, Duration::from_secs(5));
        assert_eq!(cfg.micro_sample_duration, Duration::from_millis(200));
        assert_eq!(cfg.min_quality, QualityClass::Noisy);
        assert!(!cfg.fail_on_quality);
        assert!(!cfg.fail_on_regression);
    }

    #[test]
    fn builder_sets_sample_and_filter_fields() {
        let cfg = StressRunnerConfig::new()
            .samples(5)
            .warmup_samples(2)
            .cooldown_samples(1)
            .filter("my_bench")
            .tier(3)
            .operations_per_sample(10)
            .threshold_percent(5.0);

        assert_eq!(cfg.samples, 5);
        assert_eq!(cfg.warmup_samples, 2);
        assert_eq!(cfg.cooldown_samples, 1);
        assert!(!cfg.json_stdout);
        assert_eq!(cfg.filter, Some("my_bench".to_string()));
        assert_eq!(cfg.tier, Some(3));
        assert_eq!(cfg.operations_per_sample, 10);
        assert!((cfg.threshold - 0.05).abs() < f64::EPSILON);
        assert_eq!(cfg.micro_sample_duration, Duration::from_millis(25));
    }

    #[test]
    fn threshold_fraction_has_an_explicit_unit() {
        let cfg = StressRunnerConfig::new().threshold_fraction(0.07);

        assert!((cfg.threshold - 0.07).abs() < f64::EPSILON);
    }

    #[test]
    fn validation_rejects_undefined_tier_filter() {
        let cfg = StressRunnerConfig::new().tier(MAX_TIER + 1);

        assert_eq!(
            cfg.validation_errors(),
            vec!["tier must be between 1 and 6".to_string()]
        );
    }

    #[test]
    fn stress_env_values_override_profile_defaults() {
        let env = HashMap::from([
            ("STRESS_PROFILE", "release".to_string()),
            ("STRESS_SAMPLES", "7".to_string()),
            ("STRESS_WARMUP_SAMPLES", "2".to_string()),
            ("STRESS_JSON", "true".to_string()),
            ("STRESS_TIER", "4".to_string()),
        ]);

        let resolution = StressRunnerConfig::resolve_from_env_with(|key| env.get(key).cloned());

        assert_eq!(resolution.config.profile, RunProfile::Release);
        assert_eq!(resolution.config.samples, 7);
        assert_eq!(resolution.config.warmup_samples, 2);
        assert!(resolution.config.json_stdout);
        assert_eq!(resolution.config.tier, Some(4));
        assert!(resolution.warnings.is_empty());
    }

    #[test]
    fn malformed_env_values_warn_and_keep_defaults() {
        let env = HashMap::from([
            ("STRESS_SAMPLES", "abc".to_string()),
            ("STRESS_JSON", "maybe".to_string()),
            ("STRESS_TIMEOUT_SECS", "soon".to_string()),
        ]);

        let resolution = StressRunnerConfig::resolve_from_env_with(|key| env.get(key).cloned());

        assert_eq!(resolution.config.samples, 5);
        assert!(!resolution.config.json_stdout);
        assert_eq!(resolution.config.timeout, None);
        assert_eq!(resolution.warnings.len(), 3);
    }

    #[test]
    fn invalid_profile_source_metadata_matches_default_value() {
        let env = HashMap::from([("STRESS_PROFILE", "unknown".to_string())]);

        let resolution = StressRunnerConfig::resolve_from_env_with(|key| env.get(key).cloned());

        assert_eq!(resolution.config.profile, RunProfile::Default);
        assert_eq!(
            resolution.metadata.get("profile_src"),
            Some(&"default".to_string())
        );
        assert_eq!(
            resolution.warnings,
            vec!["invalid STRESS_PROFILE; expected default, smoke, lab, or release".to_string()]
        );
    }

    #[test]
    fn validation_rejects_no_work_sample_counts() {
        let mut cfg = StressRunnerConfig::new();

        cfg.samples = 0;
        cfg.operations_per_sample = 0;

        assert_eq!(
            cfg.validation_errors(),
            vec![
                "samples must be greater than 0".to_string(),
                "operations_per_sample must be greater than 0".to_string()
            ]
        );
    }

    #[test]
    fn validation_rejects_invalid_regression_thresholds() {
        for threshold in [f64::NAN, f64::INFINITY, -0.01, 1.01] {
            let mut cfg = StressRunnerConfig::new();
            cfg.threshold = threshold;

            assert_eq!(
                cfg.validation_errors(),
                vec!["threshold must be finite and between 0 and 1".to_string()]
            );
        }
    }

    #[test]
    fn validation_rejects_zero_execution_durations() {
        let mut cfg = StressRunnerConfig::new();
        cfg.sample_duration = Duration::ZERO;
        cfg.micro_sample_duration = Duration::ZERO;
        cfg.timeout = Some(Duration::ZERO);

        assert_eq!(
            cfg.validation_errors(),
            vec![
                "sample_duration must be greater than 0".to_string(),
                "micro_sample_duration must be greater than 0".to_string(),
                "timeout must be greater than 0 when configured".to_string(),
            ]
        );
    }

    #[test]
    fn zero_execution_env_values_warn_and_keep_profile_defaults() {
        let env = HashMap::from([
            ("STRESS_TIMEOUT_SECS", "0".to_string()),
            ("STRESS_SAMPLE_DURATION_MS", "0".to_string()),
            ("STRESS_OPERATIONS_PER_SAMPLE", "0".to_string()),
            ("STRESS_MICRO_SAMPLE_DURATION_MS", "0".to_string()),
        ]);

        let resolution = StressRunnerConfig::resolve_from_env_with(|key| env.get(key).cloned());

        assert_eq!(resolution.config.timeout, None);
        assert_eq!(
            resolution.config.sample_duration,
            Duration::from_millis(500)
        );
        assert_eq!(resolution.config.operations_per_sample, 1);
        assert_eq!(
            resolution.config.micro_sample_duration,
            Duration::from_millis(25)
        );
        assert_eq!(resolution.warnings.len(), 4);
    }

    #[test]
    fn invalid_threshold_env_values_warn_and_keep_the_profile_default() {
        for threshold in ["NaN", "inf", "-0.01", "1.01"] {
            let env = HashMap::from([("STRESS_THRESHOLD", threshold.to_string())]);

            let resolution = StressRunnerConfig::resolve_from_env_with(|key| env.get(key).cloned());

            assert!((resolution.config.threshold - 0.05).abs() < f64::EPSILON);
            assert_eq!(
                resolution.warnings,
                vec!["invalid STRESS_THRESHOLD".to_string()]
            );
            assert_eq!(
                resolution.metadata.get("threshold_src"),
                Some(&"default".to_string())
            );
        }
    }

    #[test]
    fn mode_for_kind_uses_resolved_profile_settings() {
        let cfg = StressRunnerConfig::new()
            .sample_duration(Duration::from_millis(250))
            .operations_per_sample(12);

        assert_eq!(
            cfg.mode_for_kind(BenchmarkModeKind::FixedDuration),
            BenchmarkMode::FixedDuration {
                sample_duration: Duration::from_millis(250)
            }
        );
        assert_eq!(
            cfg.mode_for_kind(BenchmarkModeKind::FixedOperations),
            BenchmarkMode::FixedOperations {
                operations_per_sample: 12
            }
        );
        assert_eq!(
            cfg.mode_for_kind(BenchmarkModeKind::Micro),
            BenchmarkMode::Micro {
                target_sample_duration: Duration::from_millis(25)
            }
        );
    }

    #[test]
    fn mode_for_tier_uses_tier_derived_modes() {
        let cfg = StressRunnerConfig::new()
            .sample_duration(Duration::from_millis(250))
            .operations_per_sample(12)
            .micro_sample_duration(Duration::from_millis(25));

        assert_eq!(
            cfg.mode_for_tier(1),
            Some(BenchmarkMode::Micro {
                target_sample_duration: Duration::from_millis(25)
            })
        );
        assert_eq!(
            cfg.mode_for_tier(2),
            Some(BenchmarkMode::FixedOperations {
                operations_per_sample: 12
            })
        );
        for tier in 3..=MAX_TIER {
            assert_eq!(
                cfg.mode_for_tier(tier),
                Some(BenchmarkMode::FixedDuration {
                    sample_duration: Duration::from_millis(250)
                })
            );
        }
        assert_eq!(cfg.mode_for_tier(0), None);
        assert_eq!(cfg.mode_for_tier(MAX_TIER + 1), None);
    }
}
