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
const DEFAULT_ROWS_PER_GROUP: usize = 12;

/// Console reporter that prints compact progress to stdout.
pub struct ConsoleReporter {
    mode: ConsoleMode,
    summaries: Mutex<Vec<BenchmarkSummary>>,
    output_lock: Mutex<()>,
}

impl ConsoleReporter {
    /// Create a console reporter.
    #[must_use]
    pub fn new(mode: ConsoleMode) -> Self {
        Self {
            mode,
            summaries: Mutex::new(Vec::new()),
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
        Self::new(ConsoleMode::Default)
    }
}

impl Reporter for ConsoleReporter {
    fn bench_end(&self, summary: &BenchmarkSummary) {
        self.summaries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(summary.clone());
    }

    fn suite_end(&self, run: &StressRun) {
        let summaries = self
            .summaries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        self.write_stdout(&format_console_output(run, &summaries, self.mode));
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

fn format_console_output(
    run: &StressRun,
    summaries: &[BenchmarkSummary],
    mode: ConsoleMode,
) -> String {
    match mode {
        ConsoleMode::Default => format_human_console(run, summaries, false),
        ConsoleMode::Verbose => format_human_console(run, summaries, true),
        ConsoleMode::Quiet => format_summary_blocks(run),
        ConsoleMode::Json => serde_json::to_string_pretty(run).unwrap_or_else(|error| {
            format!(r#"{{"error":"failed to serialize stress run: {error}"}}"#)
        }),
        ConsoleMode::Markdown => format_markdown_report(run),
    }
}

fn format_human_console(run: &StressRun, summaries: &[BenchmarkSummary], verbose: bool) -> String {
    let summaries = if summaries.is_empty() {
        run.summaries.as_slice()
    } else {
        summaries
    };

    let mut output = String::new();
    write_console_header(&mut output, run);

    if !summaries.is_empty() {
        let _ = writeln!(output);
        write_grouped_tables(&mut output, run, summaries, verbose);
    }

    let _ = writeln!(output);
    output.push_str(&format_summary_blocks(run));
    write_attention_block(&mut output, run);
    write_console_footer(&mut output, summaries);
    output
}

fn write_console_header(output: &mut String, run: &StressRun) {
    let profile = &run.environment.profile_config;
    let _ = writeln!(output, "@cntryl/stress v{}", run.tool_version);
    let _ = writeln!(output, "suite: {}", run.suite);
    let _ = writeln!(
        output,
        "profile: {} | samples: {} measured, {} warmup, {} cooldown",
        run.run_profile, profile.measured_samples, profile.warmup_samples, profile.cooldown_samples
    );
    let _ = writeln!(
        output,
        "measure: {} fixed-duration default, {} op fixed-operations default",
        format_duration_ns(profile.sample_duration.as_nanos()),
        profile.operations_per_sample
    );
    let _ = writeln!(output, "commit: {}", short_commit(run));
    let _ = writeln!(
        output,
        "baseline: {} | threshold: {:.1}%",
        baseline_status(run),
        profile.regression_threshold * 100.0
    );
    let _ = writeln!(output, "machine: {}", machine_summary(run));
}

fn write_grouped_tables(
    output: &mut String,
    run: &StressRun,
    summaries: &[BenchmarkSummary],
    verbose: bool,
) {
    let comparisons = comparison_by_benchmark(run);
    for (group, rows) in grouped_summaries(summaries) {
        let _ = writeln!(output, "{group}");
        write_console_table_header(output);
        let limit = if verbose {
            rows.len()
        } else {
            DEFAULT_ROWS_PER_GROUP.min(rows.len())
        };
        for summary in rows.iter().take(limit) {
            let comparison = comparisons.get(summary.benchmark_id.as_str()).copied();
            write_console_table_row(output, summary, comparison, &group);
        }
        if rows.len() > limit {
            let _ = writeln!(
                output,
                "  ... {} more row(s); use --console verbose to show all",
                rows.len() - limit
            );
        }
        let _ = writeln!(output);
    }
}

fn write_console_table_header(output: &mut String) {
    let _ = writeln!(
        output,
        "  {name:<NAME_WIDTH$} {metric:>8} {value:>NUMBER_WIDTH$} {mean:>NUMBER_WIDTH$} {p50:>NUMBER_WIDTH$} {p95:>NUMBER_WIDTH$} {p99:>NUMBER_WIDTH$} {allocs:>NUMBER_WIDTH$} {bytes:>NUMBER_WIDTH$} {overhead:>NUMBER_WIDTH$} {rsd:>8} {quality:>13} {samples:>7} {delta:>18}  notes",
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
        delta = "delta"
    );
}

fn write_console_table_row(
    output: &mut String,
    summary: &BenchmarkSummary,
    comparison: Option<&ComparisonResult>,
    group: &str,
) {
    let name = compact_name(&display_name(&summary.name, group));
    let metric = metric_label(summary.primary_metric);
    let quality = if summary.correctness.passed {
        summary.quality.to_string()
    } else {
        "correctness_failed".to_string()
    };
    let marker = if summary.correctness.passed {
        " "
    } else {
        "✗"
    };
    let delta = comparison.map_or_else(|| "-".to_string(), format_delta_cell);
    let notes = row_notes(summary);

    if let Some(stats) = &summary.stats {
        let value = summary.primary_value().map_or_else(
            || "n/a".to_string(),
            |value| format_metric_value(value, summary.primary_metric),
        );
        let _ = writeln!(
            output,
            "{marker} {name:<NAME_WIDTH$} {metric:>8} {value:>NUMBER_WIDTH$} {mean:>NUMBER_WIDTH$} {p50:>NUMBER_WIDTH$} {p95:>NUMBER_WIDTH$} {p99:>NUMBER_WIDTH$} {allocs:>NUMBER_WIDTH$} {bytes:>NUMBER_WIDTH$} {overhead:>NUMBER_WIDTH$} {rsd:>8} {quality:>13} {samples:>7} {delta:>18}  {notes}",
            mean = format_metric_value(stats.mean, summary.primary_metric),
            p50 = format_metric_value(stats.p50, summary.primary_metric),
            p95 = format_metric_value(stats.p95, summary.primary_metric),
            p99 = format_metric_value(stats.p99, summary.primary_metric),
            allocs = format_optional_compact_stat(summary.allocs_per_op.as_ref()),
            bytes = format_optional_compact_stat(summary.bytes_per_op.as_ref()),
            overhead = format_optional_duration_stat(summary.overhead_ns_per_op.as_ref()),
            rsd = format_percent(stats.relative_std_dev),
            samples = summary.measured_samples,
        );
    } else {
        let unavailable = "n/a";
        let _ = writeln!(
            output,
            "{marker} {name:<NAME_WIDTH$} {metric:>8} {unavailable:>NUMBER_WIDTH$} {unavailable:>NUMBER_WIDTH$} {unavailable:>NUMBER_WIDTH$} {unavailable:>NUMBER_WIDTH$} {unavailable:>NUMBER_WIDTH$} {unavailable:>NUMBER_WIDTH$} {unavailable:>NUMBER_WIDTH$} {unavailable:>NUMBER_WIDTH$} {unavailable:>8} {quality:>13} {samples:>7} {delta:>18}  {notes}",
            samples = summary.measured_samples,
        );
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

fn write_attention_block(output: &mut String, run: &StressRun) {
    let items = attention_items(run);
    if items.is_empty() {
        let _ = writeln!(output);
        let _ = writeln!(output, "Needs attention");
        let _ = writeln!(output, "  none");
        return;
    }

    let _ = writeln!(output);
    let _ = writeln!(output, "Needs attention");
    for item in items.into_iter().take(8) {
        let _ = writeln!(output, "  {item}");
    }
}

fn write_console_footer(output: &mut String, summaries: &[BenchmarkSummary]) {
    if summaries
        .iter()
        .any(|summary| summary.primary_metric == PrimaryMetric::Throughput)
    {
        let _ = writeln!(output);
        let _ = writeln!(
            output,
            "Note: throughput p50/p95/p99 are sample-throughput percentiles, not operation latency percentiles."
        );
    }
}

fn attention_items(run: &StressRun) -> Vec<String> {
    let comparisons = comparison_by_benchmark(run);
    let mut items = Vec::new();
    let mut seen = BTreeSet::new();
    push_correctness_attention(&mut items, &mut seen, &run.summaries);
    push_budget_attention(&mut items, &mut seen, &run.summaries);
    push_quality_gate_attention(&mut items, &mut seen, run);
    push_comparison_attention(
        &mut items,
        &mut seen,
        &run.summaries,
        &comparisons,
        ComparisonClass::Regression,
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
            items.push(format!("! {} {flag}", summary.name));
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
                quality_note("quality", summary)
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

fn grouped_summaries(summaries: &[BenchmarkSummary]) -> BTreeMap<String, Vec<&BenchmarkSummary>> {
    let mut groups = BTreeMap::<String, Vec<&BenchmarkSummary>>::new();
    for summary in summaries {
        groups
            .entry(group_name(&summary.name))
            .or_default()
            .push(summary);
    }
    for rows in groups.values_mut() {
        rows.sort_by(|left, right| left.name.cmp(&right.name));
    }
    groups
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
        return summary.flags.join(", ");
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
                |reason| format!("{} {reason}", result.metric),
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn quality_note(label: &str, summary: &BenchmarkSummary) -> String {
    let mut parts = vec![label.to_string()];
    if summary.measured_samples < 2 {
        parts.push(format!("samples={}", summary.measured_samples));
    }
    if let Some(stats) = &summary.stats {
        parts.push(format!("rsd={}", format_percent(stats.relative_std_dev)));
    }
    parts.join(", ")
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

fn group_name(name: &str) -> String {
    name.split("::").next().unwrap_or(name).to_string()
}

fn display_name(name: &str, group: &str) -> String {
    name.strip_prefix(group)
        .and_then(|name| name.strip_prefix("::"))
        .unwrap_or(name)
        .to_string()
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
        CorrectnessCounters, CorrectnessSummary, EnvironmentInfo, ProfileConfig, Sample,
        SamplePhase, SummaryStats, SCHEMA_VERSION,
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
                budgets: BenchmarkBudgets::default(),
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
    fn formats_console_output_as_compact_bench_table() {
        let run = run_with_summaries(vec![
            summary("queue::fast", 1_000_000.0, QualityClass::Authoritative),
            summary("queue::slow", 10.0, QualityClass::Acceptable),
        ]);

        let report = format_console_output(&run, &run.summaries, ConsoleMode::Default);

        assert!(report.contains("@cntryl/stress v0.3.0"));
        assert!(report.contains("queue"));
        assert!(report.contains("name"));
        assert!(report.contains("ops/s"));
        assert!(report.contains("alloc/op"));
        assert!(report.contains("B/op"));
        assert!(report.contains("overhead"));
        assert!(report.contains("samples"));
        assert!(report.contains("authoritative"));
        assert!(report.contains("Summary"));
        assert!(report.contains("Needs attention"));
        assert!(report.contains("sample-throughput percentiles"));
    }

    #[test]
    fn console_explains_quality_gate_failures() {
        let run = run_with_summaries(
            (0..6)
                .map(|index| summary(&format!("queue::row_{index}"), 100.0, QualityClass::Noisy))
                .collect(),
        );

        let report = format_console_output(&run, &run.summaries, ConsoleMode::Default);

        assert!(report.contains("gate:            failed quality (6 below acceptable)"));
        assert!(report.contains("correctness_ok:  6"));
        assert!(report.contains("correctness_bad: 0"));
        assert!(report.contains("quality_failed:  6"));
        assert!(report.contains("quality gate failed"));
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

        let report = format_console_output(&run, &run.summaries, ConsoleMode::Default);
        let attention = report
            .split("Needs attention")
            .nth(1)
            .expect("attention block");

        assert!(
            attention.find("budget failed").expect("budget")
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
