//! Configuration for the benchmark runner.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub(crate) struct EnvConfigResolution {
    pub config: BenchRunnerConfig,
    pub metadata: HashMap<String, String>,
    pub warnings: Vec<String>,
}

impl EnvConfigResolution {
    fn new(config: BenchRunnerConfig) -> Self {
        Self {
            config,
            metadata: HashMap::new(),
            warnings: Vec::new(),
        }
    }
}

/// Configuration for the benchmark runner.
#[derive(Debug, Clone)]
pub struct BenchRunnerConfig {
    /// Number of measurement runs (reports median).
    pub runs: usize,
    /// Warmup runs (discarded).
    pub warmup_runs: usize,
    /// Output directory for JSON results.
    pub output_dir: PathBuf,
    /// Print results to stdout.
    pub verbose: bool,
    /// Filter benchmarks by name substring.
    pub filter: Option<String>,
    /// Git SHA to include in results (for regression tracking).
    pub git_sha: Option<String>,
    /// Fail if any benchmark exceeds this duration.
    pub timeout: Option<std::time::Duration>,
}

impl Default for BenchRunnerConfig {
    fn default() -> Self {
        Self {
            runs: 1,
            warmup_runs: 0,
            output_dir: PathBuf::from("target/stress"),
            verbose: true,
            filter: None,
            git_sha: None,
            timeout: None,
        }
    }
}

impl BenchRunnerConfig {
    /// Create a new config with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse config from environment variables.
    ///
    /// Supported variables:
    /// - `BENCH_RUNS`: measurement runs (default: 1)
    /// - `BENCH_WARMUP`: warmup runs (default: 0)
    /// - `BENCH_VERBOSE`: verbose output (default: true)
    /// - `BENCH_OUTPUT_DIR`: output directory
    /// - `BENCH_FILTER`: filter benchmarks by name
    /// - `BENCH_GIT_SHA`: git commit hash
    /// - `BENCH_TIMEOUT_SECS`: timeout per benchmark in seconds
    pub fn from_env() -> Self {
        let resolution = Self::resolve_from_env();
        for warning in &resolution.warnings {
            eprintln!("Warning: {}", warning);
        }
        resolution.config
    }

    pub(crate) fn resolve_from_env() -> EnvConfigResolution {
        Self::resolve_from_env_with(|key| std::env::var(key).ok())
    }

    pub(crate) fn resolve_from_env_with<F>(get_var: F) -> EnvConfigResolution
    where
        F: Fn(&str) -> Option<String>,
    {
        let mut resolution = EnvConfigResolution::new(Self::default());

        if let Some(v) = get_var("BENCH_RUNS") {
            match v.parse() {
                Ok(n) => {
                    resolution.config.runs = n;
                    resolution
                        .metadata
                        .insert("runs_src".to_string(), "env BENCH_RUNS".to_string());
                }
                Err(_) => resolution
                    .warnings
                    .push("invalid BENCH_RUNS, using default 1".to_string()),
            }
        }

        if let Some(v) = get_var("BENCH_WARMUP") {
            match v.parse() {
                Ok(n) => {
                    resolution.config.warmup_runs = n;
                    resolution.metadata.insert(
                        "warmup_runs_src".to_string(),
                        "env BENCH_WARMUP".to_string(),
                    );
                }
                Err(_) => resolution
                    .warnings
                    .push("invalid BENCH_WARMUP, using default 0".to_string()),
            }
        }

        if let Some(v) = get_var("BENCH_VERBOSE") {
            match parse_bool_env(&v) {
                Some(verbose) => {
                    resolution.config.verbose = verbose;
                    resolution
                        .metadata
                        .insert("verbose_src".to_string(), "env BENCH_VERBOSE".to_string());
                }
                None => resolution
                    .warnings
                    .push("invalid BENCH_VERBOSE, using default true".to_string()),
            }
        }

        if let Some(v) = get_var("BENCH_OUTPUT_DIR") {
            resolution.config.output_dir = PathBuf::from(v);
            resolution.metadata.insert(
                "output_dir_src".to_string(),
                "env BENCH_OUTPUT_DIR".to_string(),
            );
        }

        if let Some(v) = get_var("BENCH_FILTER") {
            resolution.config.filter = Some(v);
            resolution
                .metadata
                .insert("filter_src".to_string(), "env BENCH_FILTER".to_string());
        }

        if let Some(v) = get_var("BENCH_GIT_SHA") {
            resolution.config.git_sha = Some(v);
            resolution
                .metadata
                .insert("git_sha_src".to_string(), "env BENCH_GIT_SHA".to_string());
        }

        if let Some(v) = get_var("BENCH_TIMEOUT_SECS") {
            match v.parse::<u64>() {
                Ok(secs) => {
                    resolution.config.timeout = Some(Duration::from_secs(secs));
                    resolution.metadata.insert(
                        "timeout_secs_src".to_string(),
                        "env BENCH_TIMEOUT_SECS".to_string(),
                    );
                }
                Err(_) => resolution
                    .warnings
                    .push("invalid BENCH_TIMEOUT_SECS, using no timeout".to_string()),
            }
        }

        apply_default_sources(&mut resolution.metadata);

        if resolution.config.git_sha.is_none() {
            resolution.config.git_sha = detect_git_sha();
            if resolution.config.git_sha.is_some() {
                resolution
                    .metadata
                    .insert("git_sha_src".to_string(), "auto_detect".to_string());
            }
        }

        resolution
    }

    /// Set the number of measurement runs.
    pub fn runs(mut self, n: usize) -> Self {
        self.runs = n;
        self
    }

    /// Set the number of warmup runs.
    pub fn warmup(mut self, n: usize) -> Self {
        self.warmup_runs = n;
        self
    }

    /// Set the output directory.
    pub fn output_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.output_dir = path.into();
        self
    }

    /// Set verbose output.
    pub fn verbose(mut self, v: bool) -> Self {
        self.verbose = v;
        self
    }

    /// Set filter pattern.
    pub fn filter(mut self, pattern: impl Into<String>) -> Self {
        self.filter = Some(pattern.into());
        self
    }

    /// Clear filter pattern.
    pub fn no_filter(mut self) -> Self {
        self.filter = None;
        self
    }

    /// Set git SHA.
    pub fn git_sha(mut self, sha: impl Into<String>) -> Self {
        self.git_sha = Some(sha.into());
        self
    }

    /// Set timeout per benchmark.
    pub fn timeout(mut self, duration: std::time::Duration) -> Self {
        self.timeout = Some(duration);
        self
    }
}

fn apply_default_sources(metadata: &mut HashMap<String, String>) {
    for key in [
        "runs_src",
        "warmup_runs_src",
        "output_dir_src",
        "verbose_src",
        "filter_src",
        "git_sha_src",
        "timeout_secs_src",
    ] {
        metadata
            .entry(key.to_string())
            .or_insert_with(|| "default".to_string());
    }
}

fn parse_bool_env(value: &str) -> Option<bool> {
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
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_use_defaults_when_env_not_set() {
        let cfg = BenchRunnerConfig::default();
        assert_eq!(cfg.runs, 1);
        assert_eq!(cfg.warmup_runs, 0);
        assert!(cfg.verbose);
    }

    #[test]
    fn should_build_config_with_builder() {
        let cfg = BenchRunnerConfig::new()
            .runs(5)
            .warmup(2)
            .verbose(false)
            .filter("my_bench");

        assert_eq!(cfg.runs, 5);
        assert_eq!(cfg.warmup_runs, 2);
        assert!(!cfg.verbose);
        assert_eq!(cfg.filter, Some("my_bench".to_string()));
    }

    #[test]
    fn should_use_env_values_when_present() {
        let env = HashMap::from([
            ("BENCH_RUNS", "5".to_string()),
            ("BENCH_WARMUP", "2".to_string()),
            ("BENCH_VERBOSE", "false".to_string()),
        ]);

        let resolution = BenchRunnerConfig::resolve_from_env_with(|key| env.get(key).cloned());
        let cfg = resolution.config;

        assert_eq!(cfg.runs, 5);
        assert_eq!(cfg.warmup_runs, 2);
        assert!(!cfg.verbose);
        assert!(resolution.warnings.is_empty());
    }

    #[test]
    fn should_use_defaults_when_env_missing() {
        let cfg = BenchRunnerConfig::resolve_from_env_with(|_| None).config;

        assert_eq!(cfg.runs, 1);
        assert_eq!(cfg.warmup_runs, 0);
        assert!(cfg.verbose);
    }

    #[test]
    fn should_warn_when_env_values_are_invalid() {
        let env = HashMap::from([
            ("BENCH_RUNS", "abc".to_string()),
            ("BENCH_VERBOSE", "maybe".to_string()),
            ("BENCH_TIMEOUT_SECS", "soon".to_string()),
        ]);

        let resolution = BenchRunnerConfig::resolve_from_env_with(|key| env.get(key).cloned());

        assert_eq!(resolution.config.runs, 1);
        assert!(resolution.config.verbose);
        assert_eq!(resolution.config.timeout, None);
        assert!(resolution
            .warnings
            .contains(&"invalid BENCH_RUNS, using default 1".to_string()));
        assert!(resolution
            .warnings
            .contains(&"invalid BENCH_VERBOSE, using default true".to_string()));
        assert!(resolution
            .warnings
            .contains(&"invalid BENCH_TIMEOUT_SECS, using no timeout".to_string()));
    }
}
