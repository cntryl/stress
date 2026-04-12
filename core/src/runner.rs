//! The main benchmark runner.

use crate::config::BenchRunnerConfig;
use crate::context::StressContext;
use crate::report::{ConsoleReporter, JsonReporter, Reporter};
use crate::result::{BenchResult, SuiteResult};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Lightweight benchmark runner for single-shot measurements.
///
/// # Example
///
/// ```rust,no_run
/// use cntryl_stress::{BenchRunner, StressContext};
///
/// let mut runner = BenchRunner::new("my_suite");
///
/// runner.run("operation_a", |ctx| {
///     let data = vec![0u8; 1024 * 1024];
///     ctx.set_bytes(data.len() as u64);
///     ctx.measure(|| {
///         std::hint::black_box(&data);
///     });
/// });
///
/// let results = runner.finish();
/// ```
pub struct BenchRunner {
    suite: String,
    config: BenchRunnerConfig,
    results: Vec<BenchResult>,
    suite_start: Instant,
    reporters: Vec<Box<dyn Reporter>>,
    metadata: HashMap<String, String>,
}

impl BenchRunner {
    /// Create a new runner with default config from environment.
    pub fn new(suite: &str) -> Self {
        Self::with_config(suite, BenchRunnerConfig::from_env())
    }

    /// Create a new runner with explicit config.
    pub fn with_config(suite: &str, config: BenchRunnerConfig) -> Self {
        Self::with_config_and_metadata(suite, config, HashMap::new())
    }

    /// Create a new runner with explicit config and initial metadata.
    pub fn with_config_and_metadata(
        suite: &str,
        config: BenchRunnerConfig,
        metadata: HashMap<String, String>,
    ) -> Self {
        let suite_start = Instant::now();

        let mut reporters: Vec<Box<dyn Reporter>> =
            vec![Box::new(JsonReporter::new(config.output_dir.clone()))];

        if config.verbose {
            reporters.insert(
                0,
                Box::new(
                    ConsoleReporter::new()
                        .config_lines(build_suite_config_lines(&config, &metadata)),
                ),
            );
        }

        let runner = Self {
            suite: suite.to_string(),
            config,
            results: Vec::new(),
            suite_start,
            reporters,
            metadata,
        };

        // Notify reporters of suite start
        for r in &runner.reporters {
            r.suite_start(&runner.suite, &runner.config);
        }

        runner
    }

    /// Add custom metadata to the suite results.
    pub fn metadata(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Replace reporters with a custom set.
    pub fn reporters(&mut self, reporters: Vec<Box<dyn Reporter>>) -> &mut Self {
        self.reporters = reporters;
        self
    }

    /// Add an additional reporter.
    pub fn add_reporter(&mut self, reporter: Box<dyn Reporter>) -> &mut Self {
        self.reporters.push(reporter);
        self
    }

    fn should_run(&self, name: &str) -> bool {
        match &self.config.filter {
            Some(f) => name.contains(f.as_str()),
            None => true,
        }
    }

    /// Run a benchmark case.
    ///
    /// The closure must record exactly one duration with `ctx.measure()` or `ctx.measure_for()`.
    pub fn run<F>(&mut self, name: &str, f: F)
    where
        F: Fn(&mut StressContext),
    {
        if !self.should_run(name) {
            return;
        }

        let full_name = format!("{}/{}", self.suite, name);

        // Notify reporters
        for r in &self.reporters {
            r.bench_start(name);
        }

        // Warmup runs
        for _ in 0..self.config.warmup_runs {
            let mut ctx = StressContext::new();
            f(&mut ctx);
        }

        // Measurement runs
        let mut durations = Vec::with_capacity(self.config.runs);
        let mut bytes = None;
        let mut elements = None;
        let mut tags = HashMap::new();

        for _ in 0..self.config.runs {
            let mut ctx = StressContext::new();
            f(&mut ctx);

            if let Some(d) = ctx.duration {
                durations.push(d);
            } else {
                panic!(
                    "Benchmark '{}' did not record a duration. \
                     Call ctx.measure() or ctx.measure_for() exactly once.",
                    name
                );
            }

            bytes = ctx.bytes.or(bytes);
            elements = ctx.elements.or(elements);
            for (k, v) in ctx.tags {
                tags.insert(k, v);
            }
        }

        // Report median
        durations.sort();
        let median = durations[durations.len() / 2];

        let result = BenchResult {
            name: full_name,
            duration: median,
            bytes,
            elements,
            all_runs: durations,
            tags,
        };

        // Notify reporters
        for r in &self.reporters {
            r.bench_end(&result);
        }

        self.results.push(result);
    }

    /// Run multiple related benchmarks as a group.
    ///
    /// Groups are just for organization/reporting.
    pub fn group<F>(&mut self, group_name: &str, f: F)
    where
        F: FnOnce(&mut BenchGroup<'_>),
    {
        let mut group = BenchGroup {
            runner: self,
            prefix: group_name.to_string(),
        };
        f(&mut group);
    }

    /// Finish the suite and return results.
    ///
    /// This writes JSON output and prints summary.
    pub fn finish(self) -> Vec<BenchResult> {
        let total_duration = self.suite_start.elapsed();
        let mut metadata = self.metadata;
        metadata.insert("effective_runs".to_string(), self.config.runs.to_string());
        metadata.insert(
            "effective_warmup".to_string(),
            self.config.warmup_runs.to_string(),
        );
        metadata.insert(
            "output_dir".to_string(),
            self.config.output_dir.display().to_string(),
        );
        metadata.insert("verbose".to_string(), self.config.verbose.to_string());
        if let Some(filter) = &self.config.filter {
            metadata.insert("filter".to_string(), filter.clone());
        }
        if let Some(timeout) = self.config.timeout {
            metadata.insert("timeout_secs".to_string(), timeout.as_secs().to_string());
        }

        let suite_result = SuiteResult {
            suite: self.suite.clone(),
            results: self.results.clone(),
            total_duration,
            started_at: chrono_timestamp(),
            runs: self.config.runs,
            warmup_runs: self.config.warmup_runs,
            git_sha: self.config.git_sha.clone(),
            metadata,
        };

        // Notify reporters
        for r in &self.reporters {
            r.suite_end(&suite_result);
        }

        self.results
    }

    /// Finish and compare against a baseline file.
    ///
    /// Returns both results and any regressions found.
    pub fn finish_with_baseline(
        self,
        baseline_path: impl AsRef<std::path::Path>,
        threshold: f64,
    ) -> (Vec<BenchResult>, Vec<(BenchResult, f64)>) {
        let results = self.finish();

        let regressions = match SuiteResult::load(&baseline_path) {
            Ok(baseline) => {
                let current = SuiteResult {
                    suite: String::new(),
                    results: results.clone(),
                    total_duration: Duration::ZERO,
                    started_at: String::new(),
                    runs: 0,
                    warmup_runs: 0,
                    git_sha: None,
                    metadata: HashMap::new(),
                };
                current
                    .find_regressions(&baseline, threshold)
                    .into_iter()
                    .map(|(r, ratio)| (r.clone(), ratio))
                    .collect()
            }
            Err(_) => Vec::new(),
        };

        (results, regressions)
    }
}

fn build_suite_config_lines(
    config: &BenchRunnerConfig,
    metadata: &HashMap<String, String>,
) -> Vec<String> {
    let mut lines = vec![
        format!(
            "Runs: {} ({})",
            config.runs,
            metadata
                .get("runs_src")
                .map(String::as_str)
                .unwrap_or("unknown")
        ),
        format!(
            "Warmup: {} ({})",
            config.warmup_runs,
            metadata
                .get("warmup_runs_src")
                .map(String::as_str)
                .unwrap_or("unknown")
        ),
        format!(
            "Output: {} ({})",
            config.output_dir.display(),
            metadata
                .get("output_dir_src")
                .map(String::as_str)
                .unwrap_or("unknown")
        ),
        format!(
            "Verbose: {} ({})",
            config.verbose,
            metadata
                .get("verbose_src")
                .map(String::as_str)
                .unwrap_or("unknown")
        ),
    ];

    let filter_value = metadata
        .get("filter")
        .map(String::as_str)
        .or(config.filter.as_deref())
        .unwrap_or("<none>");
    lines.push(format!(
        "Filter: {} ({})",
        filter_value,
        metadata
            .get("filter_src")
            .map(String::as_str)
            .unwrap_or("unknown")
    ));

    let timeout_value = config
        .timeout
        .map(|timeout| format!("{}s", timeout.as_secs()))
        .unwrap_or_else(|| "<none>".to_string());
    lines.push(format!(
        "Timeout: {} ({})",
        timeout_value,
        metadata
            .get("timeout_secs_src")
            .map(String::as_str)
            .unwrap_or("unknown")
    ));

    lines
}

/// A benchmark group for organizing related benchmarks.
pub struct BenchGroup<'a> {
    runner: &'a mut BenchRunner,
    prefix: String,
}

impl<'a> BenchGroup<'a> {
    /// Run a benchmark within this group.
    pub fn run<F>(&mut self, name: &str, f: F)
    where
        F: Fn(&mut StressContext),
    {
        let full_name = format!("{}/{}", self.prefix, name);
        self.runner.run(&full_name, f);
    }
}

fn chrono_timestamp() -> String {
    // Return a compact unique timestamp (unix seconds with millisecond precision)
    // This works well for both filenames and JSON values
    let now = std::time::SystemTime::now();
    let duration = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    // Use milliseconds for better uniqueness when multiple runs happen quickly
    format!("{}", duration.as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_run_benchmark_when_no_filter() {
        let config = BenchRunnerConfig::new().verbose(false);
        let mut runner = BenchRunner::with_config("test", config);
        runner.reporters(vec![]); // Disable reporters for test

        runner.run("bench1", |ctx| {
            ctx.measure(|| {});
        });

        let results = runner.finish();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "test/bench1");
    }

    #[test]
    fn should_filter_benchmarks_when_filter_set() {
        let config = BenchRunnerConfig::new().verbose(false).filter("keep");
        let mut runner = BenchRunner::with_config("test", config);
        runner.reporters(vec![]);

        runner.run("keep_this", |ctx| {
            ctx.measure(|| {});
        });
        runner.run("skip_this", |ctx| {
            ctx.measure(|| {});
        });

        let results = runner.finish();
        assert_eq!(results.len(), 1);
        assert!(results[0].name.contains("keep"));
    }

    #[test]
    #[should_panic(expected = "did not record a duration")]
    fn should_panic_when_measure_not_called() {
        let config = BenchRunnerConfig::new().verbose(false);
        let mut runner = BenchRunner::with_config("test", config);
        runner.reporters(vec![]);

        runner.run("bad_bench", |_ctx| {
            // Forgot to call measure!
        });
    }
}
