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
        let suite_start = Instant::now();

        // Default reporters: console (always) + JSON
        let reporters: Vec<Box<dyn Reporter>> = vec![
            Box::new(ConsoleReporter::new()),
            Box::new(JsonReporter::new(config.output_dir.clone())),
        ];

        let runner = Self {
            suite: suite.to_string(),
            config,
            results: Vec::new(),
            suite_start,
            reporters,
            metadata: HashMap::new(),
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
    /// The closure must call `ctx.measure()` exactly once.
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
                    "Benchmark '{}' did not call ctx.measure(). \
                     Every benchmark must measure exactly one operation.",
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

        let suite_result = SuiteResult {
            suite: self.suite.clone(),
            results: self.results.clone(),
            total_duration,
            started_at: chrono_timestamp(),
            runs: self.config.runs,
            warmup_runs: self.config.warmup_runs,
            git_sha: self.config.git_sha.clone(),
            metadata: self.metadata,
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
    #[should_panic(expected = "did not call ctx.measure")]
    fn should_panic_when_measure_not_called() {
        let config = BenchRunnerConfig::new().verbose(false);
        let mut runner = BenchRunner::with_config("test", config);
        runner.reporters(vec![]);

        runner.run("bad_bench", |_ctx| {
            // Forgot to call measure!
        });
    }
}
