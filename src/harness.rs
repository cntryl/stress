//! Test harness for auto-discovered stress benchmarks.
//!
//! This module provides the infrastructure for discovering and running
//! benchmarks marked with `#[stress_test]`.

use crate::{BenchContext, BenchRunner, BenchRunnerConfig};

/// A registered benchmark entry.
#[doc(hidden)]
pub struct BenchmarkEntry {
    /// Benchmark name (function name or custom)
    pub name: &'static str,
    /// The benchmark function
    pub func: fn(&mut BenchContext),
    /// Whether this benchmark is ignored by default
    pub ignored: bool,
    /// Module path where the benchmark is defined
    pub module_path: &'static str,
}

// Re-export linkme for the proc macro
#[doc(hidden)]
pub use linkme;

/// Distributed slice collecting all registered benchmarks.
#[doc(hidden)]
#[linkme::distributed_slice]
pub static STRESS_BENCHMARKS: [BenchmarkEntry];

/// Options for running discovered benchmarks.
#[derive(Debug, Clone, Default)]
pub struct StressRunnerOptions {
    /// Filter benchmarks by glob pattern
    pub workload: Option<String>,
    /// Include ignored benchmarks
    pub include_ignored: bool,
    /// Number of measurement runs
    pub runs: Option<usize>,
    /// Number of warmup runs
    pub warmup: Option<usize>,
    /// Verbose output
    pub verbose: bool,
    /// Baseline file for comparison
    pub baseline: Option<std::path::PathBuf>,
    /// Regression threshold (e.g., 0.05 for 5%)
    pub threshold: f64,
}

impl StressRunnerOptions {
    pub fn new() -> Self {
        Self {
            threshold: 0.05,
            verbose: true,
            ..Default::default()
        }
    }

    pub fn workload(mut self, pattern: impl Into<String>) -> Self {
        self.workload = Some(pattern.into());
        self
    }

    pub fn runs(mut self, n: usize) -> Self {
        self.runs = Some(n);
        self
    }

    pub fn warmup(mut self, n: usize) -> Self {
        self.warmup = Some(n);
        self
    }

    pub fn verbose(mut self, v: bool) -> Self {
        self.verbose = v;
        self
    }

    pub fn include_ignored(mut self, v: bool) -> Self {
        self.include_ignored = v;
        self
    }

    pub fn baseline(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.baseline = Some(path.into());
        self
    }

    pub fn threshold(mut self, t: f64) -> Self {
        self.threshold = t;
        self
    }
}

/// Run all registered benchmarks with default options.
///
/// This is called by the `stress_main!` macro.
pub fn run_registered_benchmarks() {
    run_with_options(StressRunnerOptions::new());
}

/// Run all registered benchmarks with custom options.
pub fn run_with_options(opts: StressRunnerOptions) {
    let benchmarks: Vec<_> = STRESS_BENCHMARKS
        .iter()
        .filter(|b| {
            // Filter by ignored status
            if b.ignored && !opts.include_ignored {
                return false;
            }
            // Filter by workload pattern
            if let Some(ref pattern) = opts.workload {
                return matches_glob(b.name, pattern) || matches_glob(b.module_path, pattern);
            }
            true
        })
        .collect();

    if benchmarks.is_empty() {
        if opts.workload.is_some() {
            eprintln!("No benchmarks matched the workload pattern");
        } else {
            eprintln!("No benchmarks registered. Add #[stress_test] to your benchmark functions.");
        }
        return;
    }

    // Build config
    let mut config = BenchRunnerConfig::from_env();
    if let Some(r) = opts.runs {
        config.runs = r;
    }
    if let Some(w) = opts.warmup {
        config.warmup_runs = w;
    }
    config.verbose = opts.verbose;

    let mut runner = BenchRunner::with_config("stress", config);

    // Run each benchmark
    for bench in &benchmarks {
        let name = format!("{}::{}", bench.module_path, bench.name);
        runner.run(&name, bench.func);
    }

    // Finish and check for regressions
    if let Some(baseline_path) = opts.baseline {
        let (_results, regressions) = runner.finish_with_baseline(baseline_path, opts.threshold);
        if !regressions.is_empty() {
            eprintln!("\n❌ {} regression(s) detected!", regressions.len());
            for (result, ratio) in &regressions {
                let pct = (ratio - 1.0) * 100.0;
                eprintln!("  {} is {:.1}% slower", result.name, pct);
            }
            std::process::exit(1);
        }
    } else {
        let results = runner.finish();
        eprintln!("\n✅ {} benchmark(s) completed", results.len());
    }
}

/// Simple glob matching supporting * and ?
fn matches_glob(text: &str, pattern: &str) -> bool {
    let pattern = pattern.to_lowercase();
    let text = text.to_lowercase();

    // Convert glob to a simple check
    if pattern.contains('*') {
        // Split by * and check if all parts appear in order
        let parts: Vec<&str> = pattern.split('*').collect();
        let mut remaining = text.as_str();

        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }
            if i == 0 {
                // First part must be at the start
                if !remaining.starts_with(part) {
                    return false;
                }
                remaining = &remaining[part.len()..];
            } else if i == parts.len() - 1 && !pattern.ends_with('*') {
                // Last part must be at the end
                if !remaining.ends_with(part) {
                    return false;
                }
            } else {
                // Middle parts just need to exist
                if let Some(pos) = remaining.find(part) {
                    remaining = &remaining[pos + part.len()..];
                } else {
                    return false;
                }
            }
        }
        true
    } else {
        // No wildcards - substring match
        text.contains(&pattern)
    }
}

/// Get a list of all registered benchmark names.
///
/// Useful for tooling and IDE integration.
pub fn list_benchmarks() -> Vec<&'static str> {
    STRESS_BENCHMARKS.iter().map(|b| b.name).collect()
}

/// Get count of registered benchmarks.
pub fn benchmark_count() -> usize {
    STRESS_BENCHMARKS.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_substring() {
        assert!(matches_glob("foo_bar_baz", "bar"));
        assert!(!matches_glob("foo_bar_baz", "qux"));
    }

    #[test]
    fn glob_matches_wildcard() {
        assert!(matches_glob("foo_bar_baz", "foo*baz"));
        assert!(matches_glob("foo_bar_baz", "*bar*"));
        assert!(matches_glob("foo_bar_baz", "foo*"));
        assert!(matches_glob("foo_bar_baz", "*baz"));
        assert!(!matches_glob("foo_bar_baz", "qux*"));
    }

    #[test]
    fn glob_is_case_insensitive() {
        assert!(matches_glob("FooBar", "foobar"));
        assert!(matches_glob("foobar", "FOO*"));
    }
}
