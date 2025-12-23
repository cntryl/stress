//! Pluggable reporters for benchmark output.

use crate::config::BenchRunnerConfig;
use crate::result::{BenchResult, SuiteResult};
use std::io::Write;
use std::path::PathBuf;

/// Trait for benchmark result reporters.
pub trait Reporter: Send + Sync {
    /// Called when a suite starts.
    fn suite_start(&self, _suite: &str, _config: &BenchRunnerConfig) {}

    /// Called when a benchmark starts.
    fn bench_start(&self, _name: &str) {}

    /// Called when a benchmark completes.
    fn bench_end(&self, _result: &BenchResult) {}

    /// Called when a suite completes.
    fn suite_end(&self, _result: &SuiteResult) {}
}

/// Console reporter that prints results to stderr.
pub struct ConsoleReporter {
    show_all_runs: bool,
}

impl ConsoleReporter {
    pub fn new() -> Self {
        Self {
            show_all_runs: false,
        }
    }

    /// Show individual run times (not just median).
    pub fn show_all_runs(mut self, show: bool) -> Self {
        self.show_all_runs = show;
        self
    }

    fn format_duration(d: std::time::Duration) -> String {
        if d.as_secs() > 0 {
            format!("{:.2}s", d.as_secs_f64())
        } else if d.as_millis() > 0 {
            format!("{:.2}ms", d.as_secs_f64() * 1000.0)
        } else {
            format!("{:.2}µs", d.as_secs_f64() * 1_000_000.0)
        }
    }

    fn format_throughput(result: &BenchResult) -> String {
        match (result.bytes_per_sec(), result.elements_per_sec()) {
            (Some(bps), _) => {
                if bps >= 1_000_000_000.0 {
                    format!(" ({:.2} GB/s)", bps / 1_000_000_000.0)
                } else if bps >= 1_000_000.0 {
                    format!(" ({:.2} MB/s)", bps / 1_000_000.0)
                } else {
                    format!(" ({:.2} KB/s)", bps / 1_000.0)
                }
            }
            (None, Some(eps)) => {
                if eps > 1_000_000.0 {
                    format!(" ({:.2}M ops/s)", eps / 1_000_000.0)
                } else if eps > 1000.0 {
                    format!(" ({:.2}K ops/s)", eps / 1000.0)
                } else {
                    format!(" ({:.0} ops/s)", eps)
                }
            }
            _ => String::new(),
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
        eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        eprintln!("  Benchmark Suite: {}", suite);
        eprintln!("  Runs: {}, Warmup: {}", config.runs, config.warmup_runs);
        eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    }

    fn bench_start(&self, name: &str) {
        eprint!("  {} ... ", name);
        std::io::stderr().flush().ok();
    }

    fn bench_end(&self, result: &BenchResult) {
        let time_str = Self::format_duration(result.duration);
        let throughput = Self::format_throughput(result);
        eprintln!("{}{}", time_str, throughput);

        if self.show_all_runs && result.all_runs.len() > 1 {
            let runs: Vec<_> = result
                .all_runs
                .iter()
                .map(|d| Self::format_duration(*d))
                .collect();
            eprintln!("      runs: [{}]", runs.join(", "));
        }
    }

    fn suite_end(&self, result: &SuiteResult) {
        eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        eprintln!(
            "  Completed {} benchmarks in {:.2}s",
            result.results.len(),
            result.total_duration.as_secs_f64()
        );
        eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    }
}

/// JSON reporter that writes results to a file.
pub struct JsonReporter {
    output_dir: PathBuf,
}

impl JsonReporter {
    pub fn new(output_dir: impl Into<PathBuf>) -> Self {
        Self {
            output_dir: output_dir.into(),
        }
    }
}

impl Reporter for JsonReporter {
    fn suite_end(&self, result: &SuiteResult) {
        if let Err(e) = write_json_results(&self.output_dir, result) {
            eprintln!("Warning: failed to write JSON results: {}", e);
        }
    }
}

fn write_json_results(output_dir: &PathBuf, result: &SuiteResult) -> std::io::Result<()> {
    std::fs::create_dir_all(output_dir)?;

    let filename = format!("{}.json", result.suite.replace('/', "_"));
    let path = output_dir.join(&filename);

    let json = serde_json::to_string_pretty(result).map_err(std::io::Error::other)?;

    std::fs::write(&path, json)?;
    eprintln!("  Results written to: {}", path.display());

    Ok(())
}

/// GitHub Actions reporter that emits annotations.
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
        self.baseline = SuiteResult::load(path).ok();
        self
    }
}

impl Reporter for GitHubActionsReporter {
    fn suite_end(&self, result: &SuiteResult) {
        // Only emit if running in GitHub Actions
        if std::env::var("GITHUB_ACTIONS").is_err() {
            return;
        }

        if let Some(baseline) = &self.baseline {
            let regressions = result.find_regressions(baseline, self.threshold);
            for (r, ratio) in regressions {
                let pct = (ratio - 1.0) * 100.0;
                println!(
                    "::warning title=Performance Regression::Benchmark '{}' is {:.1}% slower than baseline",
                    r.name, pct
                );
            }
        }

        // Output summary
        println!("::group::Benchmark Results");
        for r in &result.results {
            let duration = if r.duration.as_secs() > 0 {
                format!("{:.2}s", r.duration.as_secs_f64())
            } else {
                format!("{:.2}ms", r.duration.as_secs_f64() * 1000.0)
            };
            println!("  {}: {}", r.name, duration);
        }
        println!("::endgroup::");
    }
}

/// Combines multiple reporters.
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
            r.suite_start(suite, config);
        }
    }

    fn bench_start(&self, name: &str) {
        for r in &self.reporters {
            r.bench_start(name);
        }
    }

    fn bench_end(&self, result: &BenchResult) {
        for r in &self.reporters {
            r.bench_end(result);
        }
    }

    fn suite_end(&self, result: &SuiteResult) {
        for r in &self.reporters {
            r.suite_end(result);
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
        assert!(ConsoleReporter::format_duration(Duration::from_secs(2)).contains("s"));
        assert!(ConsoleReporter::format_duration(Duration::from_millis(500)).contains("ms"));
        assert!(ConsoleReporter::format_duration(Duration::from_micros(100)).contains("µs"));
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
}
