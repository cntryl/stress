//! Pluggable reporters for current stress artifacts.

use crate::config::{ConsoleMode, StressRunnerConfig};
use crate::result::{
    BenchmarkSummary, ComparisonClass, ComparisonResult, CorrectnessSummary, PrimaryMetric,
    QualityClass, StressRun, SummaryStats,
};
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

const NAME_WIDTH: usize = 36;
const NUMBER_WIDTH: usize = 12;
const VALUE_WIDTH: usize = 16;

/// Console reporter that prints compact progress to stdout.
pub struct ConsoleReporter {
    mode: ConsoleMode,
    output_lock: Mutex<()>,
}

impl ConsoleReporter {
    /// Create a console reporter.
    #[must_use]
    pub fn new(mode: ConsoleMode) -> Self {
        Self {
            mode,
            output_lock: Mutex::new(()),
        }
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
        Self::new(ConsoleMode::Compact)
    }
}

impl Reporter for ConsoleReporter {
    fn suite_end(&self, run: &StressRun) {
        self.write_stdout(&format_console_run(run, self.mode));
    }
}

/// JSON reporter that writes JSON, text, and Markdown reports.
pub struct JsonReporter {
    output_dir: PathBuf,
    announce: bool,
}

impl JsonReporter {
    /// Create a JSON reporter.
    #[must_use]
    pub fn new(output_dir: impl Into<PathBuf>) -> Self {
        Self {
            output_dir: output_dir.into(),
            announce: true,
        }
    }

    /// Set whether artifact paths are printed to stderr.
    #[must_use]
    pub const fn announce(mut self, value: bool) -> Self {
        self.announce = value;
        self
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

        if self.announce {
            eprintln!("  Results written to: {}", json_path.display());
            eprintln!("  Latest results at: {}", latest_json_path.display());
        }
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
    let _ = writeln!(output, "## Summary");
    let _ = writeln!(output);
    let _ = writeln!(output, "```text");
    output.push_str(&format_summary_blocks(run));
    let _ = writeln!(output, "```");
    let _ = writeln!(output);
    let _ = writeln!(output, "## Needs attention");
    let _ = writeln!(output);
    let attention = attention_items(run);
    if attention.is_empty() {
        let _ = writeln!(output, "- none");
    } else {
        for item in attention {
            let _ = writeln!(output, "- {item}");
        }
    }
    let _ = writeln!(output);
    let _ = writeln!(output, "## Benchmarks");
    let _ = writeln!(output);
    let _ = writeln!(
        output,
        "| Benchmark | Tier | Metric | Value | Quality | Samples | Wall |"
    );
    let _ = writeln!(output, "|---|---:|---|---:|---|---:|---:|");
    for summary in &run.summaries {
        let value = summary.primary_value().map_or_else(
            || "n/a".to_string(),
            |value| format_metric(value, summary.primary_metric),
        );
        let _ = writeln!(
            output,
            "| {} | {} | {:?} | {} | {} | {} | {} |",
            summary.name,
            summary.tier,
            summary.primary_metric,
            value,
            summary.quality,
            summary.measured_samples,
            format_duration_ns(summary.total_wall_clock_ns)
        );
    }
    output
}

/// Format one stress run for stdout using the selected console mode.
#[must_use]
pub fn format_console_run(run: &StressRun, mode: ConsoleMode) -> String {
    match mode {
        ConsoleMode::Json => serde_json::to_string_pretty(run).unwrap_or_else(|error| {
            format!(r#"{{"error":"failed to serialize stress run: {error}"}}"#)
        }),
        ConsoleMode::Compact | ConsoleMode::Full | ConsoleMode::Verbose | ConsoleMode::Ci => {
            format_console_runs(std::slice::from_ref(run), mode)
        }
    }
}

/// Format multiple stress runs as one consolidated stdout report.
#[must_use]
pub fn format_console_runs(runs: &[StressRun], mode: ConsoleMode) -> String {
    match mode {
        ConsoleMode::Json => serde_json::to_string_pretty(runs).unwrap_or_else(|error| {
            format!(r#"[{{"error":"failed to serialize stress runs: {error}"}}]"#)
        }),
        ConsoleMode::Compact | ConsoleMode::Full | ConsoleMode::Verbose | ConsoleMode::Ci => {
            format_human_console_runs(runs, mode)
        }
    }
}

#[cfg(test)]
fn format_console_output(
    run: &StressRun,
    _summaries: &[BenchmarkSummary],
    mode: ConsoleMode,
) -> String {
    format_console_run(run, mode)
}

fn format_human_console_runs(runs: &[StressRun], mode: ConsoleMode) -> String {
    let mut output = String::new();
    write_run_header(&mut output, runs);
    let mut wrote_suite = false;
    for run in runs {
        if should_print_suite(run, mode) {
            if wrote_suite || !output.is_empty() {
                let _ = writeln!(output);
            }
            write_suite_block(&mut output, run, mode);
            wrote_suite = true;
        }
    }
    if wrote_suite || !output.is_empty() {
        let _ = writeln!(output);
    }
    write_run_summary(&mut output, runs);
    output
}

fn write_run_header(output: &mut String, runs: &[StressRun]) {
    let Some(first) = runs.first() else {
        return;
    };
    let profile = &first.environment.profile_config;
    let bench_count = runs.iter().map(|run| run.summaries.len()).sum::<usize>();
    let _ = writeln!(output, "@cntryl/stress v{}", first.tool_version);
    let _ = writeln!(
        output,
        "profile: {} | suites={} | benches={} | measured={} warmup={} cooldown={}",
        first.run_profile,
        runs.len(),
        bench_count,
        profile.measured_samples,
        profile.warmup_samples,
        profile.cooldown_samples
    );
    let _ = writeln!(
        output,
        "measure: {} fixed-duration default, {} op fixed-operations default",
        format_duration_ns(profile.sample_duration.as_nanos()),
        profile.operations_per_sample
    );
    let _ = writeln!(
        output,
        "commit: {} | baseline: {} | threshold: {:.1}%",
        short_commit(first),
        aggregate_baseline_status(runs),
        profile.regression_threshold * 100.0
    );
    let _ = writeln!(output, "machine: {}", machine_summary(first));
}

fn aggregate_baseline_status(runs: &[StressRun]) -> &'static str {
    if runs.iter().any(|run| baseline_status(run) == "found") {
        "found"
    } else {
        "none"
    }
}

fn should_print_suite(run: &StressRun, mode: ConsoleMode) -> bool {
    mode != ConsoleMode::Ci || suite_status(run) != SuiteStatus::Pass
}

fn write_suite_block(output: &mut String, run: &StressRun, mode: ConsoleMode) {
    let status = suite_status(run);
    let _ = writeln!(
        output,
        "{}  {}  {} benches",
        run.suite,
        status.as_str(),
        run.summaries.len()
    );

    let rows = rows_for_mode(run, mode);
    if rows.is_empty() {
        return;
    }

    match mode {
        ConsoleMode::Verbose => {
            write_verbose_table_header(output);
            let comparisons = comparison_by_benchmark(run);
            for summary in &rows {
                let comparison = comparisons.get(summary.benchmark_id.as_str()).copied();
                write_verbose_table_row(output, summary, comparison);
            }
            write_verbose_footer(output, &run.summaries);
        }
        ConsoleMode::Compact | ConsoleMode::Full | ConsoleMode::Ci => {
            write_narrow_table_header(output);
            for summary in &rows {
                write_narrow_table_row(output, summary);
            }
            if matches!(mode, ConsoleMode::Compact | ConsoleMode::Ci) {
                let hidden = run.summaries.len().saturating_sub(rows.len());
                if hidden != 0 {
                    let _ = writeln!(output, "  ... {hidden} ok hidden; use --console full");
                }
            }
        }
        ConsoleMode::Json => {}
    }

    write_attention_details(output, run, mode);
}

fn rows_for_mode(run: &StressRun, mode: ConsoleMode) -> Vec<&BenchmarkSummary> {
    let comparisons = comparison_by_benchmark(run);
    let mut rows = run
        .summaries
        .iter()
        .filter(|summary| {
            matches!(mode, ConsoleMode::Full | ConsoleMode::Verbose)
                || summary_attention_rank(run, summary, &comparisons).is_some()
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        summary_attention_rank(run, left, &comparisons)
            .unwrap_or(u8::MAX)
            .cmp(&summary_attention_rank(run, right, &comparisons).unwrap_or(u8::MAX))
            .then_with(|| left.name.cmp(&right.name))
    });
    rows
}

fn write_narrow_table_header(output: &mut String) {
    let _ = writeln!(
        output,
        "  {benchmark:<NAME_WIDTH$} {value:>VALUE_WIDTH$} {p95:>VALUE_WIDTH$} {rsd:>8} {allocs:>NUMBER_WIDTH$} {bytes:>NUMBER_WIDTH$} {quality:>13}",
        benchmark = "benchmark",
        value = "value",
        p95 = "p95",
        rsd = "rsd",
        allocs = "alloc/op",
        bytes = "B/op",
        quality = "quality",
    );
}

fn write_narrow_table_row(output: &mut String, summary: &BenchmarkSummary) {
    let name = compact_name(&summary.name);
    let quality = display_quality(summary);
    let value = summary.primary_value().map_or_else(
        || "n/a".to_string(),
        |value| format_metric_value(value, summary.primary_metric),
    );
    let p95 = summary.stats.as_ref().map_or_else(
        || "n/a".to_string(),
        |stats| format_metric_value(stats.p95, summary.primary_metric),
    );
    let rsd = summary.stats.as_ref().map_or_else(
        || "n/a".to_string(),
        |stats| format_percent(stats.relative_std_dev),
    );
    let _ = writeln!(
        output,
        "  {name:<NAME_WIDTH$} {value:>VALUE_WIDTH$} {p95:>VALUE_WIDTH$} {rsd:>8} {allocs:>NUMBER_WIDTH$} {bytes:>NUMBER_WIDTH$} {quality:>13}",
        allocs = format_optional_compact_stat(summary.allocs_per_op.as_ref()),
        bytes = format_optional_compact_stat(summary.bytes_per_op.as_ref()),
    );
}

fn write_verbose_table_header(output: &mut String) {
    let _ = writeln!(
        output,
        "  {name:<NAME_WIDTH$} {metric:>8} {value:>NUMBER_WIDTH$} {mean:>NUMBER_WIDTH$} {p50:>NUMBER_WIDTH$} {p95:>NUMBER_WIDTH$} {p99:>NUMBER_WIDTH$} {allocs:>NUMBER_WIDTH$} {bytes:>NUMBER_WIDTH$} {overhead:>NUMBER_WIDTH$} {rsd:>8} {quality:>13} {samples:>7} {wall:>NUMBER_WIDTH$} {delta:>18}  notes",
        name = "name",
        metric = "metric",
        value = "value",
        mean = "mean",
        p50 = "p50",
        p95 = "p95",
        p99 = "p99",
        allocs = "alloc/op",
        bytes = "B/op",
        overhead = "overhead",
        rsd = "rsd",
        quality = "quality",
        samples = "samples",
        wall = "wall",
        delta = "delta"
    );
}

fn write_verbose_table_row(
    output: &mut String,
    summary: &BenchmarkSummary,
    comparison: Option<&ComparisonResult>,
) {
    let name = compact_name(&summary.name);
    let metric = metric_label(summary.primary_metric);
    let quality = display_quality(summary);
    let marker = if summary.correctness.passed { " " } else { "!" };
    let delta = comparison.map_or_else(|| "-".to_string(), format_delta_cell);
    let notes = row_notes(summary);

    if let Some(stats) = &summary.stats {
        let value = summary.primary_value().map_or_else(
            || "n/a".to_string(),
            |value| format_metric_value(value, summary.primary_metric),
        );
        let _ = writeln!(
            output,
            "{marker} {name:<NAME_WIDTH$} {metric:>8} {value:>NUMBER_WIDTH$} {mean:>NUMBER_WIDTH$} {p50:>NUMBER_WIDTH$} {p95:>NUMBER_WIDTH$} {p99:>NUMBER_WIDTH$} {allocs:>NUMBER_WIDTH$} {bytes:>NUMBER_WIDTH$} {overhead:>NUMBER_WIDTH$} {rsd:>8} {quality:>13} {samples:>7} {wall:>NUMBER_WIDTH$} {delta:>18}  {notes}",
            mean = format_metric_value(stats.mean, summary.primary_metric),
            p50 = format_metric_value(stats.p50, summary.primary_metric),
            p95 = format_metric_value(stats.p95, summary.primary_metric),
            p99 = format_metric_value(stats.p99, summary.primary_metric),
            allocs = format_optional_compact_stat(summary.allocs_per_op.as_ref()),
            bytes = format_optional_compact_stat(summary.bytes_per_op.as_ref()),
            overhead = format_optional_duration_stat(summary.overhead_ns_per_op.as_ref()),
            rsd = format_percent(stats.relative_std_dev),
            samples = summary.measured_samples,
            wall = format_duration_ns(summary.total_wall_clock_ns),
        );
    } else {
        let unavailable = "n/a";
        let _ = writeln!(
            output,
            "{marker} {name:<NAME_WIDTH$} {metric:>8} {unavailable:>NUMBER_WIDTH$} {unavailable:>NUMBER_WIDTH$} {unavailable:>NUMBER_WIDTH$} {unavailable:>NUMBER_WIDTH$} {unavailable:>NUMBER_WIDTH$} {unavailable:>NUMBER_WIDTH$} {unavailable:>NUMBER_WIDTH$} {unavailable:>NUMBER_WIDTH$} {unavailable:>8} {quality:>13} {samples:>7} {wall:>NUMBER_WIDTH$} {delta:>18}  {notes}",
            samples = summary.measured_samples,
            wall = format_duration_ns(summary.total_wall_clock_ns),
        );
    }
}

fn display_quality(summary: &BenchmarkSummary) -> String {
    if summary.correctness.passed {
        summary.quality.to_string()
    } else {
        "correctness_failed".to_string()
    }
}

fn write_verbose_footer(output: &mut String, summaries: &[BenchmarkSummary]) {
    if summaries
        .iter()
        .any(|summary| summary.primary_metric == PrimaryMetric::Throughput)
    {
        let _ = writeln!(
            output,
            "  note: throughput p50/p95/p99 are sample-throughput percentiles, not operation latency percentiles."
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SuiteStatus {
    Pass,
    Warn,
    Fail,
}

impl SuiteStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
        }
    }
}

fn suite_status(run: &StressRun) -> SuiteStatus {
    if failed_correctness_count(&run.summaries) != 0
        || budget_failure_count(&run.summaries) != 0
        || !quality_gate_failures(run).is_empty()
        || regression_gate_count(run) != 0
    {
        return SuiteStatus::Fail;
    }

    let comparisons = comparison_by_benchmark(run);
    if run
        .summaries
        .iter()
        .any(|summary| summary_attention_rank(run, summary, &comparisons).is_some())
    {
        SuiteStatus::Warn
    } else {
        SuiteStatus::Pass
    }
}

fn summary_attention_rank(
    run: &StressRun,
    summary: &BenchmarkSummary,
    comparisons: &BTreeMap<&str, &ComparisonResult>,
) -> Option<u8> {
    if !summary.correctness.passed {
        return Some(0);
    }
    if summary.budget_results.iter().any(|result| !result.passed) {
        return Some(1);
    }
    if run.environment.profile_config.fail_on_quality
        && quality_rank(summary.quality) < quality_rank(run.environment.profile_config.min_quality)
    {
        return Some(2);
    }
    if comparisons
        .get(summary.benchmark_id.as_str())
        .is_some_and(|comparison| comparison.classification == ComparisonClass::Regression)
    {
        return Some(3);
    }
    if summary.quality == QualityClass::Untrustworthy {
        return Some(4);
    }
    if summary.quality == QualityClass::Noisy {
        return Some(5);
    }
    if !summary.flags.is_empty() {
        return Some(6);
    }
    if comparisons
        .get(summary.benchmark_id.as_str())
        .is_some_and(|comparison| {
            matches!(
                comparison.classification,
                ComparisonClass::Improvement | ComparisonClass::Inconclusive
            ) && comparison.change_percent.is_some()
        })
    {
        return Some(7);
    }
    None
}

fn write_attention_details(output: &mut String, run: &StressRun, mode: ConsoleMode) {
    if mode == ConsoleMode::Full && suite_status(run) == SuiteStatus::Pass {
        return;
    }
    let details = attention_details(run);
    if details.is_empty() {
        return;
    }

    let _ = writeln!(output, "  attention:");
    for detail in details {
        let _ = writeln!(output, "    {detail}");
    }
}

fn attention_details(run: &StressRun) -> Vec<String> {
    let mut details = Vec::new();
    push_count_detail(
        &mut details,
        "correctness",
        failed_correctness_count(&run.summaries),
        "failed; inspect counters and validation errors",
    );
    push_count_detail(
        &mut details,
        "budget",
        budget_failure_count(&run.summaries),
        "failed; reduce measured cost or adjust the explicit budget",
    );
    push_count_detail(
        &mut details,
        "quality",
        quality_gate_failures(run).len(),
        &format!(
            "below {}; collect more measured runs or stabilize setup",
            run.environment.profile_config.min_quality
        ),
    );
    let regressions = run
        .comparisons
        .iter()
        .filter(|comparison| comparison.classification == ComparisonClass::Regression)
        .count();
    push_count_detail(
        &mut details,
        "regression",
        regressions,
        "against baseline; inspect same benchmark row before updating baselines",
    );
    let noisy = run
        .summaries
        .iter()
        .filter(|summary| summary.quality == QualityClass::Noisy)
        .count();
    let untrustworthy = run
        .summaries
        .iter()
        .filter(|summary| summary.quality == QualityClass::Untrustworthy)
        .count();
    if noisy != 0 || untrustworthy != 0 {
        details.push(format!(
            "noise: {noisy} noisy, {untrustworthy} untrustworthy; fix: use deterministic fixtures and move setup outside measurement"
        ));
    }
    let flag_summary = flag_attention_summary(&run.summaries);
    if !flag_summary.is_empty() {
        details.push(format!("flags: {}", flag_summary.join("; ")));
    }
    let improvements = comparison_count(run, ComparisonClass::Improvement);
    push_count_detail(
        &mut details,
        "improvement",
        improvements,
        "trustworthy improvement; update baselines only when intentional",
    );
    details
}

fn push_count_detail(details: &mut Vec<String>, label: &str, count: usize, text: &str) {
    if count != 0 {
        details.push(format!("{label}: {count} {text}"));
    }
}

fn flag_attention_summary(summaries: &[BenchmarkSummary]) -> Vec<String> {
    let mut counts = BTreeMap::<&str, usize>::new();
    for flag in summaries.iter().flat_map(|summary| &summary.flags) {
        *counts.entry(flag.as_str()).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(flag, count)| format!("{flag}={count}; {}", concise_flag_advice(flag)))
        .collect()
}

fn concise_flag_advice(flag: &str) -> &'static str {
    match flag {
        "tier_throughput_single_op" => "use measure_batch, measure_counted, or record_external",
        "suspicious_micro" => "validate the micro path before marking it trusted",
        "allocation_tracking_required" => "install cntryl_stress::stress_allocator!()",
        "zero_completed_ops" => "record completed logical work",
        "overhead_dominant" => "increase measured work per timing operation",
        "invalid_timing" => "measure one non-empty workload",
        "budget_failed" => "inspect configured budgets",
        _ => "inspect benchmark setup",
    }
}

fn write_run_summary(output: &mut String, runs: &[StressRun]) {
    let aggregate = AggregateSummary::from_runs(runs);
    let _ = writeln!(output, "Run summary");
    let _ = writeln!(output, "  suites:          {}", aggregate.suites);
    let _ = writeln!(output, "  benchmarks:      {}", aggregate.benchmarks);
    let _ = writeln!(output, "  gate:            {}", aggregate.gate_status());
    let _ = writeln!(output, "  failed_suites:   {}", aggregate.failed_suites);
    let _ = writeln!(output, "  warning_suites:  {}", aggregate.warning_suites);
    let _ = writeln!(
        output,
        "  correctness_bad: {}",
        aggregate.correctness_failed
    );
    let _ = writeln!(output, "  budget_failed:   {}", aggregate.budget_failed);
    let _ = writeln!(output, "  quality_failed:  {}", aggregate.quality_failed);
    let _ = writeln!(output, "  noisy:           {}", aggregate.noisy);
    let _ = writeln!(output, "  untrustworthy:   {}", aggregate.untrustworthy);
    let _ = writeln!(output, "  regressions:     {}", aggregate.regressions);
    let _ = writeln!(output, "  improvements:    {}", aggregate.improvements);
    let _ = writeln!(
        output,
        "  elapsed:         {}",
        format_duration_ns(aggregate.elapsed_ns)
    );
}

#[derive(Default)]
struct AggregateSummary {
    suites: usize,
    benchmarks: usize,
    failed_suites: usize,
    warning_suites: usize,
    correctness_failed: usize,
    budget_failed: usize,
    quality_failed: usize,
    noisy: usize,
    untrustworthy: usize,
    regressions: usize,
    improvements: usize,
    elapsed_ns: u128,
}

impl AggregateSummary {
    fn from_runs(runs: &[StressRun]) -> Self {
        runs.iter().fold(Self::default(), |mut aggregate, run| {
            aggregate.suites += 1;
            aggregate.benchmarks += run.summaries.len();
            match suite_status(run) {
                SuiteStatus::Fail => aggregate.failed_suites += 1,
                SuiteStatus::Warn => aggregate.warning_suites += 1,
                SuiteStatus::Pass => {}
            }
            aggregate.correctness_failed += failed_correctness_count(&run.summaries);
            aggregate.budget_failed += budget_failure_count(&run.summaries);
            aggregate.quality_failed += quality_gate_failures(run).len();
            aggregate.noisy += run
                .summaries
                .iter()
                .filter(|summary| summary.quality == QualityClass::Noisy)
                .count();
            aggregate.untrustworthy += run
                .summaries
                .iter()
                .filter(|summary| summary.quality == QualityClass::Untrustworthy)
                .count();
            aggregate.regressions += run
                .comparisons
                .iter()
                .filter(|comparison| comparison.classification == ComparisonClass::Regression)
                .count();
            aggregate.improvements += comparison_count(run, ComparisonClass::Improvement);
            aggregate.elapsed_ns += run.total_elapsed_ns;
            aggregate
        })
    }

    fn gate_status(&self) -> &'static str {
        if self.failed_suites == 0 {
            "passed"
        } else {
            "failed"
        }
    }
}

fn metric_label(metric: PrimaryMetric) -> &'static str {
    match metric {
        PrimaryMetric::Throughput => "ops/s",
        PrimaryMetric::LatencyP95 => "latency",
        PrimaryMetric::NsPerOp => "ns/op",
    }
}

fn format_summary_blocks(run: &StressRun) -> String {
    let mut output = String::new();
    let correctness_failed = failed_correctness_count(&run.summaries);
    let budget_failures = budget_failure_count(&run.summaries);
    let quality_failures = quality_gate_failures(run).len();
    let regressions = regression_gate_count(run);
    let improvements = comparison_count(run, ComparisonClass::Improvement);
    let _ = writeln!(output, "Summary");
    let _ = writeln!(output, "  benchmarks:      {}", run.summaries.len());
    let _ = writeln!(output, "  gate:            {}", gate_status(run));
    let _ = writeln!(
        output,
        "  correctness_ok:  {}",
        run.summaries.len().saturating_sub(correctness_failed)
    );
    let _ = writeln!(output, "  correctness_bad: {correctness_failed}");
    let _ = writeln!(output, "  budget_failed:   {budget_failures}");
    let _ = writeln!(output, "  quality_failed:  {quality_failures}");
    let _ = writeln!(output, "  regressions:     {regressions}");
    let _ = writeln!(output, "  improvements:    {improvements}");
    let counts = quality_counts(&run.summaries);
    let _ = writeln!(output, "Quality");
    let _ = writeln!(output, "  authoritative:   {}", counts.authoritative);
    let _ = writeln!(output, "  acceptable:      {}", counts.acceptable);
    let _ = writeln!(output, "  noisy:           {}", counts.noisy);
    let _ = writeln!(output, "  untrustworthy:   {}", counts.untrustworthy);
    output
}

fn attention_items(run: &StressRun) -> Vec<String> {
    let comparisons = comparison_by_benchmark(run);
    let mut items = Vec::new();
    let mut seen = BTreeSet::new();
    push_correctness_attention(&mut items, &mut seen, &run.summaries);
    push_budget_attention(&mut items, &mut seen, &run.summaries);
    push_quality_gate_attention(&mut items, &mut seen, run);
    push_noisy_attention(&mut items, &mut seen, &run.summaries);
    push_comparison_attention(
        &mut items,
        &mut seen,
        &run.summaries,
        &comparisons,
        ComparisonClass::Regression,
    );
    push_flag_attention(
        &mut items,
        &mut seen,
        &run.summaries,
        "tier_throughput_single_op",
    );
    push_flag_attention(&mut items, &mut seen, &run.summaries, "suspicious_micro");
    push_untrustworthy_attention(&mut items, &mut seen, &run.summaries);
    push_comparison_attention(
        &mut items,
        &mut seen,
        &run.summaries,
        &comparisons,
        ComparisonClass::Improvement,
    );
    push_stable_change_attention(&mut items, &mut seen, &run.summaries, &comparisons);
    items
}

fn push_budget_attention(
    items: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    summaries: &[BenchmarkSummary],
) {
    for summary in summaries
        .iter()
        .filter(|summary| summary.budget_results.iter().any(|result| !result.passed))
    {
        if seen.insert(summary.benchmark_id.clone()) {
            items.push(format!(
                "! {} budget failed: {}",
                summary.name,
                budget_note(summary)
            ));
        }
    }
}

fn push_flag_attention(
    items: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    summaries: &[BenchmarkSummary],
    flag: &str,
) {
    for summary in summaries
        .iter()
        .filter(|summary| summary.flags.iter().any(|item| item == flag))
    {
        if seen.insert(summary.benchmark_id.clone()) {
            items.push(format!(
                "! {} {}",
                summary.name,
                flag_note(summary, flag).unwrap_or_else(|| flag.to_string())
            ));
        }
    }
}

fn push_correctness_attention(
    items: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    summaries: &[BenchmarkSummary],
) {
    for summary in summaries
        .iter()
        .filter(|summary| !summary.correctness.passed)
    {
        seen.insert(summary.benchmark_id.clone());
        items.push(format!(
            "✗ {} correctness failed: {}",
            summary.name,
            correctness_note(&summary.correctness)
        ));
    }
}

fn push_comparison_attention(
    items: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    summaries: &[BenchmarkSummary],
    comparisons: &BTreeMap<&str, &ComparisonResult>,
    class: ComparisonClass,
) {
    let mut rows = summaries
        .iter()
        .filter_map(|summary| comparisons.get(summary.benchmark_id.as_str()).copied())
        .filter(|comparison| {
            class == ComparisonClass::Regression || comparison_is_trustworthy(comparison)
        })
        .filter(|comparison| comparison.classification == class)
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .change_percent
            .unwrap_or_default()
            .abs()
            .total_cmp(&left.change_percent.unwrap_or_default().abs())
    });
    for comparison in rows {
        if seen.insert(comparison.benchmark_id.clone()) {
            let icon = if class == ComparisonClass::Regression {
                "↓"
            } else {
                "↑"
            };
            items.push(format!(
                "{icon} {} {}",
                comparison.benchmark_id,
                format_delta_cell(comparison)
            ));
        }
    }
}

fn push_quality_gate_attention(
    items: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    run: &StressRun,
) {
    for summary in quality_gate_failures(run) {
        if seen.insert(summary.benchmark_id.clone()) {
            items.push(format!(
                "! {} quality gate failed: quality={} below min={} {}",
                summary.name,
                summary.quality,
                run.environment.profile_config.min_quality,
                row_notes(summary)
            ));
        }
    }
}

fn push_noisy_attention(
    items: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    summaries: &[BenchmarkSummary],
) {
    let mut rows = summaries
        .iter()
        .filter(|summary| summary.quality == QualityClass::Noisy)
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .stats
            .as_ref()
            .map_or(0.0, |stats| stats.relative_std_dev)
            .total_cmp(
                &left
                    .stats
                    .as_ref()
                    .map_or(0.0, |stats| stats.relative_std_dev),
            )
    });
    for summary in rows {
        if seen.insert(summary.benchmark_id.clone()) {
            items.push(format!(
                "! {} {}",
                summary.name,
                quality_note("noisy", summary)
            ));
        }
    }
}

fn push_untrustworthy_attention(
    items: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    summaries: &[BenchmarkSummary],
) {
    let mut rows = summaries
        .iter()
        .filter(|summary| summary.quality == QualityClass::Untrustworthy)
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .stats
            .as_ref()
            .map_or(0.0, |stats| stats.relative_std_dev)
            .total_cmp(
                &left
                    .stats
                    .as_ref()
                    .map_or(0.0, |stats| stats.relative_std_dev),
            )
    });
    for summary in rows {
        if seen.insert(summary.benchmark_id.clone()) {
            items.push(format!("! {} {}", summary.name, row_notes(summary)));
        }
    }
}

fn push_stable_change_attention(
    items: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    summaries: &[BenchmarkSummary],
    comparisons: &BTreeMap<&str, &ComparisonResult>,
) {
    let mut rows = summaries
        .iter()
        .filter(|summary| is_trustworthy(summary))
        .filter_map(|summary| comparisons.get(summary.benchmark_id.as_str()).copied())
        .filter(|comparison| comparison.classification == ComparisonClass::Inconclusive)
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .change_percent
            .unwrap_or_default()
            .abs()
            .total_cmp(&left.change_percent.unwrap_or_default().abs())
    });
    for comparison in rows.into_iter().take(3) {
        if seen.insert(comparison.benchmark_id.clone()) {
            items.push(format!(
                "~ {} {}",
                comparison.benchmark_id,
                format_delta_cell(comparison)
            ));
        }
    }
}

fn comparison_by_benchmark(run: &StressRun) -> BTreeMap<&str, &ComparisonResult> {
    run.comparisons
        .iter()
        .map(|comparison| (comparison.benchmark_id.as_str(), comparison))
        .collect()
}

fn format_delta_cell(comparison: &ComparisonResult) -> String {
    let Some(change) = comparison.change_percent else {
        return "-".to_string();
    };
    if !comparison_is_trustworthy(comparison) {
        return format!("{change:+.1}% noisy");
    }
    let label = match comparison.classification {
        ComparisonClass::Regression => "regression",
        ComparisonClass::Improvement => "improved",
        ComparisonClass::Inconclusive | ComparisonClass::MissingBaseline => "unchanged",
    };
    format!("{change:+.1}% {label}")
}

fn comparison_is_trustworthy(comparison: &ComparisonResult) -> bool {
    is_comparison_quality_trustworthy(comparison.current_quality)
        && comparison
            .baseline_quality
            .is_some_and(is_comparison_quality_trustworthy)
}

const fn is_comparison_quality_trustworthy(quality: QualityClass) -> bool {
    matches!(
        quality,
        QualityClass::Authoritative | QualityClass::Acceptable
    )
}

fn row_notes(summary: &BenchmarkSummary) -> String {
    if !summary.correctness.passed {
        return correctness_note(&summary.correctness);
    }
    if summary.budget_results.iter().any(|result| !result.passed) {
        return budget_note(summary);
    }
    if !summary.flags.is_empty() {
        return summary
            .flags
            .iter()
            .map(|flag| flag_note(summary, flag).unwrap_or_else(|| flag.clone()))
            .collect::<Vec<_>>()
            .join("; ");
    }
    match summary.quality {
        QualityClass::Authoritative | QualityClass::Acceptable => String::new(),
        QualityClass::Noisy => quality_note("noisy", summary),
        QualityClass::Untrustworthy => quality_note("untrustworthy", summary),
    }
}

fn budget_note(summary: &BenchmarkSummary) -> String {
    summary
        .budget_results
        .iter()
        .filter(|result| !result.passed)
        .map(|result| {
            result.reason.as_ref().map_or_else(
                || format!("{} failed", result.metric),
                |reason| {
                    let mut note = format!("{} {reason}", result.metric);
                    if allocation_budget_unavailable(result) {
                        note.push_str(
                            "; allocation budgets require cntryl_stress::stress_allocator!()",
                        );
                    }
                    note
                },
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn allocation_budget_unavailable(result: &crate::result::BudgetResult) -> bool {
    matches!(
        result.metric.as_str(),
        "max_allocs_per_op" | "max_bytes_per_op"
    ) && result.actual.is_none()
}

fn quality_note(label: &str, summary: &BenchmarkSummary) -> String {
    let mut parts = vec![label.to_string()];
    if summary.measured_samples < 2 {
        parts.push(format!("samples={}", summary.measured_samples));
    }
    if let Some(stats) = &summary.stats {
        parts.push(format!("rsd={}", format_percent(stats.relative_std_dev)));
    }
    if let Some(advice) = quality_advice(summary) {
        parts.push(format!("fix: {advice}"));
    }
    parts.join(", ")
}

fn quality_advice(summary: &BenchmarkSummary) -> Option<String> {
    if summary.measured_samples < 5 {
        return Some(format!(
            "collect more measured samples; avoid one-sample release gates; {}",
            tier_recipe(summary)
        ));
    }
    if summary
        .stats
        .as_ref()
        .is_some_and(|stats| stats.relative_std_dev > 0.10)
    {
        return Some(format!(
            "use deterministic fixtures and move setup outside measurement; {}",
            tier_recipe(summary)
        ));
    }
    (summary.quality == QualityClass::Noisy).then(|| {
        format!(
            "use deterministic fixtures and move setup outside measurement; {}",
            tier_recipe(summary)
        )
    })
}

fn flag_note(summary: &BenchmarkSummary, flag: &str) -> Option<String> {
    match flag {
        "invalid_timing" => Some(
            format!(
                "invalid_timing: recorded zero or invalid timing; measure exactly one non-empty workload; {}",
                tier_recipe(summary)
            ),
        ),
        "zero_completed_ops" => Some(
            format!(
                "zero_completed_ops: completed operations were zero; record logical work with {}; {}",
                operation_count_recipe(summary),
                tier_recipe(summary)
            ),
        ),
        "overhead_dominant" => Some(
            format!(
                "overhead_dominant: timing overhead dominates the measured work; {}; {}",
                overhead_recipe(summary),
                tier_recipe(summary)
            ),
        ),
        "allocation_tracking_required" => Some(
            "allocation_tracking_required: allocation budgets require cntryl_stress::stress_allocator!() in the benchmark crate"
                .to_string(),
        ),
        "budget_failed" => Some("budget_failed: configured budget failed".to_string()),
        "suspicious_micro" => Some(format!(
            "suspicious_micro: {} is below 5ns/op without validation; Tier 1 should use ctx.measure_micro(...) for the hot path and set metadata(validated_micro = \"true\") only after independent validation",
            summary.name
        )),
        "tier_throughput_single_op" => Some(
            "tier_throughput_single_op: only one completed op per sample; if this was batch work, use ctx.measure_batch(...) or ctx.record_external(...); if it is one subsystem operation, use Tier 2"
                .to_string(),
        ),
        _ => None,
    }
}

fn tier_recipe(summary: &BenchmarkSummary) -> &'static str {
    match summary.tier {
        1 => "Tier 1 recipe: ctx.measure_micro(...) on the hot path only",
        2 => {
            "Tier 2 recipe: ctx.measure(...) for one subsystem operation, or ctx.measure_counted(...) when that call completes a batch"
        }
        3 => "Tier 3 recipe: #[stress_test(tier = 3)] with ctx.measure_batch(n, ...) for system throughput",
        4 => "Tier 4 recipe: #[stress_test(tier = 4)] with ctx.measure_batch(n, ...) or ctx.record_external(duration, n) for integration throughput",
        5 => "Tier 5 recipe: #[stress_test(tier = 5)] with scale parameters and ctx.measure_batch(n, ...)",
        6 => "Tier 6 recipe: #[stress_test(tier = 6)] with ctx.measure_batch(n, ...) or ctx.record_external(duration, n) over the soak window",
        _ => "undefined tier: cntryl-stress defines tiers 1 through 6; choose the closest defined tier before authoring the benchmark",
    }
}

fn operation_count_recipe(summary: &BenchmarkSummary) -> &'static str {
    match summary.tier {
        1 => "ctx.measure_micro(...) for calibrated hot-path iterations",
        2 => "ctx.measure_counted(|| completed_work()) or ctx.operations(n) after ctx.measure(...) returns completed batch work",
        _ => "ctx.measure_batch(n, ...) or ctx.record_external(duration, n)",
    }
}

fn overhead_recipe(summary: &BenchmarkSummary) -> &'static str {
    match summary.tier {
        1 => "batch more hot-path work or mark validated_micro only after independent validation",
        2 => "use ctx.measure_counted(...) for batch-returning subsystem calls or move tiny hot paths to Tier 1",
        _ => "increase logical work per iteration with ctx.measure_batch(n, ...)",
    }
}

fn correctness_note(correctness: &CorrectnessSummary) -> String {
    let counters = correctness.counters;
    let mut parts = vec![
        format!("attempted={}", counters.attempted),
        format!("completed={}", counters.completed),
    ];
    if counters.attempted > counters.completed {
        parts.push(format!("lost={}", counters.attempted - counters.completed));
    }
    for (label, value) in [
        ("failed", counters.failures),
        ("timed_out", counters.timeouts),
        ("duplicates", counters.duplicates),
        ("dropped", counters.dropped),
        ("validation_errors", counters.validation_errors),
    ] {
        if value != 0 {
            parts.push(format!("{label}={value}"));
        }
    }
    parts.join(" ")
}

fn quality_counts(summaries: &[BenchmarkSummary]) -> QualityCounts {
    summaries
        .iter()
        .fold(QualityCounts::default(), |mut acc, summary| {
            match summary.quality {
                QualityClass::Authoritative => acc.authoritative += 1,
                QualityClass::Acceptable => acc.acceptable += 1,
                QualityClass::Noisy => acc.noisy += 1,
                QualityClass::Untrustworthy => acc.untrustworthy += 1,
            }
            acc
        })
}

#[derive(Default)]
struct QualityCounts {
    authoritative: usize,
    acceptable: usize,
    noisy: usize,
    untrustworthy: usize,
}

fn failed_correctness_count(summaries: &[BenchmarkSummary]) -> usize {
    summaries
        .iter()
        .filter(|summary| !summary.correctness.passed)
        .count()
}

fn budget_failure_count(summaries: &[BenchmarkSummary]) -> usize {
    summaries
        .iter()
        .filter(|summary| summary.budget_results.iter().any(|result| !result.passed))
        .count()
}

fn quality_gate_failures(run: &StressRun) -> Vec<&BenchmarkSummary> {
    let profile_config = &run.environment.profile_config;
    if !profile_config.fail_on_quality {
        return Vec::new();
    }
    run.summaries
        .iter()
        .filter(|summary| quality_rank(summary.quality) < quality_rank(profile_config.min_quality))
        .collect()
}

fn regression_gate_count(run: &StressRun) -> usize {
    if !run.environment.profile_config.fail_on_regression {
        return 0;
    }
    run.comparisons
        .iter()
        .filter(|comparison| comparison.classification == ComparisonClass::Regression)
        .count()
}

fn comparison_count(run: &StressRun, class: ComparisonClass) -> usize {
    run.comparisons
        .iter()
        .filter(|comparison| comparison_is_trustworthy(comparison))
        .filter(|comparison| comparison.classification == class)
        .count()
}

fn gate_status(run: &StressRun) -> String {
    if failed_correctness_count(&run.summaries) != 0 {
        return "failed correctness".to_string();
    }
    if budget_failure_count(&run.summaries) != 0 {
        return "failed budget".to_string();
    }
    let quality_failures = quality_gate_failures(run).len();
    if quality_failures != 0 {
        return format!(
            "failed quality ({quality_failures} below {})",
            run.environment.profile_config.min_quality
        );
    }
    let regressions = regression_gate_count(run);
    if regressions != 0 {
        return format!("failed regression ({regressions})");
    }
    "passed".to_string()
}

const fn quality_rank(quality: QualityClass) -> u8 {
    match quality {
        QualityClass::Untrustworthy => 0,
        QualityClass::Noisy => 1,
        QualityClass::Acceptable => 2,
        QualityClass::Authoritative => 3,
    }
}

fn is_trustworthy(summary: &BenchmarkSummary) -> bool {
    summary.correctness.passed
        && matches!(
            summary.quality,
            QualityClass::Authoritative | QualityClass::Acceptable
        )
}

fn compact_name(name: &str) -> String {
    let chars = name.chars().collect::<Vec<_>>();
    if chars.len() <= NAME_WIDTH {
        return name.to_string();
    }

    let keep = NAME_WIDTH.saturating_sub(2);
    let tail = chars[chars.len() - keep..].iter().collect::<String>();
    format!("..{tail}")
}

fn format_metric_value(value: f64, metric: PrimaryMetric) -> String {
    if !value.is_finite() {
        return "n/a".to_string();
    }
    match metric {
        PrimaryMetric::Throughput => format_compact_number(value),
        PrimaryMetric::LatencyP95 | PrimaryMetric::NsPerOp => {
            format_duration_ns(f64_to_u128(value))
        }
    }
}

fn format_optional_compact_stat(stats: Option<&SummaryStats>) -> String {
    stats.map_or_else(
        || "-".to_string(),
        |stats| format_compact_number(stats.mean),
    )
}

fn format_optional_duration_stat(stats: Option<&SummaryStats>) -> String {
    stats.map_or_else(
        || "-".to_string(),
        |stats| format_duration_ns(f64_to_u128(stats.mean)),
    )
}

fn format_compact_number(value: f64) -> String {
    let abs = value.abs();
    if abs >= 1_000_000.0 {
        format!("{:.2}M", value / 1_000_000.0)
    } else if abs >= 1_000.0 {
        format!("{:.2}K", value / 1_000.0)
    } else if abs >= 10.0 {
        format!("{value:.2}")
    } else if abs >= 0.01 {
        format!("{value:.4}")
    } else if abs == 0.0 {
        "0.00".to_string()
    } else {
        format!("{value:.2e}")
    }
}

fn format_percent(ratio: f64) -> String {
    if ratio.is_finite() {
        format!("{:.1}%", ratio * 100.0)
    } else {
        "n/a".to_string()
    }
}

fn short_commit(run: &StressRun) -> String {
    run.environment.git_commit.as_deref().map_or_else(
        || "unknown".to_string(),
        |commit| commit.chars().take(7).collect(),
    )
}

fn baseline_status(run: &StressRun) -> &'static str {
    if run
        .metadata
        .get("baseline_src")
        .is_some_and(|source| source != "default")
        || !run.comparisons.is_empty()
    {
        "found"
    } else {
        "none"
    }
}

fn machine_summary(run: &StressRun) -> String {
    let cores = run.environment.core_count.map_or_else(
        || "unknown cores".to_string(),
        |count| format!("{count} cores"),
    );
    format!("{}, {cores}", run.environment.cpu_model)
}

fn write_summary_line(output: &mut String, summary: &BenchmarkSummary) {
    let value = summary.primary_value().map_or_else(
        || "n/a".to_string(),
        |value| format_metric(value, summary.primary_metric),
    );
    let _ = writeln!(
        output,
        "  {name:<NAME_WIDTH$} {value:>VALUE_WIDTH$}  tier={tier} quality={quality} samples={samples} wall={wall}",
        name = summary.name,
        tier = summary.tier,
        quality = summary.quality,
        samples = summary.measured_samples,
        wall = format_duration_ns(summary.total_wall_clock_ns)
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
        PrimaryMetric::LatencyP95 | PrimaryMetric::NsPerOp => {
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
        format!("{:.2}µs", secs * 1_000_000.0)
    } else {
        format!("{:.2}ns", secs * 1_000_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{
        BenchmarkBudgets, BenchmarkMode, BenchmarkSpec, BudgetResult, ComparisonResult,
        CorrectnessCounters, CorrectnessSummary, EnvironmentInfo, Sample, SamplePhase,
        SummaryStats, SCHEMA_VERSION,
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
            wall_clock: SummaryStats::from_values(&[1_000_000.0]),
            total_wall_clock_ns: 1_000_000,
            ns_per_op: None,
            gross_ns_per_op: None,
            overhead_ns_per_op: None,
            allocs_per_op: None,
            bytes_per_op: None,
            quality,
            budgets: BenchmarkBudgets::default(),
            budget_results: Vec::new(),
            flags: Vec::new(),
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
        let profile_config =
            crate::config::StressRunnerConfig::for_profile(crate::result::RunProfile::Release)
                .profile_config();
        StressRun {
            schema_version: SCHEMA_VERSION.to_string(),
            tool_version: "0.3.0".to_string(),
            suite: "suite".to_string(),
            run_profile: profile_config.profile,
            environment: EnvironmentInfo::unknown(profile_config.clone()),
            benchmark_specs: vec![BenchmarkSpec {
                id: "bench".to_string(),
                name: "bench".to_string(),
                tier: 2,
                mode: BenchmarkMode::FixedOperations {
                    operations_per_sample: 1,
                },
                budgets: BenchmarkBudgets::default(),
                parameters: BTreeMap::new(),
                metadata: BTreeMap::new(),
            }],
            samples: vec![Sample {
                benchmark_id: "bench".to_string(),
                sample_number: 0,
                phase: SamplePhase::Measured,
                elapsed_ns: 1,
                wall_clock_ns: 1,
                operations_attempted: 1,
                operations_completed: 1,
                throughput: 1.0,
                calibrated_iterations: None,
                gross_elapsed_ns: None,
                overhead_ns: None,
                net_elapsed_ns: None,
                gross_ns_per_op: None,
                overhead_ns_per_op: None,
                net_ns_per_op: None,
                allocs: None,
                bytes: None,
                allocs_per_op: None,
                bytes_per_op: None,
                latency_ns: Vec::new(),
                parameters: BTreeMap::new(),
                counters: CorrectnessCounters {
                    attempted: 1,
                    completed: 1,
                    ..CorrectnessCounters::default()
                },
                environment: EnvironmentInfo::unknown(profile_config),
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
    fn formats_console_output_as_compact_bench_table() {
        let run = run_with_summaries(vec![
            summary("queue::fast", 1_000_000.0, QualityClass::Authoritative),
            summary("queue::slow", 10.0, QualityClass::Acceptable),
        ]);

        let report = format_console_output(&run, &run.summaries, ConsoleMode::Compact);

        assert!(report.contains("@cntryl/stress v0.3.0"));
        assert!(report.contains("suite  PASS  2 benches"));
        assert!(report.contains("Run summary"));
        assert!(!report.contains("Summary"));
        assert!(!report.contains("Quality"));
        assert!(!report.contains("Needs attention"));
        for hidden_column in [
            "mean", "p50", "p99", "wall", "overhead", "samples", "metric", "delta", "notes",
        ] {
            assert!(
                !report.contains(hidden_column),
                "compact output unexpectedly contained {hidden_column}: {report}"
            );
        }
    }

    #[test]
    fn compact_output_shows_attention_rows_and_hides_ok_rows() {
        let run = run_with_summaries(vec![
            summary("queue::fast", 1_000_000.0, QualityClass::Authoritative),
            summary("queue::noisy", 10.0, QualityClass::Noisy),
        ]);

        let report = format_console_output(&run, &run.summaries, ConsoleMode::Compact);

        assert!(report.contains("suite  FAIL  2 benches"));
        assert!(report.contains("benchmark"));
        assert!(report.contains("alloc/op"));
        assert!(report.contains("B/op"));
        assert!(report.contains("queue::noisy"));
        assert!(!report.contains("queue::fast"));
        assert!(report.contains("... 1 ok hidden; use --console full"));
        assert!(report.contains("attention:"));
        assert_eq!(report.matches("fix:").count(), 1);
        for hidden_column in [
            "mean", "p50", "p99", "wall", "overhead", "samples", "metric", "delta", "notes",
        ] {
            assert!(
                !report.contains(hidden_column),
                "compact output unexpectedly contained {hidden_column}: {report}"
            );
        }
    }

    #[test]
    fn full_output_shows_all_rows_with_narrow_columns() {
        let run = run_with_summaries(vec![
            summary("queue::fast", 1_000_000.0, QualityClass::Authoritative),
            summary("queue::slow", 10.0, QualityClass::Acceptable),
        ]);

        let report = format_console_output(&run, &run.summaries, ConsoleMode::Full);

        assert!(report.contains("benchmark"));
        assert!(report.contains("value"));
        assert!(report.contains("p95"));
        assert!(report.contains("rsd"));
        assert!(report.contains("alloc/op"));
        assert!(report.contains("B/op"));
        assert!(report.contains("queue::fast"));
        assert!(report.contains("queue::slow"));
        assert!(!report.contains("mean"));
        assert!(!report.contains("metric"));
        assert!(!report.contains("delta"));
        assert!(!report.contains("notes"));
    }

    #[test]
    fn verbose_output_keeps_diagnostic_columns_and_notes() {
        let run = run_with_summaries(vec![summary("queue::noisy", 10.0, QualityClass::Noisy)]);

        let report = format_console_output(&run, &run.summaries, ConsoleMode::Verbose);

        for column in [
            "metric", "mean", "p50", "p95", "p99", "overhead", "samples", "wall", "delta", "notes",
        ] {
            assert!(report.contains(column), "missing {column}: {report}");
        }
        assert!(report.contains("fix:"));
    }

    #[test]
    fn console_explains_quality_gate_failures() {
        let run = run_with_summaries(
            (0..6)
                .map(|index| summary(&format!("queue::row_{index}"), 100.0, QualityClass::Noisy))
                .collect(),
        );

        let report = format_console_output(&run, &run.summaries, ConsoleMode::Compact);

        assert!(report.contains("suite  FAIL  6 benches"));
        assert!(report.contains("correctness_bad: 0"));
        assert!(report.contains("quality_failed:  6"));
        assert!(report.contains("quality: 6 below acceptable"));
        assert!(!report.contains("Needs attention\n  none"));
    }

    #[test]
    fn markdown_explains_quality_gate_failures() {
        let run = run_with_summaries(
            (0..6)
                .map(|index| summary(&format!("queue::row_{index}"), 100.0, QualityClass::Noisy))
                .collect(),
        );

        let report = format_markdown_report(&run);

        assert!(report.contains("## Summary"));
        assert!(report.contains("gate:            failed quality (6 below acceptable)"));
        assert!(report.contains("quality_failed:  6"));
        assert!(report.contains("## Needs attention"));
        assert!(report.contains("quality gate failed"));
    }

    #[test]
    fn markdown_explains_budget_failures() {
        let mut budget = summary("budget", 100.0, QualityClass::Untrustworthy);
        budget.budget_results = vec![BudgetResult {
            metric: "max_allocs_per_op".to_string(),
            limit: 0.0,
            actual: Some(1.0),
            passed: false,
            reason: Some("1.0000 exceeds 0.0000".to_string()),
        }];
        let run = run_with_summaries(vec![budget]);

        let report = format_markdown_report(&run);

        assert!(report.contains("gate:            failed budget"));
        assert!(report.contains("budget_failed:   1"));
        assert!(report.contains("budget failed: max_allocs_per_op 1.0000 exceeds 0.0000"));
    }

    #[test]
    fn markdown_explains_tier_driven_recipe_misconfigurations() {
        let mut noisy = summary("tier3_noisy", 100.0, QualityClass::Noisy);
        noisy.tier = 3;
        let mut single_op = summary("tier3_single_op", 100.0, QualityClass::Acceptable);
        single_op.tier = 3;
        single_op.flags = vec!["tier_throughput_single_op".to_string()];
        let mut zero = summary("tier4_zero", 100.0, QualityClass::Untrustworthy);
        zero.tier = 4;
        zero.flags = vec!["zero_completed_ops".to_string()];
        let mut overhead = summary("tier1_overhead", 4.0, QualityClass::Untrustworthy);
        overhead.tier = 1;
        overhead.flags = vec!["overhead_dominant".to_string()];
        let mut suspicious = summary("tier1_suspicious", 4.0, QualityClass::Acceptable);
        suspicious.tier = 1;
        suspicious.flags = vec!["suspicious_micro".to_string()];
        let mut allocation = summary("tier2_allocation", 100.0, QualityClass::Untrustworthy);
        allocation.tier = 2;
        allocation.budget_results = vec![BudgetResult {
            metric: "max_allocs_per_op".to_string(),
            limit: 0.0,
            actual: None,
            passed: false,
            reason: Some("required measurement is unavailable".to_string()),
        }];
        let run = run_with_summaries(vec![
            noisy, single_op, zero, overhead, suspicious, allocation,
        ]);

        let report = format_markdown_report(&run);

        assert!(report.contains("Tier 3 recipe: #[stress_test(tier = 3)] with ctx.measure_batch"));
        assert!(report.contains("tier_throughput_single_op: only one completed op per sample"));
        assert!(report.contains("if it is one subsystem operation, use Tier 2"));
        assert!(report.contains("zero_completed_ops: completed operations were zero"));
        assert!(report.contains("ctx.record_external(duration, n)"));
        assert!(report.contains("overhead_dominant: timing overhead dominates"));
        assert!(report.contains("Tier 1 recipe: ctx.measure_micro"));
        assert!(report.contains("suspicious_micro: tier1_suspicious is below 5ns/op"));
        assert!(report.contains("cntryl_stress::stress_allocator!()"));
    }

    #[test]
    fn markdown_explains_regression_failures() {
        let mut run =
            run_with_summaries(vec![summary("regressed", 80.0, QualityClass::Acceptable)]);
        run.comparisons = vec![ComparisonResult {
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
        }];

        let report = format_markdown_report(&run);

        assert!(report.contains("gate:            failed regression (1)"));
        assert!(report.contains("regressions:     1"));
        assert!(report.contains("- ↓ regressed -20.0% regression"));
    }

    #[test]
    fn console_attention_orders_budget_before_regression_and_suspicious_micro() {
        let mut budget = summary("budget", 100.0, QualityClass::Untrustworthy);
        budget.budget_results = vec![BudgetResult {
            metric: "max_ns_per_op".to_string(),
            limit: 50.0,
            actual: Some(100.0),
            passed: false,
            reason: Some("100 exceeds 50".to_string()),
        }];
        let mut suspicious = summary("micro", 4.0, QualityClass::Acceptable);
        suspicious.flags = vec!["suspicious_micro".to_string()];
        let mut run = run_with_summaries(vec![
            budget,
            summary("regressed", 80.0, QualityClass::Acceptable),
            suspicious,
        ]);
        run.comparisons = vec![ComparisonResult {
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
        }];

        let report = format_console_output(&run, &run.summaries, ConsoleMode::Compact);
        let attention = report.split("attention:").nth(1).expect("attention block");

        assert!(
            attention.find("budget").expect("budget")
                < attention.find("regression").expect("regression")
        );
        assert!(
            attention.find("regression").expect("regression")
                < attention
                    .find("suspicious_micro")
                    .expect("suspicious micro")
        );
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
