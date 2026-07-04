//! Pluggable reporters for v2 stress artifacts.

use crate::config::StressRunnerConfig;
use crate::result::{BenchmarkSummary, ComparisonClass, PrimaryMetric, QualityClass, StressRun};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// Trait for benchmark result reporters.
pub trait Reporter: Send + Sync {
    /// Called when a suite starts.
    fn suite_start(&self, _suite: &str, _config: &StressRunnerConfig) {}

    /// Called when a benchmark summary is available.
    fn bench_end(&self, _summary: &BenchmarkSummary) {}

    /// Called when a suite completes.
    fn suite_end(&self, _run: &StressRun) {}
}

const NAME_WIDTH: usize = 48;
const VALUE_WIDTH: usize = 16;

/// Console reporter that prints compact progress to stdout.
pub struct ConsoleReporter {
    suite_config_lines: Vec<String>,
    output_lock: Mutex<()>,
}

impl ConsoleReporter {
    /// Create a console reporter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            suite_config_lines: Vec::new(),
            output_lock: Mutex::new(()),
        }
    }

    /// Attach resolved config lines to the suite header.
    #[must_use]
    pub fn config_lines(mut self, lines: Vec<String>) -> Self {
        self.suite_config_lines = lines;
        self
    }

    fn write_stdout(&self, message: &str) {
        let _guard = self
            .output_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut stdout = std::io::stdout().lock();
        if let Err(error) = writeln!(stdout, "{message}") {
            let _ = writeln!(
                std::io::stderr(),
                "Warning: failed to write to stdout: {error}"
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
    fn suite_start(&self, suite: &str, config: &StressRunnerConfig) {
        let details = if self.suite_config_lines.is_empty() {
            default_config_lines(config).join("\n")
        } else {
            self.suite_config_lines.join("\n")
        };
        let header = format!(
            "---------------------------------------------------------------\n\
             Benchmark Suite: {suite}\n\
             {details}\n\
             ---------------------------------------------------------------"
        );
        self.write_stdout(&header);
    }

    fn bench_end(&self, summary: &BenchmarkSummary) {
        let value = summary.primary_value().map_or_else(
            || "n/a".to_string(),
            |value| format_metric(value, summary.primary_metric),
        );
        let line = format!(
            "  {name:<NAME_WIDTH$} {value:>VALUE_WIDTH$}  tier={tier} quality={quality}",
            name = summary.name,
            tier = summary.tier,
            quality = summary.quality
        );
        self.write_stdout(&line);
    }

    fn suite_end(&self, run: &StressRun) {
        let footer = format!(
            "---------------------------------------------------------------\n\
             Completed {} benchmark(s), {} sample row(s) in {}\n\
             ---------------------------------------------------------------",
            run.summaries.len(),
            run.samples.len(),
            format_duration_ns(run.total_elapsed_ns)
        );
        self.write_stdout(&footer);
    }
}

/// JSON reporter that writes v2 JSON, text, and Markdown reports.
pub struct JsonReporter {
    output_dir: PathBuf,
}

impl JsonReporter {
    /// Create a JSON reporter.
    #[must_use]
    pub fn new(output_dir: impl Into<PathBuf>) -> Self {
        Self {
            output_dir: output_dir.into(),
        }
    }

    fn write_results(&self, run: &StressRun) {
        if let Err(error) = self.write_results_inner(run) {
            eprintln!("Warning: failed to write results: {error}");
        }
    }

    fn write_results_inner(&self, run: &StressRun) -> std::io::Result<()> {
        let sanitized_name = run.suite.replace(['/', '\\'], "_");
        let suite_dir = self.output_dir.join(sanitized_name);
        std::fs::create_dir_all(&suite_dir)?;

        let timestamp = &run.started_at;
        let json_path = suite_dir.join(format!("{timestamp}.json"));
        let txt_path = suite_dir.join(format!("{timestamp}.txt"));
        let md_path = suite_dir.join(format!("{timestamp}.md"));
        let latest_json_path = suite_dir.join("latest.json");
        let latest_txt_path = suite_dir.join("latest.txt");
        let latest_md_path = suite_dir.join("latest.md");

        let json = serde_json::to_string_pretty(run).map_err(std::io::Error::other)?;
        let report = format_report(run);
        let markdown = format_markdown_report(run);

        std::fs::write(&json_path, &json)?;
        std::fs::write(&txt_path, &report)?;
        std::fs::write(&md_path, &markdown)?;
        std::fs::write(&latest_json_path, &json)?;
        std::fs::write(&latest_txt_path, &report)?;
        std::fs::write(&latest_md_path, &markdown)?;

        eprintln!("  Results written to: {}", json_path.display());
        eprintln!("  Latest results at: {}", latest_json_path.display());
        Ok(())
    }
}

impl Reporter for JsonReporter {
    fn suite_end(&self, run: &StressRun) {
        self.write_results(run);
    }
}

/// GitHub Actions reporter that emits annotations when running in Actions.
#[allow(dead_code)]
pub struct GitHubActionsReporter;

#[allow(dead_code)]
impl GitHubActionsReporter {
    /// Create a new GitHub Actions reporter.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    fn is_github_actions() -> bool {
        std::env::var("GITHUB_ACTIONS").is_ok()
    }
}

impl Default for GitHubActionsReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl Reporter for GitHubActionsReporter {
    fn suite_end(&self, run: &StressRun) {
        if !Self::is_github_actions() {
            return;
        }

        for comparison in &run.comparisons {
            if comparison.classification == ComparisonClass::Regression {
                println!(
                    "::warning title=Performance Regression in {}::Benchmark '{}' regressed by {:.1}%",
                    run.suite,
                    comparison.benchmark_id,
                    comparison.change_percent.unwrap_or_default().abs()
                );
            }
        }

        println!("::group::Stress Results - {}", run.suite);
        for summary in &run.summaries {
            println!(
                "  {}: {} ({})",
                summary.name,
                summary.primary_value().map_or_else(
                    || "n/a".to_string(),
                    |value| { format_metric(value, summary.primary_metric) }
                ),
                summary.quality
            );
        }
        println!("::endgroup::");
    }
}

/// Combines multiple reporters.
pub struct MultiReporter {
    reporters: Vec<Box<dyn Reporter>>,
}

impl MultiReporter {
    /// Create a multi-reporter.
    #[must_use]
    pub fn new(reporters: Vec<Box<dyn Reporter>>) -> Self {
        Self { reporters }
    }
}

impl Reporter for MultiReporter {
    fn suite_start(&self, suite: &str, config: &StressRunnerConfig) {
        for reporter in &self.reporters {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                reporter.suite_start(suite, config);
            }));
        }
    }

    fn bench_end(&self, summary: &BenchmarkSummary) {
        for reporter in &self.reporters {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                reporter.bench_end(summary);
            }));
        }
    }

    fn suite_end(&self, run: &StressRun) {
        for reporter in &self.reporters {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                reporter.suite_end(run);
            }));
        }
    }
}

pub(crate) fn format_report(run: &StressRun) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "Benchmark Suite: {}", run.suite);
    let _ = writeln!(output, "Schema: {}", run.schema_version);
    let _ = writeln!(output, "Profile: {}", run.run_profile);
    let _ = writeln!(output, "Completed: {}", run.started_at);
    let _ = writeln!(
        output,
        "Samples: measured={} warmup={} cooldown={}",
        run.environment.profile_config.measured_samples,
        run.environment.profile_config.warmup_samples,
        run.environment.profile_config.cooldown_samples
    );
    let _ = writeln!(
        output,
        "Total time: {}",
        format_duration_ns(run.total_elapsed_ns)
    );
    output.push('\n');

    output.push_str("Summary\n");
    output.push_str("-------\n");
    for summary in &run.summaries {
        write_summary_line(&mut output, summary);
    }

    write_comparison_section(&mut output, "Regressions", run, ComparisonClass::Regression);
    write_comparison_section(
        &mut output,
        "Improvements",
        run,
        ComparisonClass::Improvement,
    );
    write_quality_section(&mut output, run);
    write_sweep_tables(&mut output, run);

    output
}

pub(crate) fn format_markdown_report(run: &StressRun) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "# {}", run.suite);
    let _ = writeln!(output);
    let _ = writeln!(output, "- Schema: `{}`", run.schema_version);
    let _ = writeln!(output, "- Profile: `{}`", run.run_profile);
    let _ = writeln!(output, "- Completed: `{}`", run.started_at);
    let _ = writeln!(
        output,
        "- Total time: `{}`",
        format_duration_ns(run.total_elapsed_ns)
    );
    let _ = writeln!(output);
    let _ = writeln!(
        output,
        "| Benchmark | Tier | Metric | Value | Quality | Samples |"
    );
    let _ = writeln!(output, "|---|---:|---|---:|---|---:|");
    for summary in &run.summaries {
        let value = summary.primary_value().map_or_else(
            || "n/a".to_string(),
            |value| format_metric(value, summary.primary_metric),
        );
        let _ = writeln!(
            output,
            "| {} | {} | {:?} | {} | {} | {} |",
            summary.name,
            summary.tier,
            summary.primary_metric,
            value,
            summary.quality,
            summary.measured_samples
        );
    }
    output
}

fn default_config_lines(config: &StressRunnerConfig) -> Vec<String> {
    vec![
        format!("Profile: {}", config.profile),
        format!("Samples: {}", config.samples),
        format!("Warmup samples: {}", config.warmup_samples),
        format!("Cooldown samples: {}", config.cooldown_samples),
        format!("Output: {}", config.output_dir.display()),
        format!("Filter: {}", config.filter.as_deref().unwrap_or("<none>")),
        format!(
            "Tier: {}",
            config
                .tier
                .map_or_else(|| "<any>".to_string(), |tier| tier.to_string())
        ),
    ]
}

fn write_summary_line(output: &mut String, summary: &BenchmarkSummary) {
    let value = summary.primary_value().map_or_else(
        || "n/a".to_string(),
        |value| format_metric(value, summary.primary_metric),
    );
    let _ = writeln!(
        output,
        "  {name:<NAME_WIDTH$} {value:>VALUE_WIDTH$}  tier={tier} quality={quality} samples={samples}",
        name = summary.name,
        tier = summary.tier,
        quality = summary.quality,
        samples = summary.measured_samples
    );
}

fn write_comparison_section(
    output: &mut String,
    title: &str,
    run: &StressRun,
    class: ComparisonClass,
) {
    let rows = run
        .comparisons
        .iter()
        .filter(|comparison| comparison.classification == class)
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return;
    }

    let _ = writeln!(output);
    let _ = writeln!(output, "{title}");
    output.push_str(&"-".repeat(title.len()));
    output.push('\n');
    for comparison in rows {
        let _ = writeln!(
            output,
            "  {} {:+.1}% ({:?})",
            comparison.benchmark_id,
            comparison.change_percent.unwrap_or_default(),
            comparison.primary_metric
        );
    }
}

fn write_quality_section(output: &mut String, run: &StressRun) {
    let rows = run
        .summaries
        .iter()
        .filter(|summary| {
            matches!(
                summary.quality,
                QualityClass::Noisy | QualityClass::Untrustworthy
            )
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return;
    }

    output.push_str("\nNoisy Or Untrustworthy\n");
    output.push_str("----------------------\n");
    for summary in rows {
        let _ = writeln!(
            output,
            "  {} quality={} correctness={}",
            summary.name, summary.quality, summary.correctness.passed
        );
    }
}

fn write_sweep_tables(output: &mut String, run: &StressRun) {
    let numeric_keys = numeric_parameter_keys(&run.summaries);
    if numeric_keys.is_empty() {
        return;
    }

    output.push_str("\nSweep Tables\n");
    output.push_str("------------\n");
    for key in numeric_keys {
        let mut rows = run
            .summaries
            .iter()
            .filter_map(|summary| {
                let x = summary.parameters.get(&key)?.parse::<f64>().ok()?;
                let y = summary.primary_value()?;
                Some((x, y, summary))
            })
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| left.0.total_cmp(&right.0));
        if rows.len() < 2 {
            continue;
        }

        let baseline_x = rows[0].0;
        let baseline_y = rows[0].1;
        let mut plateau = None;
        let _ = writeln!(output, "Parameter: {key}");
        for (idx, (x, y, summary)) in rows.iter().enumerate() {
            let speedup = if summary.primary_metric.higher_is_better() {
                y / baseline_y
            } else {
                baseline_y / y
            };
            let efficiency = if baseline_x > 0.0 && *x > 0.0 {
                speedup / (*x / baseline_x)
            } else {
                0.0
            };
            if idx > 0 && plateau.is_none() {
                let previous_y = rows[idx - 1].1;
                let gain = if summary.primary_metric.higher_is_better() {
                    (y - previous_y) / previous_y
                } else {
                    (previous_y - y) / previous_y
                };
                if gain < 0.10 {
                    plateau = Some(*x);
                }
            }
            let _ = writeln!(
                output,
                "  {}={} value={} speedup={:.2} efficiency={:.2}",
                key,
                x,
                format_metric(*y, summary.primary_metric),
                speedup,
                efficiency
            );
        }
        if let Some(point) = plateau {
            let _ = writeln!(
                output,
                "  plateau: first {key} where incremental gain < 10% is {point}"
            );
        }
    }
}

fn numeric_parameter_keys(summaries: &[BenchmarkSummary]) -> Vec<String> {
    let mut values: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for summary in summaries {
        for (key, value) in &summary.parameters {
            if value.parse::<f64>().is_ok() {
                values.entry(key.clone()).or_default().insert(value.clone());
            }
        }
    }
    values
        .into_iter()
        .filter_map(|(key, seen)| (seen.len() > 1).then_some(key))
        .collect()
}

fn format_metric(value: f64, metric: PrimaryMetric) -> String {
    match metric {
        PrimaryMetric::Throughput => format_throughput(value),
        PrimaryMetric::LatencyP95 | PrimaryMetric::ElapsedPerOperation => {
            format_duration_ns(f64_to_u128(value))
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn f64_to_u128(value: f64) -> u128 {
    value.max(0.0).round() as u128
}

fn format_throughput(value: f64) -> String {
    if value >= 1_000_000.0 {
        format!("{:.2}M ops/s", value / 1_000_000.0)
    } else if value >= 1_000.0 {
        format!("{:.2}K ops/s", value / 1_000.0)
    } else {
        format!("{value:.2} ops/s")
    }
}

#[allow(clippy::cast_precision_loss)]
fn format_duration_ns(nanos: u128) -> String {
    let secs = nanos as f64 / 1_000_000_000.0;
    if secs >= 1.0 {
        format!("{secs:.2}s")
    } else if secs >= 0.001 {
        format!("{:.2}ms", secs * 1_000.0)
    } else if secs >= 0.000_001 {
        format!("{:.2}us", secs * 1_000_000.0)
    } else {
        format!("{:.2}ns", secs * 1_000_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{
        BenchmarkMode, BenchmarkSpec, ComparisonResult, CorrectnessCounters, CorrectnessSummary,
        EnvironmentInfo, ProfileConfig, Sample, SamplePhase, SummaryStats, SCHEMA_VERSION,
    };

    fn summary(name: &str, value: f64, quality: QualityClass) -> BenchmarkSummary {
        BenchmarkSummary {
            benchmark_id: name.to_string(),
            name: name.to_string(),
            tier: 2,
            primary_metric: PrimaryMetric::Throughput,
            measured_samples: 10,
            warmup_samples: 1,
            cooldown_samples: 0,
            stats: SummaryStats::from_values(&[value, value * 1.01])
                .or_else(|| SummaryStats::from_values(&[value])),
            quality,
            correctness: CorrectnessSummary {
                passed: true,
                counters: CorrectnessCounters {
                    attempted: 10,
                    completed: 10,
                    ..CorrectnessCounters::default()
                },
                errors: Vec::new(),
            },
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
        }
    }

    fn run_with_summaries(summaries: Vec<BenchmarkSummary>) -> StressRun {
        let profile_config = ProfileConfig::default();
        StressRun {
            schema_version: SCHEMA_VERSION.to_string(),
            tool_version: "0.3.0".to_string(),
            suite: "suite".to_string(),
            run_profile: profile_config.profile,
            environment: EnvironmentInfo::unknown(profile_config),
            benchmark_specs: vec![BenchmarkSpec {
                id: "bench".to_string(),
                name: "bench".to_string(),
                tier: 2,
                mode: BenchmarkMode::FixedOperations {
                    operations_per_sample: 1,
                },
                parameters: BTreeMap::new(),
                metadata: BTreeMap::new(),
            }],
            samples: vec![Sample {
                benchmark_id: "bench".to_string(),
                sample_number: 0,
                phase: SamplePhase::Measured,
                elapsed_ns: 1,
                operations_attempted: 1,
                operations_completed: 1,
                throughput: 1.0,
                latency_ns: Vec::new(),
                parameters: BTreeMap::new(),
                counters: CorrectnessCounters {
                    attempted: 1,
                    completed: 1,
                    ..CorrectnessCounters::default()
                },
                environment: EnvironmentInfo::unknown(ProfileConfig::default()),
            }],
            summaries,
            comparisons: Vec::new(),
            started_at: "123".to_string(),
            total_elapsed_ns: 1_000,
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn formats_summary_and_noisy_rows() {
        let run = run_with_summaries(vec![
            summary("fast", 1_000_000.0, QualityClass::Authoritative),
            summary("weak", 10.0, QualityClass::Noisy),
        ]);

        let report = format_report(&run);

        assert!(report.contains("Summary"));
        assert!(report.contains("fast"));
        assert!(report.contains("Noisy Or Untrustworthy"));
        assert!(report.contains("weak quality=noisy"));
    }

    #[test]
    fn formats_regressions_and_improvements() {
        let mut run = run_with_summaries(vec![summary("bench", 100.0, QualityClass::Acceptable)]);
        run.comparisons = vec![
            ComparisonResult {
                benchmark_id: "regressed".to_string(),
                current_quality: QualityClass::Acceptable,
                baseline_quality: Some(QualityClass::Acceptable),
                primary_metric: PrimaryMetric::Throughput,
                baseline_value: Some(100.0),
                current_value: Some(80.0),
                change_percent: Some(-20.0),
                threshold: 0.05,
                confidence_intervals_overlap: Some(false),
                classification: ComparisonClass::Regression,
            },
            ComparisonResult {
                benchmark_id: "improved".to_string(),
                current_quality: QualityClass::Acceptable,
                baseline_quality: Some(QualityClass::Acceptable),
                primary_metric: PrimaryMetric::Throughput,
                baseline_value: Some(100.0),
                current_value: Some(130.0),
                change_percent: Some(30.0),
                threshold: 0.05,
                confidence_intervals_overlap: Some(false),
                classification: ComparisonClass::Improvement,
            },
        ];

        let report = format_report(&run);

        assert!(report.contains("Regressions"));
        assert!(report.contains("regressed -20.0%"));
        assert!(report.contains("Improvements"));
        assert!(report.contains("improved +30.0%"));
    }

    #[test]
    fn formats_sweep_table_and_plateau() {
        let mut s1 = summary("client-1", 100.0, QualityClass::Acceptable);
        s1.parameters
            .insert("client_count".to_string(), "1".to_string());
        let mut s2 = summary("client-2", 180.0, QualityClass::Acceptable);
        s2.parameters
            .insert("client_count".to_string(), "2".to_string());
        let mut s4 = summary("client-4", 190.0, QualityClass::Acceptable);
        s4.parameters
            .insert("client_count".to_string(), "4".to_string());
        let run = run_with_summaries(vec![s1, s2, s4]);

        let report = format_report(&run);

        assert!(report.contains("Sweep Tables"));
        assert!(report.contains("Parameter: client_count"));
        assert!(report.contains("plateau: first client_count"));
    }
}
