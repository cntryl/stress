//! Configuration for the benchmark runner.

use std::path::PathBuf;

/// Configuration for the benchmark runner.
#[derive(Debug, Clone)]
pub struct BenchRunnerConfig {
    /// Number of measurement runs (reports median).
    pub runs: usize,
    /// Warmup runs (discarded).
    pub warmup_runs: usize,
    /// Output directory for JSON results.
    pub output_dir: PathBuf,
    /// Print results to stderr.
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
        let mut cfg = Self::default();

        if let Ok(v) = std::env::var("BENCH_RUNS") {
            if let Ok(n) = v.parse() {
                cfg.runs = n;
            }
        }
        if let Ok(v) = std::env::var("BENCH_WARMUP") {
            if let Ok(n) = v.parse() {
                cfg.warmup_runs = n;
            }
        }
        if let Ok(v) = std::env::var("BENCH_VERBOSE") {
            cfg.verbose = v != "0" && !v.eq_ignore_ascii_case("false");
        }
        if let Ok(v) = std::env::var("BENCH_OUTPUT_DIR") {
            cfg.output_dir = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("BENCH_FILTER") {
            cfg.filter = Some(v);
        }
        if let Ok(v) = std::env::var("BENCH_GIT_SHA") {
            cfg.git_sha = Some(v);
        }
        if let Ok(v) = std::env::var("BENCH_TIMEOUT_SECS") {
            if let Ok(secs) = v.parse::<u64>() {
                cfg.timeout = Some(std::time::Duration::from_secs(secs));
            }
        }

        // Try to detect git SHA if not set
        if cfg.git_sha.is_none() {
            cfg.git_sha = detect_git_sha();
        }

        cfg
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
}
