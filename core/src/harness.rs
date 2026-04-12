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
use std::collections::HashMap;
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
#[derive(Debug, Clone, Default)]
struct StressBinaryArgs {
    /// Filter benchmarks by glob pattern
    workload: Option<String>,
    /// Number of measurement runs
    runs: Option<usize>,
    /// Number of warmup runs
    warmup: Option<usize>,
    /// Verbose output
    verbose: Option<bool>,
    /// Include ignored benchmarks
    include_ignored: Option<bool>,
    /// List benchmarks without running
    list: bool,
    /// Print resolved config without running benchmarks
    print_config: bool,
    /// Output directory for JSON results
    output_dir: Option<PathBuf>,
    /// Baseline JSON for regression comparison
    baseline: Option<PathBuf>,
    /// Regression threshold
    threshold: Option<f64>,
}

#[derive(Debug, Clone)]
struct ResolvedStressConfig {
    config: BenchRunnerConfig,
    metadata: HashMap<String, String>,
    warnings: Vec<String>,
    workload: Option<String>,
    include_ignored: bool,
    baseline: Option<PathBuf>,
    threshold: f64,
    print_config: bool,
}

impl StressBinaryArgs {
    /// Parse command-line arguments.
    ///
    /// We use a simple hand-rolled parser to avoid adding clap as a dependency
    /// for every stress binary. The argument format matches what cargo-stress passes.
    fn parse() -> Self {
        let args: Vec<String> = std::env::args().collect();
        Self::parse_from_args(&args)
    }

    fn parse_from_args(args: &[String]) -> Self {
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
                        if let Ok(runs) = args[i].parse() {
                            result.runs = Some(runs);
                        }
                    }
                }
                "--warmup" => {
                    i += 1;
                    if i < args.len() {
                        if let Ok(warmup) = args[i].parse() {
                            result.warmup = Some(warmup);
                        }
                    }
                }
                "--verbose" | "-v" => {
                    result.verbose = Some(true);
                }
                "--quiet" | "-q" => {
                    result.verbose = Some(false);
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
                        if let Ok(threshold) = args[i].parse() {
                            result.threshold = Some(threshold);
                        }
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
    eprintln!(
        "    --runs <N>             Number of measurement runs (fallback: BENCH_RUNS, then 1)"
    );
    eprintln!("    --warmup <N>           Number of warmup runs (fallback: BENCH_WARMUP, then 0)");
    eprintln!("    -v, --verbose          Verbose output (fallback: BENCH_VERBOSE, then true)");
    eprintln!("    -q, --quiet            Quiet mode (overrides BENCH_VERBOSE)");
    eprintln!(
        "    --include-ignored      Include ignored benchmarks (fallback: BENCH_INCLUDE_IGNORED)"
    );
    eprintln!("    --list                 List benchmarks without running");
    eprintln!("    --print-config         Print resolved config and exit");
    eprintln!(
        "    --output-dir <PATH>    Output directory for JSON results (fallback: BENCH_OUTPUT_DIR)"
    );
    eprintln!("    --baseline <PATH>      Baseline JSON for regression comparison (fallback: BENCH_BASELINE)");
    eprintln!(
        "    --threshold <FLOAT>    Regression threshold (fallback: BENCH_THRESHOLD, then 0.05)"
    );
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
    run_from_env_and_args();
}

pub fn run_from_env_and_args() {
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

    let resolved = resolve_from_binary_args(&args);
    for warning in &resolved.warnings {
        eprintln!("Warning: {}", warning);
    }

    if resolved.print_config {
        print_resolved_config(&get_suite_name(), &resolved);
        return;
    }

    run_with_resolved_config(resolved);
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
    let mut metadata = HashMap::new();
    metadata.insert(
        "runs_src".to_string(),
        source_label(opts.runs.is_some(), "cli --runs"),
    );
    metadata.insert(
        "warmup_runs_src".to_string(),
        source_label(opts.warmup.is_some(), "cli --warmup"),
    );
    metadata.insert("verbose_src".to_string(), "explicit option".to_string());
    metadata.insert("output_dir_src".to_string(), "default".to_string());
    metadata.insert("filter_src".to_string(), "default".to_string());
    metadata.insert("timeout_secs_src".to_string(), "default".to_string());

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
    apply_runner_option_overrides(&mut config, &opts);

    let suite_name = get_suite_name();
    let mut runner = BenchRunner::with_config_and_metadata(&suite_name, config, metadata);

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

fn resolve_from_binary_args(args: &StressBinaryArgs) -> ResolvedStressConfig {
    resolve_from_binary_args_with(args, |key| std::env::var(key).ok())
}

fn resolve_from_binary_args_with<F>(args: &StressBinaryArgs, get_var: F) -> ResolvedStressConfig
where
    F: Fn(&str) -> Option<String>,
{
    let env_resolution = BenchRunnerConfig::resolve_from_env_with(&get_var);
    let mut config = env_resolution.config;
    let mut metadata = env_resolution.metadata;
    let mut warnings = env_resolution.warnings;

    let mut include_ignored = false;
    metadata.insert("include_ignored_src".to_string(), "default".to_string());
    if let Some(v) = get_var("BENCH_INCLUDE_IGNORED") {
        match parse_bool_env(&v) {
            Some(value) => {
                include_ignored = value;
                metadata.insert(
                    "include_ignored_src".to_string(),
                    "env BENCH_INCLUDE_IGNORED".to_string(),
                );
            }
            None => warnings.push("invalid BENCH_INCLUDE_IGNORED, using default false".to_string()),
        }
    }

    let mut baseline = None;
    metadata.insert("baseline_src".to_string(), "default".to_string());
    if let Some(v) = get_var("BENCH_BASELINE") {
        baseline = Some(PathBuf::from(v));
        metadata.insert("baseline_src".to_string(), "env BENCH_BASELINE".to_string());
    }

    let mut threshold = 0.05;
    metadata.insert("threshold_src".to_string(), "default".to_string());
    if let Some(v) = get_var("BENCH_THRESHOLD") {
        match v.parse::<f64>() {
            Ok(value) => {
                threshold = value;
                metadata.insert(
                    "threshold_src".to_string(),
                    "env BENCH_THRESHOLD".to_string(),
                );
            }
            Err(_) => warnings.push("invalid BENCH_THRESHOLD, using default 0.05".to_string()),
        }
    }

    if let Some(runs) = args.runs {
        config.runs = runs;
        metadata.insert("runs_src".to_string(), "cli --runs".to_string());
    }
    if let Some(warmup) = args.warmup {
        config.warmup_runs = warmup;
        metadata.insert("warmup_runs_src".to_string(), "cli --warmup".to_string());
    }
    if let Some(verbose) = args.verbose {
        config.verbose = verbose;
        metadata.insert(
            "verbose_src".to_string(),
            if verbose {
                "cli --verbose".to_string()
            } else {
                "cli --quiet".to_string()
            },
        );
    }
    if let Some(output_dir) = &args.output_dir {
        config.output_dir = output_dir.clone();
        metadata.insert("output_dir_src".to_string(), "cli --output-dir".to_string());
    }
    if let Some(pattern) = &args.workload {
        metadata.insert("filter".to_string(), pattern.clone());
        metadata.insert("filter_src".to_string(), "cli --workload".to_string());
    } else if let Some(filter) = &config.filter {
        metadata.insert("filter".to_string(), filter.clone());
    }
    if let Some(include) = args.include_ignored {
        include_ignored = include;
        metadata.insert(
            "include_ignored_src".to_string(),
            "cli --include-ignored".to_string(),
        );
    }
    if let Some(path) = &args.baseline {
        baseline = Some(path.clone());
        metadata.insert("baseline_src".to_string(), "cli --baseline".to_string());
    }
    if let Some(value) = args.threshold {
        threshold = value;
        metadata.insert("threshold_src".to_string(), "cli --threshold".to_string());
    }

    ResolvedStressConfig {
        config,
        metadata,
        warnings,
        workload: args.workload.clone(),
        include_ignored,
        baseline,
        threshold,
        print_config: args.print_config,
    }
}

fn run_with_resolved_config(resolved: ResolvedStressConfig) {
    let benchmarks: Vec<_> = STRESS_BENCHMARKS
        .iter()
        .filter(|b| {
            if b.ignored && !resolved.include_ignored {
                return false;
            }
            if let Some(ref pattern) = resolved.workload {
                return matches_glob(b.name, pattern) || matches_glob(b.module_path, pattern);
            }
            true
        })
        .collect();

    if benchmarks.is_empty() {
        if resolved.workload.is_some() {
            eprintln!("No benchmarks matched the workload pattern");
        } else {
            eprintln!("No benchmarks registered. Add #[stress_test] to your benchmark functions.");
        }
        return;
    }

    let suite_name = get_suite_name();
    let mut runner =
        BenchRunner::with_config_and_metadata(&suite_name, resolved.config, resolved.metadata);

    for bench in &benchmarks {
        let name = format!("{}::{}", bench.module_path, bench.name);
        runner.run(&name, bench.func);
    }

    if let Some(baseline_path) = resolved.baseline {
        let (_results, regressions) =
            runner.finish_with_baseline(baseline_path, resolved.threshold);
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
    }
}

fn print_resolved_config(suite: &str, resolved: &ResolvedStressConfig) {
    println!("Benchmark Suite: {}", suite);
    println!(
        "Runs: {} ({})",
        resolved.config.runs,
        resolved
            .metadata
            .get("runs_src")
            .map(String::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "Warmup: {} ({})",
        resolved.config.warmup_runs,
        resolved
            .metadata
            .get("warmup_runs_src")
            .map(String::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "Output: {} ({})",
        resolved.config.output_dir.display(),
        resolved
            .metadata
            .get("output_dir_src")
            .map(String::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "Filter: {} ({})",
        resolved
            .metadata
            .get("filter")
            .map(String::as_str)
            .unwrap_or("<none>"),
        resolved
            .metadata
            .get("filter_src")
            .map(String::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "Verbose: {} ({})",
        resolved.config.verbose,
        resolved
            .metadata
            .get("verbose_src")
            .map(String::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "Timeout: {} ({})",
        resolved
            .config
            .timeout
            .map(|timeout| format!("{}s", timeout.as_secs()))
            .unwrap_or_else(|| "<none>".to_string()),
        resolved
            .metadata
            .get("timeout_secs_src")
            .map(String::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "Include ignored: {} ({})",
        resolved.include_ignored,
        resolved
            .metadata
            .get("include_ignored_src")
            .map(String::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "Baseline: {} ({})",
        resolved
            .baseline
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<none>".to_string()),
        resolved
            .metadata
            .get("baseline_src")
            .map(String::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "Threshold: {} ({})",
        resolved.threshold,
        resolved
            .metadata
            .get("threshold_src")
            .map(String::as_str)
            .unwrap_or("unknown")
    );
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

fn source_label(explicit: bool, label: &str) -> String {
    if explicit {
        label.to_string()
    } else {
        "default".to_string()
    }
}

fn apply_runner_option_overrides(config: &mut BenchRunnerConfig, opts: &StressRunnerOptions) {
    if let Some(r) = opts.runs {
        config.runs = r;
    }
    if let Some(w) = opts.warmup {
        config.warmup_runs = w;
    }
    config.verbose = opts.verbose;
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

    #[test]
    fn parse_args_keeps_runs_and_warmup_unset_when_not_provided() {
        let args = vec!["stress-demo".to_string(), "--list".to_string()];
        let parsed = StressBinaryArgs::parse_from_args(&args);
        assert_eq!(parsed.runs, None);
        assert_eq!(parsed.warmup, None);
    }

    #[test]
    fn parse_args_sets_runs_and_warmup_only_when_provided() {
        let args = vec![
            "stress-demo".to_string(),
            "--runs".to_string(),
            "4".to_string(),
            "--warmup".to_string(),
            "2".to_string(),
        ];
        let parsed = StressBinaryArgs::parse_from_args(&args);
        assert_eq!(parsed.runs, Some(4));
        assert_eq!(parsed.warmup, Some(2));
    }

    #[test]
    fn options_without_cli_values_keep_base_config_values() {
        let mut cfg = BenchRunnerConfig::new().runs(3).warmup(1).verbose(true);
        let opts = StressRunnerOptions::new().verbose(false);
        apply_runner_option_overrides(&mut cfg, &opts);
        assert_eq!(cfg.runs, 3);
        assert_eq!(cfg.warmup_runs, 1);
        assert!(!cfg.verbose);
    }

    #[test]
    fn cli_options_override_base_config_values() {
        let mut cfg = BenchRunnerConfig::new().runs(3).warmup(1).verbose(true);
        let opts = StressRunnerOptions::new().runs(1).warmup(0).verbose(false);
        apply_runner_option_overrides(&mut cfg, &opts);
        assert_eq!(cfg.runs, 1);
        assert_eq!(cfg.warmup_runs, 0);
        assert!(!cfg.verbose);
    }

    #[test]
    fn parse_plus_option_override_matches_expected_precedence_flow() {
        // Simulate env-derived base config and explicit CLI override.
        let mut cfg = BenchRunnerConfig::new().runs(3).warmup(2);
        let args = vec![
            "stress-demo".to_string(),
            "--runs".to_string(),
            "1".to_string(),
        ];
        let parsed = StressBinaryArgs::parse_from_args(&args);

        let mut opts = StressRunnerOptions::new();
        if let Some(runs) = parsed.runs {
            opts = opts.runs(runs);
        }
        if let Some(warmup) = parsed.warmup {
            opts = opts.warmup(warmup);
        }

        apply_runner_option_overrides(&mut cfg, &opts);

        assert_eq!(cfg.runs, 1);
        assert_eq!(cfg.warmup_runs, 2);
    }

    #[test]
    fn resolve_from_binary_args_uses_env_when_cli_absent() {
        let args = StressBinaryArgs::default();
        let env = HashMap::from([
            ("BENCH_RUNS", "3".to_string()),
            ("BENCH_WARMUP", "1".to_string()),
        ]);

        let resolved = resolve_from_binary_args_with(&args, |key| env.get(key).cloned());

        assert_eq!(resolved.config.runs, 3);
        assert_eq!(resolved.config.warmup_runs, 1);
        assert_eq!(
            resolved.metadata.get("runs_src"),
            Some(&"env BENCH_RUNS".to_string())
        );
    }

    #[test]
    fn resolve_from_binary_args_cli_overrides_env() {
        let args = StressBinaryArgs {
            runs: Some(5),
            ..StressBinaryArgs::default()
        };
        let env = HashMap::from([("BENCH_RUNS", "3".to_string())]);

        let resolved = resolve_from_binary_args_with(&args, |key| env.get(key).cloned());

        assert_eq!(resolved.config.runs, 5);
        assert_eq!(
            resolved.metadata.get("runs_src"),
            Some(&"cli --runs".to_string())
        );
    }

    #[test]
    fn resolve_from_binary_args_warns_on_malformed_env() {
        let args = StressBinaryArgs::default();
        let env = HashMap::from([("BENCH_THRESHOLD", "nope".to_string())]);

        let resolved = resolve_from_binary_args_with(&args, |key| env.get(key).cloned());

        assert_eq!(resolved.threshold, 0.05);
        assert!(resolved
            .warnings
            .contains(&"invalid BENCH_THRESHOLD, using default 0.05".to_string()));
    }
}
