//! Pluggable reporters for current stress artifacts.

use crate::config::StressRunnerConfig;
use crate::result::{
    BenchmarkDiagnostic, BenchmarkSummary, ComparisonClass, ComparisonResult, CorrectnessSummary,
    PrimaryMetric, QualityClass, StressRun, SummaryStats,
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
const HUMAN_TABLE_NAME_WIDTH: usize = 24;
const HUMAN_TABLE_VALUE_WIDTH: usize = 11;
const HUMAN_TABLE_ALLOC_WIDTH: usize = 9;
const VALUE_WIDTH: usize = 16;

/// Console reporter that prints the human benchmark table to stdout.
pub struct ConsoleReporter {
    output_lock: Mutex<()>,
}

impl ConsoleReporter {
    /// Create a console reporter.
    #[must_use]
    pub fn new() -> Self {
        Self {
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
        Self::new()
    }
}

impl Reporter for ConsoleReporter {
    fn suite_end(&self, run: &StressRun) {
        self.write_stdout(&format_console_run(run));
    }
}

pub(crate) struct JsonStdoutReporter {
    output_lock: Mutex<()>,
}

impl JsonStdoutReporter {
    pub(crate) fn new() -> Self {
        Self {
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

impl Reporter for JsonStdoutReporter {
    fn suite_end(&self, run: &StressRun) {
        let output = serde_json::to_string_pretty(run).unwrap_or_else(|error| {
            format!(r#"{{"error":"failed to serialize stress run: {error}"}}"#)
        });
        self.write_stdout(&output);
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

/// Format one stress run for the human console.
#[must_use]
pub fn format_console_run(run: &StressRun) -> String {
    format_human_console_runs(std::slice::from_ref(run))
}

/// Format multiple stress runs as one consolidated human console report.
#[must_use]
pub fn format_console_runs(runs: &[StressRun]) -> String {
    format_human_console_runs(runs)
}

#[cfg(test)]
fn format_console_output(run: &StressRun) -> String {
    format_console_run(run)
}

fn format_human_console_runs(runs: &[StressRun]) -> String {
    let mut output = String::new();
    write_run_header(&mut output, runs);
    let mut wrote_suite = false;
    for run in runs {
        if wrote_suite || !output.is_empty() {
            let _ = writeln!(output);
        }
        write_suite_block(&mut output, run);
        wrote_suite = true;
    }
    output
}

fn write_run_header(output: &mut String, runs: &[StressRun]) {
    let Some(first) = runs.first() else {
        return;
    };
    let _ = writeln!(output, "@cntryl/stress v{}", first.tool_version);
}

fn write_suite_block(output: &mut String, run: &StressRun) {
    let _ = writeln!(output, "{}", run.suite);

    let rows = rows_for_human_console(run);
    if rows.is_empty() {
        return;
    }

    let comparisons = comparison_by_benchmark(run);
    write_human_table(output, &rows, &comparisons);
}

fn rows_for_human_console(run: &StressRun) -> Vec<&BenchmarkSummary> {
    let comparisons = comparison_by_benchmark(run);
    let mut rows = run.summaries.iter().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        summary_attention_rank(run, left, &comparisons)
            .unwrap_or(u8::MAX)
            .cmp(&summary_attention_rank(run, right, &comparisons).unwrap_or(u8::MAX))
            .then_with(|| left.name.cmp(&right.name))
    });
    rows
}

fn write_human_table(
    output: &mut String,
    summaries: &[&BenchmarkSummary],
    comparisons: &BTreeMap<&str, &ComparisonResult>,
) {
    write_human_table_header(output);
    for summary in summaries {
        write_human_table_row(output, summary);
    }
    let issues = suite_issue_bullets(summaries, comparisons);
    if !issues.is_empty() {
        let _ = writeln!(output);
        let _ = writeln!(output, "issues");
        for issue in issues {
            let _ = writeln!(output, "  • {issue}");
        }
    }
}

fn write_human_table_header(output: &mut String) {
    let header = format!(
        "{benchmark:<HUMAN_TABLE_NAME_WIDTH$} {value:>HUMAN_TABLE_VALUE_WIDTH$} {mean:>HUMAN_TABLE_VALUE_WIDTH$} {p50:>HUMAN_TABLE_VALUE_WIDTH$} {p95:>HUMAN_TABLE_VALUE_WIDTH$} {p99:>HUMAN_TABLE_VALUE_WIDTH$} {rsd:>7} {allocs:>HUMAN_TABLE_ALLOC_WIDTH$} {bytes:>HUMAN_TABLE_ALLOC_WIDTH$}",
        benchmark = "benchmark",
        value = "value",
        mean = "mean",
        p50 = "p50",
        p95 = "p95",
        p99 = "p99",
        rsd = "rsd",
        allocs = "alloc/op",
        bytes = "B/op",
    );
    let _ = writeln!(output, "{header}");
    let _ = writeln!(output, "{}", "-".repeat(header.len()));
}

fn write_human_table_row(output: &mut String, summary: &BenchmarkSummary) {
    let name = truncate_name_to_width(&summary.name, HUMAN_TABLE_NAME_WIDTH);
    let value = summary.primary_value().map_or_else(
        || "n/a".to_string(),
        |value| format_metric_value(value, summary.primary_metric),
    );
    let stats = summary.stats.as_ref();
    let mean = format_metric_stat(stats, summary.primary_metric, |stats| stats.mean);
    let p50 = format_metric_stat(stats, summary.primary_metric, |stats| stats.p50);
    let p95 = format_metric_stat(stats, summary.primary_metric, |stats| stats.p95);
    let p99 = format_metric_stat(stats, summary.primary_metric, |stats| stats.p99);
    let rsd = stats.map_or_else(
        || "n/a".to_string(),
        |stats| format_percent(stats.relative_std_dev),
    );
    let _ = writeln!(
        output,
        "{name:<HUMAN_TABLE_NAME_WIDTH$} {value:>HUMAN_TABLE_VALUE_WIDTH$} {mean:>HUMAN_TABLE_VALUE_WIDTH$} {p50:>HUMAN_TABLE_VALUE_WIDTH$} {p95:>HUMAN_TABLE_VALUE_WIDTH$} {p99:>HUMAN_TABLE_VALUE_WIDTH$} {rsd:>7} {allocs:>HUMAN_TABLE_ALLOC_WIDTH$} {bytes:>HUMAN_TABLE_ALLOC_WIDTH$}",
        allocs = format_optional_scaled_stat(summary.allocs_per_op.as_ref()),
        bytes = format_optional_scaled_stat(summary.bytes_per_op.as_ref()),
    );
}

fn suite_issue_bullets(
    summaries: &[&BenchmarkSummary],
    comparisons: &BTreeMap<&str, &ComparisonResult>,
) -> Vec<String> {
    let mut issues = Vec::new();
    push_allocation_issue(&mut issues, summaries);
    push_diagnostic_issue(&mut issues, summaries, "high_variance", variance_issue);
    push_sample_count_issue(&mut issues, summaries);
    push_quality_issue(&mut issues, summaries);
    push_diagnostic_issue(
        &mut issues,
        summaries,
        "suspicious_micro_timing",
        |summary| {
            format!(
                "{} is suspiciously fast for a microbenchmark.",
                summary.name
            )
        },
    );
    push_diagnostic_issue(&mut issues, summaries, "too_fast", |summary| {
        format!("{} is too small for stable timing.", summary.name)
    });
    push_diagnostic_issue(
        &mut issues,
        summaries,
        "setup_dominates_measurement",
        |summary| format!("{} is dominated by setup or timing overhead.", summary.name),
    );
    push_diagnostic_issue(&mut issues, summaries, "single_op_throughput", |summary| {
        format!(
            "{} is a throughput-tier benchmark but records one operation per sample.",
            summary.name
        )
    });
    push_diagnostic_issue(&mut issues, summaries, "zero_completed_ops", |summary| {
        format!(
            "{} completed zero logical operations in at least one sample.",
            summary.name
        )
    });
    push_diagnostic_issue(&mut issues, summaries, "invalid_timing", |summary| {
        format!("{} recorded invalid timing.", summary.name)
    });
    for summary in summaries
        .iter()
        .copied()
        .filter(|summary| !summary.correctness.passed)
    {
        issues.push(format!("{} failed correctness checks.", summary.name));
    }
    for summary in summaries
        .iter()
        .copied()
        .filter(|summary| summary.budget_results.iter().any(|result| !result.passed))
    {
        issues.push(format!(
            "{} failed budget checks: {}.",
            summary.name,
            budget_note(summary)
        ));
    }
    for summary in summaries {
        if let Some(comparison) = comparisons.get(summary.benchmark_id.as_str()).copied() {
            match comparison.classification {
                ComparisonClass::Regression => issues.push(format!(
                    "{} regressed against baseline ({}).",
                    summary.name,
                    format_delta_cell(comparison)
                )),
                ComparisonClass::Improvement if comparison_is_trustworthy(comparison) => {
                    issues.push(format!(
                        "{} improved against baseline ({}).",
                        summary.name,
                        format_delta_cell(comparison)
                    ));
                }
                ComparisonClass::Inconclusive
                | ComparisonClass::Improvement
                | ComparisonClass::MissingBaseline => {}
            }
        }
    }
    issues
}

fn push_allocation_issue(issues: &mut Vec<String>, summaries: &[&BenchmarkSummary]) {
    let names = summaries
        .iter()
        .copied()
        .filter(|summary| has_diagnostic(summary, "high_allocations"))
        .map(|summary| summary.name.as_str())
        .collect::<Vec<_>>();
    match names.as_slice() {
        [] => {}
        [name] => issues.push(format!("{name} allocates memory during measurement.")),
        _ => issues.push(format!(
            "{} benchmarks allocate memory during measurement.",
            names.len()
        )),
    }
}

fn push_sample_count_issue(issues: &mut Vec<String>, summaries: &[&BenchmarkSummary]) {
    let names = summaries
        .iter()
        .copied()
        .filter(|summary| has_diagnostic(summary, "too_few_samples"))
        .map(|summary| summary.name.as_str())
        .collect::<Vec<_>>();
    match names.as_slice() {
        [] => {}
        [name] => issues.push(format!("{name} has too few measured samples.")),
        _ => issues.push(format!(
            "{} benchmarks have too few measured samples.",
            names.len()
        )),
    }
}

fn push_quality_issue(issues: &mut Vec<String>, summaries: &[&BenchmarkSummary]) {
    let noisy = summaries
        .iter()
        .copied()
        .filter(|summary| {
            summary.quality == QualityClass::Noisy && !has_diagnostic(summary, "high_variance")
        })
        .count();
    if noisy == 1 {
        issues.push("1 benchmark has noisy results.".to_string());
    } else if noisy > 1 {
        issues.push(format!("{noisy} benchmarks have noisy results."));
    }

    let untrustworthy = summaries
        .iter()
        .copied()
        .filter(|summary| {
            summary.quality == QualityClass::Untrustworthy
                && !has_diagnostic(summary, "too_few_samples")
                && !has_diagnostic(summary, "invalid_timing")
                && !has_diagnostic(summary, "zero_completed_ops")
                && !has_diagnostic(summary, "setup_dominates_measurement")
                && !has_diagnostic(summary, "budget_failure")
                && summary.correctness.passed
                && summary.budget_results.iter().all(|result| result.passed)
        })
        .count();
    if untrustworthy == 1 {
        issues.push("1 benchmark has untrustworthy results.".to_string());
    } else if untrustworthy > 1 {
        issues.push(format!(
            "{untrustworthy} benchmarks have untrustworthy results."
        ));
    }
}

fn push_diagnostic_issue<F>(
    issues: &mut Vec<String>,
    summaries: &[&BenchmarkSummary],
    code: &str,
    format_issue: F,
) where
    F: Fn(&BenchmarkSummary) -> String,
{
    for summary in summaries
        .iter()
        .copied()
        .filter(|summary| has_diagnostic(summary, code))
    {
        issues.push(format_issue(summary));
    }
}

fn variance_issue(summary: &BenchmarkSummary) -> String {
    let rsd = summary.stats.as_ref().map_or_else(
        || "unknown".to_string(),
        |stats| format_percent(stats.relative_std_dev),
    );
    format!("{} has elevated variance (RSD {rsd}).", summary.name)
}

fn has_diagnostic(summary: &BenchmarkSummary, code: &str) -> bool {
    summary
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == code)
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
    if !summary.diagnostics.is_empty() {
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
    push_diagnostic_attention(
        &mut items,
        &mut seen,
        &run.summaries,
        "single_op_throughput",
    );
    push_diagnostic_attention(
        &mut items,
        &mut seen,
        &run.summaries,
        "suspicious_micro_timing",
    );
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

fn push_diagnostic_attention(
    items: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    summaries: &[BenchmarkSummary],
    code: &str,
) {
    for summary in summaries
        .iter()
        .filter(|summary| summary.diagnostics.iter().any(|item| item.code == code))
    {
        if seen.insert(summary.benchmark_id.clone()) {
            items.push(format!(
                "! {} {}",
                summary.name,
                diagnostic_note(summary, code)
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
    if !summary.diagnostics.is_empty() {
        return summary
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic_detail(summary, diagnostic))
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

fn diagnostic_note(summary: &BenchmarkSummary, code: &str) -> String {
    summary
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == code)
        .map_or_else(
            || code.to_string(),
            |diagnostic| diagnostic_detail(summary, diagnostic),
        )
}

fn diagnostic_detail(summary: &BenchmarkSummary, diagnostic: &BenchmarkDiagnostic) -> String {
    let mut detail = format!("{}: {}", diagnostic.code, diagnostic.reason);
    if !diagnostic.suggestions.is_empty() {
        detail.push_str(" fix: ");
        detail.push_str(&diagnostic.suggestions.join("; "));
    }
    if matches!(
        diagnostic.code.as_str(),
        "zero_completed_ops" | "single_op_throughput" | "too_fast" | "setup_dominates_measurement"
    ) {
        detail.push_str("; ");
        detail.push_str(tier_recipe(summary));
    }
    detail
}

fn tier_recipe(summary: &BenchmarkSummary) -> &'static str {
    match summary.tier {
        1 => "Tier 1 recipe: ctx.measure(\"name\", || hot_path())",
        2 => {
            "Tier 2 recipe: ctx.measure(\"name\", || one_operation()) or ctx.measure_batch(\"name\", n, || batch())"
        }
        3 => "Tier 3 recipe: #[stress(tier = 3)] with ctx.measure_batch(\"name\", n, || batch())",
        4 => "Tier 4 recipe: #[stress(tier = 4)] with ctx.measure_batch(\"name\", n, || batch()) or ctx.record_external(\"name\", duration, n)",
        5 => "Tier 5 recipe: #[stress(tier = 5)] with scale parameters and ctx.measure_batch(\"name\", n, || batch())",
        6 => "Tier 6 recipe: #[stress(tier = 6)] with ctx.measure_batch(\"name\", n, || batch()) or ctx.record_external(\"name\", duration, n)",
        _ => "undefined tier: cntryl-stress defines tiers 1 through 6; choose the closest defined tier before authoring the benchmark",
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

fn truncate_name_to_width(name: &str, width: usize) -> String {
    let chars = name.chars().collect::<Vec<_>>();
    if chars.len() <= width {
        return name.to_string();
    }

    let keep = width.saturating_sub(2);
    let tail = chars[chars.len() - keep..].iter().collect::<String>();
    format!("..{tail}")
}

fn format_metric_value(value: f64, metric: PrimaryMetric) -> String {
    if !value.is_finite() {
        return "n/a".to_string();
    }
    match metric {
        PrimaryMetric::Throughput => format_scaled_number(value),
        PrimaryMetric::LatencyP95 | PrimaryMetric::NsPerOp => {
            format_duration_ns(f64_to_u128(value))
        }
    }
}

fn format_optional_scaled_stat(stats: Option<&SummaryStats>) -> String {
    stats.map_or_else(|| "-".to_string(), |stats| format_scaled_number(stats.mean))
}

fn format_metric_stat<F>(stats: Option<&SummaryStats>, metric: PrimaryMetric, select: F) -> String
where
    F: FnOnce(&SummaryStats) -> f64,
{
    stats.map_or_else(
        || "n/a".to_string(),
        |stats| format_metric_value(select(stats), metric),
    )
}

fn format_scaled_number(value: f64) -> String {
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
        CorrectnessCounters, CorrectnessSummary, DiagnosticSeverity, EnvironmentInfo,
        MeasurementIntent, Sample, SamplePhase, SummaryStats, SCHEMA_VERSION,
    };

    fn summary(name: &str, value: f64, quality: QualityClass) -> BenchmarkSummary {
        BenchmarkSummary {
            benchmark_id: name.to_string(),
            name: name.to_string(),
            tier: 2,
            intent: MeasurementIntent::General,
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
            diagnostics: Vec::new(),
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
                intent: MeasurementIntent::General,
                budgets: BenchmarkBudgets::default(),
                parameters: BTreeMap::new(),
                metadata: BTreeMap::new(),
            }],
            samples: vec![Sample {
                benchmark_id: "bench".to_string(),
                intent: MeasurementIntent::General,
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

    fn diagnostic(code: &str, reason: &str, suggestion: &str) -> BenchmarkDiagnostic {
        BenchmarkDiagnostic {
            code: code.to_string(),
            severity: DiagnosticSeverity::Warning,
            reason: reason.to_string(),
            evidence: BTreeMap::new(),
            suggestions: vec![suggestion.to_string()],
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
    fn formats_console_output_as_human_table() {
        let run = run_with_summaries(vec![
            summary("queue::fast", 1_000_000.0, QualityClass::Authoritative),
            summary("queue::slow", 10.0, QualityClass::Acceptable),
        ]);

        let report = format_console_output(&run);

        assert!(report.contains("@cntryl/stress v0.3.0"));
        assert!(report.contains("suite"));
        assert!(report.contains("benchmark"));
        assert!(report.contains("value"));
        assert!(report.contains("mean"));
        assert!(report.contains("p50"));
        assert!(report.contains("p95"));
        assert!(report.contains("p99"));
        assert!(report.contains("rsd"));
        assert!(report.contains("alloc/op"));
        assert!(report.contains("B/op"));
        assert!(report.contains("queue::fast"));
        assert!(report.contains("1.00M"));
        assert!(!report.contains("Summary"));
        assert!(!report.contains("Quality"));
        assert!(!report.contains("issues"));
        assert!(!report.contains("Run summary"));
    }

    #[test]
    fn human_console_lists_issues_after_table() {
        let mut noisy = summary("queue::noisy", 10.0, QualityClass::Noisy);
        noisy.diagnostics = vec![diagnostic(
            "high_variance",
            "Measured samples varied.",
            "Use deterministic fixtures.",
        )];
        let run = run_with_summaries(vec![
            summary("queue::fast", 1_000_000.0, QualityClass::Authoritative),
            noisy,
        ]);

        let report = format_console_output(&run);

        assert!(report.contains("queue::noisy"));
        assert!(report.contains("issues"));
        assert!(report.contains("• queue::noisy has elevated variance"));
        assert!(!report.contains("issue   "));
    }

    #[test]
    fn console_explains_quality_gate_failures() {
        let run = run_with_summaries(
            (0..6)
                .map(|index| summary(&format!("queue::row_{index}"), 100.0, QualityClass::Noisy))
                .collect(),
        );

        let report = format_console_output(&run);

        assert!(report.contains("queue::row_0"));
        assert!(report.contains("issues"));
        assert!(report.contains("6 benchmarks have noisy results."));
        assert!(!report.contains("summary: gate"));
        assert!(!report.contains("attention:"));
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
        single_op.diagnostics = vec![diagnostic(
            "single_op_throughput",
            "A throughput-tier row completed only one operation per sample.",
            "Use measure_batch or record_external.",
        )];
        let mut zero = summary("tier4_zero", 100.0, QualityClass::Untrustworthy);
        zero.tier = 4;
        zero.diagnostics = vec![diagnostic(
            "zero_completed_ops",
            "At least one measured sample completed zero logical operations.",
            "Record completed logical work.",
        )];
        let mut overhead = summary("tier1_overhead", 4.0, QualityClass::Untrustworthy);
        overhead.tier = 1;
        overhead.diagnostics = vec![diagnostic(
            "setup_dominates_measurement",
            "Timing overhead or setup dominates the measured work.",
            "Increase measured work per iteration.",
        )];
        let mut suspicious = summary("tier1_suspicious", 4.0, QualityClass::Acceptable);
        suspicious.tier = 1;
        suspicious.diagnostics = vec![diagnostic(
            "suspicious_micro_timing",
            "Tier 1 timing is below 5 ns/op without explicit validation.",
            "Validate the microbenchmark independently.",
        )];
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

        assert!(report.contains("Tier 3 recipe: #[stress(tier = 3)] with ctx.measure_batch"));
        assert!(report.contains(
            "single_op_throughput: A throughput-tier row completed only one operation per sample"
        ));
        assert!(report.contains("Use measure_batch or record_external."));
        assert!(report.contains(
            "zero_completed_ops: At least one measured sample completed zero logical operations"
        ));
        assert!(report.contains("ctx.record_external(\"name\", duration, n)"));
        assert!(report.contains("setup_dominates_measurement: Timing overhead or setup dominates"));
        assert!(report.contains("Tier 1 recipe: ctx.measure(\"name\", || hot_path())"));
        assert!(report.contains("suspicious_micro_timing: Tier 1 timing is below 5 ns/op"));
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
    fn human_console_lists_budget_regression_and_micro_issues() {
        let mut budget = summary("budget", 100.0, QualityClass::Untrustworthy);
        budget.budget_results = vec![BudgetResult {
            metric: "max_ns_per_op".to_string(),
            limit: 50.0,
            actual: Some(100.0),
            passed: false,
            reason: Some("100 exceeds 50".to_string()),
        }];
        let mut suspicious = summary("micro", 4.0, QualityClass::Acceptable);
        suspicious.diagnostics = vec![diagnostic(
            "suspicious_micro_timing",
            "Tier 1 timing is below 5 ns/op without explicit validation.",
            "Validate the microbenchmark independently.",
        )];
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

        let report = format_console_output(&run);

        assert!(report.contains("budget failed"));
        assert!(report.contains("regression"));
        assert!(report.contains("micro is suspiciously fast"));
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
