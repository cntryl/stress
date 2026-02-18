//! Test harness for auto-discovered stress benchmarks.
//!
//! This module provides the infrastructure for discovering and running
//! benchmarks marked with `#[stress_test]`.
//!
//! ## Architecture
//!
//! When you use `#[stress_test]` and `stress_main!()`, this is what happens:
//!
//! 1. `#[stress_test]` registers each function in a distributed slice via linkme
//! 2. `stress_main!()` generates a main() that calls `stress_binary_main()`
//! 3. `stress_binary_main()` parses CLI args and calls `run_with_options()`
//! 4. `run_with_options()` iterates the slice and runs matching benchmarks
//!
//! This means each stress binary is self-contained and handles its own argument
//! parsing - `cargo-stress` just orchestrates which binaries to build and run.

use crate::{BenchRunner, BenchRunnerConfig, StressContext};
use std::path::PathBuf;

/// A registered benchmark entry.
#[doc(hidden)]
pub struct BenchmarkEntry {
    /// Benchmark name (function name or custom)
    pub name: &'static str,
    /// The benchmark function
    pub func: fn(&mut StressContext),
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

// ============================================================================
// CLI Arguments for Stress Binaries
// ============================================================================

/// Command-line arguments for stress test binaries.
///
/// These arguments are parsed by the generated main() function from stress_main!().
/// They match the flags that cargo-stress passes through.
#[derive(Debug, Clone)]
struct StressBinaryArgs {
    /// Filter benchmarks by glob pattern
    workload: Option<String>,
    /// Number of measurement runs
    runs: usize,
    /// Number of warmup runs
    warmup: usize,
    /// Verbose output
    verbose: bool,
    /// Quiet mode
    quiet: bool,
    /// Include ignored benchmarks
    include_ignored: bool,
    /// List benchmarks without running
    list: bool,
    /// Output directory for JSON results
    output_dir: Option<PathBuf>,
    /// Baseline JSON for regression comparison
    baseline: Option<PathBuf>,
    /// Regression threshold
    threshold: f64,
}

impl Default for StressBinaryArgs {
    fn default() -> Self {
        Self {
            workload: None,
            runs: 1,
            warmup: 0,
            verbose: false,
            quiet: false,
            include_ignored: false,
            list: false,
            output_dir: None,
            baseline: None,
            threshold: 0.05,
        }
    }
}

impl StressBinaryArgs {
    /// Parse command-line arguments.
    ///
    /// We use a simple hand-rolled parser to avoid adding clap as a dependency
    /// for every stress binary. The argument format matches what cargo-stress passes.
    fn parse() -> Self {
        let args: Vec<String> = std::env::args().collect();
        let mut result = Self::default();
        let mut i = 1;

        while i < args.len() {
            match args[i].as_str() {
                "--workload" => {
                    i += 1;
                    if i < args.len() {
                        result.workload = Some(args[i].clone());
                    }
                }
                "--runs" => {
                    i += 1;
                    if i < args.len() {
                        result.runs = args[i].parse().unwrap_or(1);
                    }
                }
                "--warmup" => {
                    i += 1;
                    if i < args.len() {
                        result.warmup = args[i].parse().unwrap_or(0);
                    }
                }
                "--verbose" | "-v" => {
                    result.verbose = true;
                }
                "--quiet" | "-q" => {
                    result.quiet = true;
                }
                "--include-ignored" => {
                    result.include_ignored = true;
                }
                "--list" => {
                    result.list = true;
                }
                "--output-dir" => {
                    i += 1;
                    if i < args.len() {
                        result.output_dir = Some(PathBuf::from(&args[i]));
                    }
                }
                "--baseline" => {
                    i += 1;
                    if i < args.len() {
                        result.baseline = Some(PathBuf::from(&args[i]));
                    }
                }
                "--threshold" => {
                    i += 1;
                    if i < args.len() {
                        result.threshold = args[i].parse().unwrap_or(0.05);
                    }
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                _ => {
                    // Ignore unknown args
                }
            }
            i += 1;
        }

        result
    }
}

fn print_help() {
    eprintln!("Stress test binary");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("    <binary> [OPTIONS]");
    eprintln!();
    eprintln!("OPTIONS:");
    eprintln!("    --workload <PATTERN>   Filter benchmarks by glob pattern");
    eprintln!("    --runs <N>             Number of measurement runs (default: 1)");
    eprintln!("    --warmup <N>           Number of warmup runs (default: 0)");
    eprintln!("    -v, --verbose          Verbose output");
    eprintln!("    -q, --quiet            Quiet mode");
    eprintln!("    --include-ignored      Include ignored benchmarks");
    eprintln!("    --list                 List benchmarks without running");
    eprintln!("    --output-dir <PATH>    Output directory for JSON results");
    eprintln!("    --baseline <PATH>      Baseline JSON for regression comparison");
    eprintln!("    --threshold <FLOAT>    Regression threshold (default: 0.05)");
    eprintln!("    -h, --help             Show this help message");
}

// ============================================================================
// Main Entry Point for Stress Binaries
// ============================================================================

/// Main entry point for stress test binaries generated by `stress_main!()`.
///
/// This function:
/// 1. Parses command-line arguments
/// 2. Handles --list mode
/// 3. Runs benchmarks with the specified options
/// 4. Exits with non-zero status on failure
///
/// # Panics
///
/// This function does not panic. It exits with appropriate exit codes:
/// - 0: All benchmarks passed
/// - 1: One or more benchmarks failed or regressed
pub fn stress_binary_main() {
    let args = StressBinaryArgs::parse();

    // Handle --list mode
    if args.list {
        let benchmarks = list_benchmarks();
        if benchmarks.is_empty() {
            println!("No benchmarks registered.");
            println!("Add #[stress_test] to your benchmark functions.");
        } else {
            println!("Registered benchmarks ({}):", benchmarks.len());
            for name in benchmarks {
                println!("  {}", name);
            }
        }
        return;
    }

    // Build options
    let verbose = if args.quiet {
        false
    } else {
        args.verbose || !args.quiet
    };

    let mut opts = StressRunnerOptions::new()
        .runs(args.runs)
        .warmup(args.warmup)
        .verbose(verbose)
        .include_ignored(args.include_ignored)
        .threshold(args.threshold);

    if let Some(pattern) = args.workload {
        opts = opts.workload(pattern);
    }

    if let Some(baseline) = args.baseline {
        opts = opts.baseline(baseline);
    }

    // Run benchmarks
    run_with_options(opts);
}

// ============================================================================
// Options
// ============================================================================

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

/// Get the benchmark suite name from the executable name.
fn get_suite_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.file_stem().map(|s| s.to_string_lossy().to_string()))
        .map(|name| {
            // Remove cargo's hash suffix (format: name-HASH)
            // The hash is always a hex string with exactly 16 characters
            let clean_name = if let Some(dash_pos) = name.rfind('-') {
                let potential_hash = &name[dash_pos + 1..];
                // Check if it looks like a hash (all hex chars and correct length)
                if potential_hash.len() == 16
                    && potential_hash.chars().all(|c| c.is_ascii_hexdigit())
                {
                    &name[..dash_pos]
                } else {
                    &name
                }
            } else {
                &name
            };

            // Convert underscores to hyphens (cargo converts hyphen to underscore in exe name)
            clean_name.replace('_', "-")
        })
        .unwrap_or_else(|| "stress".to_string())
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

    let suite_name = get_suite_name();
    let mut runner = BenchRunner::with_config(&suite_name, config);

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
        let _results = runner.finish();
        // Summary already printed by ConsoleReporter
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
