//! Pluggable reporters for benchmark output.
//!
//! All reporters implement the `Reporter` trait and are designed to be:
//! - Non-panicking: errors are logged to stderr but never propagate
//! - Atomic: output is written in complete lines to avoid interleaving
//! - Deterministic: identical inputs produce identical outputs

use crate::config::BenchRunnerConfig;
use crate::result::{BenchResult, SuiteResult};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// Trait for benchmark result reporters.
pub trait Reporter: Send + Sync {
    /// Called when a suite starts.
    fn suite_start(&self, _suite: &str, _config: &BenchRunnerConfig) {}

    /// Called when a benchmark starts.
    /// Note: Reporters should NOT print partial output here to ensure atomicity.
    fn bench_start(&self, _name: &str) {}

    /// Called when a benchmark completes.
    fn bench_end(&self, _result: &BenchResult) {}

    /// Called when a suite completes.
    fn suite_end(&self, _result: &SuiteResult) {}
}

/// Fixed width for benchmark name column in console output.
const NAME_WIDTH: usize = 40;
/// Fixed width for duration column in console output.
const DURATION_WIDTH: usize = 14;

/// Console reporter that prints results to stdout.
///
/// Output is atomic: each benchmark is printed as a single complete line
/// in `bench_end`, ensuring logs cannot interleave even if a benchmark panics.
pub struct ConsoleReporter {
    show_all_runs: bool,
    /// Mutex ensures atomic writes across threads.
    output_lock: Mutex<()>,
}

impl ConsoleReporter {
    pub fn new() -> Self {
        Self {
            show_all_runs: false,
            output_lock: Mutex::new(()),
        }
    }

    /// Show individual run times (not just median) on a separate indented line.
    pub fn show_all_runs(mut self, show: bool) -> Self {
        self.show_all_runs = show;
        self
    }

    /// Format a duration with consistent units: ns, µs, ms, or s.
    /// Always uses 2 decimal places, no scientific notation.
    fn format_duration(d: std::time::Duration) -> String {
        format_duration(d)
    }

    /// Format throughput string, only if bytes or elements are set.
    /// Returns empty string if neither is set.
    fn format_throughput(result: &BenchResult) -> String {
        // Bytes take precedence over elements for throughput display
        if let Some(bps) = result.bytes_per_sec() {
            if bps >= 1_000_000_000.0 {
                format!("{:.2} GB/s", bps / 1_000_000_000.0)
            } else if bps >= 1_000_000.0 {
                format!("{:.2} MB/s", bps / 1_000_000.0)
            } else if bps >= 1_000.0 {
                format!("{:.2} KB/s", bps / 1_000.0)
            } else {
                format!("{:.2} B/s", bps)
            }
        } else if let Some(eps) = result.elements_per_sec() {
            if eps >= 1_000_000.0 {
                format!("{:.2}M ops/s", eps / 1_000_000.0)
            } else if eps >= 1_000.0 {
                format!("{:.2}K ops/s", eps / 1_000.0)
            } else {
                format!("{:.0} ops/s", eps)
            }
        } else {
            String::new()
        }
    }

    /// Atomically write a complete message to stdout.
    /// Never panics; logs warning on error.
    fn write_stdout(&self, message: &str) {
        // Acquire lock to ensure atomicity; ignore poison (another thread panicked)
        let _guard = self.output_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut stdout = std::io::stdout().lock();
        if let Err(e) = writeln!(stdout, "{}", message) {
            let _ = writeln!(
                std::io::stderr(),
                "Warning: failed to write to stdout: {}",
                e
            );
        }
    }
}

impl Default for ConsoleReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl Reporter for ConsoleReporter {
    fn suite_start(&self, suite: &str, config: &BenchRunnerConfig) {
        // Build complete header atomically
        let header = format!(
            "---------------------------------------------------------------\n\
             Benchmark Suite: {}\n\
             Runs: {}, Warmup: {}\n\
             ---------------------------------------------------------------\n",
            suite, config.runs, config.warmup_runs
        );
        self.write_stdout(&header);
    }

    fn bench_start(&self, _name: &str) {
        // Intentionally empty: we print the complete line in bench_end
        // to ensure atomic output that cannot interleave.
    }

    fn bench_end(&self, result: &BenchResult) {
        // Extract just the benchmark name (remove suite prefix)
        let name_parts: Vec<&str> = result.name.split('/').collect();
        let bench_name = if name_parts.len() >= 2 {
            name_parts[name_parts.len() - 1]
        } else {
            &result.name
        };

        // Format duration (median from BenchResult)
        let duration_str = Self::format_duration(result.duration);

        // Format throughput only if bytes or elements are set
        let throughput_str = Self::format_throughput(result);

        // Build the main result line with fixed column alignment
        let mut line = if throughput_str.is_empty() {
            format!(
                "  {:<width$} {:>dur_width$}",
                bench_name,
                duration_str,
                width = NAME_WIDTH,
                dur_width = DURATION_WIDTH
            )
        } else {
            format!(
                "  {:<width$} {:>dur_width$}  ({})",
                bench_name,
                duration_str,
                throughput_str,
                width = NAME_WIDTH,
                dur_width = DURATION_WIDTH
            )
        };

        // Optionally append individual runs on a separate indented line
        if self.show_all_runs && result.all_runs.len() > 1 {
            let runs_formatted: Vec<_> = result
                .all_runs
                .iter()
                .map(|d| Self::format_duration(*d))
                .collect();
            line.push_str(&format!("\n      runs: [{}]", runs_formatted.join(", ")));
        }

        self.write_stdout(&line);
    }

    fn suite_end(&self, result: &SuiteResult) {
        let footer = format!(
            "---------------------------------------------------------------\n\
             Completed {} benchmarks in {}\n\
             ---------------------------------------------------------------\n",
            result.results.len(),
            Self::format_duration(result.total_duration)
        );
        self.write_stdout(&footer);
    }
}

/// JSON reporter that writes results to a file.
///
/// Writes timestamped results files organized by suite:
/// - `{suite}/{timestamp}.json` - Machine-readable results
/// - `{suite}/{timestamp}.txt` - Human-readable summary
/// - `{suite}/latest.json` and `latest.txt` - Most recent results
pub struct JsonReporter {
    output_dir: PathBuf,
}

impl JsonReporter {
    pub fn new(output_dir: impl Into<PathBuf>) -> Self {
        Self {
            output_dir: output_dir.into(),
        }
    }

    /// Write JSON results to the output directory.
    /// Creates both timestamped JSON and text summary files organized by suite name.
    /// Never panics; logs warnings to stderr on failure.
    fn write_results(&self, result: &SuiteResult) {
        if let Err(e) = self.write_results_inner(result) {
            eprintln!("Warning: failed to write results: {}", e);
        }
    }

    fn write_results_inner(&self, result: &SuiteResult) -> std::io::Result<()> {
        // Sanitize suite name for directory (replace path separators)
        let sanitized_name = result.suite.replace(['/', '\\'], "_");

        // Create suite-specific subdirectory
        let suite_dir = self.output_dir.join(&sanitized_name);
        std::fs::create_dir_all(&suite_dir)?;

        // Create timestamped filename to make each run unique
        let timestamp_str = &result.started_at;
        let json_filename = format!("{}.json", timestamp_str);
        let txt_filename = format!("{}.txt", timestamp_str);

        let timestamped_json_path = suite_dir.join(&json_filename);
        let timestamped_txt_path = suite_dir.join(&txt_filename);

        // Serialize to JSON
        let json = serde_json::to_string_pretty(result).map_err(std::io::Error::other)?;

        // Write timestamped JSON file
        std::fs::write(&timestamped_json_path, &json)?;
        eprintln!("  Results written to: {}", timestamped_json_path.display());

        // Generate and write text summary
        let summary = self.format_summary(result);
        std::fs::write(&timestamped_txt_path, &summary)?;

        // Write latest files for convenient access to most recent results
        let latest_json_path = suite_dir.join("latest.json");
        let latest_txt_path = suite_dir.join("latest.txt");
        std::fs::write(&latest_json_path, &json)?;
        std::fs::write(&latest_txt_path, &summary)?;
        eprintln!("  Latest results at: {}", latest_json_path.display());

        Ok(())
    }

    fn format_summary(&self, result: &SuiteResult) -> String {
        let mut output = String::new();
        output.push_str("===============================================================\n");
        output.push_str(&format!("Benchmark Suite: {}\n", result.suite));
        output.push_str("===============================================================\n\n");

        output.push_str(&format!("Completed: {}\n", result.started_at));
        if let Some(sha) = &result.git_sha {
            output.push_str(&format!("Git SHA:   {}\n", sha));
        }
        output.push('\n');

        output.push_str("Results:\n");
        output.push_str("---------------------------------------------------------------\n");

        for result in &result.results {
            // Extract just the benchmark name (remove suite prefix)
            let name_parts: Vec<&str> = result.name.split('/').collect();
            let bench_name = if name_parts.len() >= 2 {
                name_parts[name_parts.len() - 1]
            } else {
                &result.name
            };

            // Format duration
            let duration_str = format_duration(result.duration);

            // Format throughput using same logic as console reporter
            let throughput_str = if let Some(_bps) = result.bytes_per_sec() {
                let bps = result.bytes_per_sec().unwrap();
                if bps >= 1_000_000_000.0 {
                    format!("  ({:.2} GB/s)", bps / 1_000_000_000.0)
                } else if bps >= 1_000_000.0 {
                    format!("  ({:.2} MB/s)", bps / 1_000_000.0)
                } else if bps >= 1_000.0 {
                    format!("  ({:.2} KB/s)", bps / 1_000.0)
                } else {
                    format!("  ({:.2} B/s)", bps)
                }
            } else if let Some(_eps) = result.elements_per_sec() {
                let eps = result.elements_per_sec().unwrap();
                if eps >= 1_000_000.0 {
                    format!("  ({:.2}M ops/s)", eps / 1_000_000.0)
                } else if eps >= 1_000.0 {
                    format!("  ({:.2}K ops/s)", eps / 1_000.0)
                } else {
                    format!("  ({:.0} ops/s)", eps)
                }
            } else {
                String::new()
            };

            output.push_str(&format!(
                "  {:<width$} {:>dur_width$}{}
",
                bench_name,
                duration_str,
                throughput_str,
                width = NAME_WIDTH,
                dur_width = DURATION_WIDTH
            ));
        }

        output.push_str("---------------------------------------------------------------\n");
        output.push_str(&format!(
            "Total time: {}\n",
            format_duration(result.total_duration)
        ));
        output.push_str(&format!("Benchmarks: {}\n", result.results.len()));
        output.push_str("===============================================================\n");

        output
    }
}

fn format_duration(nanos: std::time::Duration) -> String {
    let secs = nanos.as_secs_f64();
    if secs >= 1.0 {
        format!("{:.2}s", secs)
    } else if secs >= 0.001 {
        format!("{:.2}ms", secs * 1_000.0)
    } else if secs >= 0.000_001 {
        format!("{:.2}us", secs * 1_000_000.0)
    } else {
        format!("{:.2}ns", secs * 1_000_000_000.0)
    }
}

impl Reporter for JsonReporter {
    fn suite_end(&self, result: &SuiteResult) {
        self.write_results(result);
    }
}

/// GitHub Actions reporter that emits annotations.
///
/// Only produces output when running in GitHub Actions environment.
/// Emits warnings for performance regressions that exceed the threshold.
/// Output goes to stdout (as required by GitHub Actions annotation format).
#[allow(dead_code)]
pub struct GitHubActionsReporter {
    threshold: f64,
    baseline: Option<SuiteResult>,
}

#[allow(dead_code)]
impl GitHubActionsReporter {
    /// Create a new GitHub Actions reporter.
    ///
    /// `threshold` is the regression threshold (e.g., 0.05 for 5%).
    pub fn new(threshold: f64) -> Self {
        Self {
            threshold,
            baseline: None,
        }
    }

    /// Load baseline from a file for comparison.
    pub fn with_baseline(mut self, path: impl AsRef<std::path::Path>) -> Self {
        match SuiteResult::load(path.as_ref()) {
            Ok(baseline) => self.baseline = Some(baseline),
            Err(e) => {
                // Log to stderr, don't fail - baseline is optional
                eprintln!(
                    "Warning: failed to load baseline from '{}': {}",
                    path.as_ref().display(),
                    e
                );
            }
        }
        self
    }

    /// Check if we're running in GitHub Actions environment.
    fn is_github_actions() -> bool {
        std::env::var("GITHUB_ACTIONS").is_ok()
    }

    /// Format duration consistently for GitHub Actions output.
    fn format_duration(d: std::time::Duration) -> String {
        let secs = d.as_secs_f64();
        if secs >= 1.0 {
            format!("{:.2}s", secs)
        } else {
            format!("{:.2}ms", secs * 1000.0)
        }
    }
}

impl Reporter for GitHubActionsReporter {
    fn suite_end(&self, result: &SuiteResult) {
        // Only emit when running in GitHub Actions
        if !Self::is_github_actions() {
            return;
        }

        // Emit regression warnings if baseline is available
        if let Some(baseline) = &self.baseline {
            let regressions = result.find_regressions(baseline, self.threshold);
            for (r, ratio) in regressions {
                let pct = (ratio - 1.0) * 100.0;
                // Include suite name and benchmark name in annotation
                // Format: ::warning title=<title>::<message>
                println!(
                    "::warning title=Performance Regression in {}::Benchmark '{}' is {:.1}% slower than baseline",
                    result.suite, r.name, pct
                );
            }
        }

        // Output summary in a collapsible group
        println!("::group::Benchmark Results - {}", result.suite);
        for r in &result.results {
            let duration = Self::format_duration(r.duration);
            println!("  {}: {}", r.name, duration);
        }
        println!("::endgroup::");
    }
}

/// Combines multiple reporters.
///
/// Delegates all reporter calls to each contained reporter.
/// Errors in one reporter do not affect others.
pub struct MultiReporter {
    reporters: Vec<Box<dyn Reporter>>,
}

impl MultiReporter {
    pub fn new(reporters: Vec<Box<dyn Reporter>>) -> Self {
        Self { reporters }
    }
}

impl Reporter for MultiReporter {
    fn suite_start(&self, suite: &str, config: &BenchRunnerConfig) {
        for r in &self.reporters {
            // Catch panics to ensure one reporter failure doesn't affect others
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                r.suite_start(suite, config);
            }));
        }
    }

    fn bench_start(&self, name: &str) {
        for r in &self.reporters {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                r.bench_start(name);
            }));
        }
    }

    fn bench_end(&self, result: &BenchResult) {
        for r in &self.reporters {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                r.bench_end(result);
            }));
        }
    }

    fn suite_end(&self, result: &SuiteResult) {
        for r in &self.reporters {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                r.suite_end(result);
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    #[test]
    fn should_format_duration_in_appropriate_units() {
        // Seconds for durations >= 1s
        assert!(ConsoleReporter::format_duration(Duration::from_secs(2)).contains("s"));
        assert!(!ConsoleReporter::format_duration(Duration::from_secs(2)).contains("ms"));

        // Milliseconds for durations >= 1ms and < 1s
        assert!(ConsoleReporter::format_duration(Duration::from_millis(500)).contains("ms"));

        // Microseconds for durations < 1ms
        assert!(ConsoleReporter::format_duration(Duration::from_micros(100)).contains("us"));
    }

    #[test]
    fn should_format_duration_with_two_decimals() {
        let d = Duration::from_secs_f64(1.234567);
        let formatted = ConsoleReporter::format_duration(d);
        assert_eq!(formatted, "1.23s");

        let d = Duration::from_secs_f64(0.123456);
        let formatted = ConsoleReporter::format_duration(d);
        assert_eq!(formatted, "123.46ms");

        let d = Duration::from_secs_f64(0.000123456);
        let formatted = ConsoleReporter::format_duration(d);
        assert_eq!(formatted, "123.46us");
    }

    #[test]
    fn should_format_throughput_when_bytes_set() {
        let result = BenchResult {
            name: "test".to_string(),
            duration: Duration::from_secs(1),
            bytes: Some(1_000_000_000),
            elements: None,
            all_runs: vec![],
            tags: HashMap::new(),
        };
        let throughput = ConsoleReporter::format_throughput(&result);
        assert!(throughput.contains("GB/s"));
    }

    #[test]
    fn should_format_throughput_when_elements_set() {
        let result = BenchResult {
            name: "test".to_string(),
            duration: Duration::from_secs(1),
            bytes: None,
            elements: Some(1_000_000),
            all_runs: vec![],
            tags: HashMap::new(),
        };
        let throughput = ConsoleReporter::format_throughput(&result);
        assert!(throughput.contains("ops/s"));
    }

    #[test]
    fn should_return_empty_throughput_when_neither_set() {
        let result = BenchResult {
            name: "test".to_string(),
            duration: Duration::from_secs(1),
            bytes: None,
            elements: None,
            all_runs: vec![],
            tags: HashMap::new(),
        };
        let throughput = ConsoleReporter::format_throughput(&result);
        assert!(throughput.is_empty());
    }

    #[test]
    fn should_prefer_bytes_over_elements_for_throughput() {
        let result = BenchResult {
            name: "test".to_string(),
            duration: Duration::from_secs(1),
            bytes: Some(1_000_000),
            elements: Some(500),
            all_runs: vec![],
            tags: HashMap::new(),
        };
        let throughput = ConsoleReporter::format_throughput(&result);
        // Should show bytes throughput, not elements
        assert!(throughput.contains("MB/s") || throughput.contains("KB/s"));
        assert!(!throughput.contains("ops/s"));
    }
}
