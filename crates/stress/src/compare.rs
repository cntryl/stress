//! Offline comparison of two saved run artifacts (`cargo stress compare`).
//!
//! This reuses the exact engine behind `--baseline`: both artifacts are
//! validated and their summaries recomputed from raw samples (including
//! legacy summary semantics), then compared with the same threshold,
//! confidence-interval overlap, and trust rules.

use crate::artifact::{
    compare_summaries_with_specs, incompatible_environment_reason, ComparisonClass,
    ComparisonResult, SourceLocation, StressRun,
};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Version tag of the JSON document produced by [`render_json`].
pub const COMPARE_JSON_SCHEMA: &str = "cntryl-stress-compare/1";

/// Overall result of comparing two artifacts, mapped to a process exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CompareOutcome {
    /// At least one row was validly compared and no gating row regressed. Exit 0.
    NoRegression,
    /// At least one gating row regressed. Exit 1.
    Regression,
    /// Environments are incompatible and `--ignore-env` was not given. Exit 2.
    IncompatibleEnvironment,
    /// An intended-gate or budgeted row, or every row, could not be validly
    /// compared (missing from the baseline or rejected with a reason). Exit 2.
    Inconclusive,
}

impl CompareOutcome {
    /// Process exit code for this outcome.
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::NoRegression => 0,
            Self::Regression => 1,
            Self::IncompatibleEnvironment | Self::Inconclusive => 2,
        }
    }
}

/// Options for [`compare_runs`].
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CompareOptions {
    /// Regression threshold as a fraction. Defaults to the candidate profile's threshold.
    pub threshold: Option<f64>,
    /// Compare even when the environments are incompatible.
    pub ignore_environment: bool,
}

impl CompareOptions {
    /// Default options.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the regression threshold as a fraction (`0.05` means 5%).
    #[must_use]
    pub const fn threshold(mut self, threshold: f64) -> Self {
        self.threshold = Some(threshold);
        self
    }

    /// Compare despite an incompatible environment.
    #[must_use]
    pub const fn ignore_environment(mut self, ignore: bool) -> Self {
        self.ignore_environment = ignore;
        self
    }
}

/// One compared row.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct CompareRow {
    /// Engine comparison result.
    pub comparison: ComparisonResult,
    /// Whether a regression on this row fails the comparison.
    pub gating: bool,
    /// Candidate source location, when recorded.
    pub source: Option<SourceLocation>,
}

/// Result of [`compare_runs`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RunComparison {
    /// Baseline suite name.
    pub baseline_suite: String,
    /// Candidate suite name.
    pub candidate_suite: String,
    /// Rows in candidate order.
    pub rows: Vec<CompareRow>,
    /// Environment incompatibility reason, when the environments differ.
    pub environment_mismatch: Option<String>,
    /// Whether the mismatch was ignored.
    pub environment_ignored: bool,
    /// Overall outcome.
    pub outcome: CompareOutcome,
}

/// Resolve an artifact argument: a directory means its `latest.json`.
#[must_use]
pub fn resolve_artifact_path(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join("latest.json")
    } else {
        path.to_path_buf()
    }
}

/// Load and compare two artifacts with the `--baseline` engine.
///
/// # Errors
///
/// Returns an error when the threshold is invalid, when the baseline is not
/// an eligible (passed) run, or when either artifact fails canonical
/// evidence validation.
pub fn compare_runs(
    baseline: &StressRun,
    candidate: &StressRun,
    options: &CompareOptions,
) -> Result<RunComparison, String> {
    let threshold = options
        .threshold
        .unwrap_or(candidate.environment.profile_config.regression_threshold);
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        return Err(format!(
            "threshold {threshold} must be finite and between 0 and 1"
        ));
    }
    let baseline_gate = crate::runner::evaluate_run_gate(baseline);
    if baseline_gate != crate::runner::RunGate::Passed {
        return Err(format!(
            "baseline run is not eligible because its recorded gate evaluates to {baseline_gate:?}; use a passed run saved with --save-baseline"
        ));
    }
    let baseline_summaries = baseline
        .canonical_baseline_summaries()
        .map_err(|error| format!("invalid baseline artifact: {error}"))?;
    let candidate_summaries = candidate
        .canonical_baseline_summaries()
        .map_err(|error| format!("invalid candidate artifact: {error}"))?;

    let environment_mismatch =
        incompatible_environment_reason(&candidate.environment, &baseline.environment);
    let environment_ignored = options.ignore_environment && environment_mismatch.is_some();
    let baseline_environment = if environment_ignored {
        &candidate.environment
    } else {
        &baseline.environment
    };
    let comparisons = compare_summaries_with_specs(
        &candidate_summaries,
        &candidate.benchmark_specs,
        &candidate.environment,
        &baseline_summaries,
        &baseline.benchmark_specs,
        baseline_environment,
        threshold,
    );

    // Gate semantics mirror `--baseline`: the stored candidate summaries carry
    // the trust class and budgets the live run would have used, so evaluate
    // the public gate predicates on the candidate with these comparisons.
    let mut gate_view = candidate.clone();
    gate_view.comparisons.clone_from(&comparisons);
    let rows = comparisons
        .into_iter()
        .map(|comparison| {
            let stored = candidate
                .summaries
                .iter()
                .find(|summary| summary.benchmark_id == comparison.benchmark_id);
            CompareRow {
                gating: stored.is_some_and(|summary| {
                    (summary.is_intended_gate() && summary.is_gate())
                        || summary.budgets.max_regression_pct.is_some()
                }),
                source: stored.and_then(|summary| summary.source.clone()),
                comparison,
            }
        })
        .collect::<Vec<_>>();

    let outcome = if environment_mismatch.is_some() && !environment_ignored {
        CompareOutcome::IncompatibleEnvironment
    } else if rows
        .iter()
        .any(|row| row.gating && row.comparison.classification == ComparisonClass::Regression)
    {
        CompareOutcome::Regression
    } else if !gate_view.regression_budgets_passed()
        || !gate_view.rejected_gate_comparisons().is_empty()
        || rows.iter().all(|row| {
            row.comparison.classification == ComparisonClass::MissingBaseline
                || row.comparison.reason.is_some()
        })
    {
        CompareOutcome::Inconclusive
    } else {
        CompareOutcome::NoRegression
    };

    Ok(RunComparison {
        baseline_suite: baseline.suite.clone(),
        candidate_suite: candidate.suite.clone(),
        rows,
        environment_mismatch,
        environment_ignored,
        outcome,
    })
}

const fn classification_label(class: ComparisonClass) -> &'static str {
    match class {
        ComparisonClass::Regression => "regression",
        ComparisonClass::Improvement => "improvement",
        ComparisonClass::Inconclusive => "inconclusive",
        ComparisonClass::MissingBaseline => "missing_baseline",
    }
}

const fn outcome_label(outcome: CompareOutcome) -> &'static str {
    match outcome {
        CompareOutcome::NoRegression => "no regression",
        CompareOutcome::Regression => "regression",
        CompareOutcome::IncompatibleEnvironment => "incompatible environment",
        CompareOutcome::Inconclusive => "inconclusive",
    }
}

fn format_value(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_string(), |value| format!("{value:.3}"))
}

fn format_change(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_string(), |value| format!("{value:+.2}%"))
}

fn row_class(row: &CompareRow) -> String {
    let mut class = classification_label(row.comparison.classification).to_string();
    if row.comparison.classification == ComparisonClass::Regression && !row.gating {
        class.push_str(" (non-gating)");
    }
    class
}

/// Plain-text rendering.
#[must_use]
pub fn render_text(comparison: &RunComparison) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if let Some(reason) = &comparison.environment_mismatch {
        if comparison.environment_ignored {
            let _ = writeln!(out, "!!! ENVIRONMENT MISMATCH IGNORED (--ignore-env) !!!");
            let _ = writeln!(out, "!!! {reason}");
            let _ = writeln!(out);
        } else {
            let _ = writeln!(out, "incompatible environment: {reason}");
        }
    }
    let _ = writeln!(
        out,
        "compare {} (baseline) -> {} (candidate): {}",
        comparison.baseline_suite,
        comparison.candidate_suite,
        outcome_label(comparison.outcome)
    );
    for row in &comparison.rows {
        let c = &row.comparison;
        let _ = write!(
            out,
            "  {}  {} -> {}  {}  {}",
            c.benchmark_id,
            format_value(c.baseline_value),
            format_value(c.current_value),
            format_change(c.change_percent),
            row_class(row)
        );
        if let Some(source) = &row.source {
            let _ = write!(out, "  [{source}]");
        }
        if let Some(reason) = &c.reason {
            let _ = write!(out, "  ({reason})");
        }
        let _ = writeln!(out);
    }
    out
}

/// Escape artifact-controlled text for GitHub-flavored markdown: HTML is
/// entity-escaped, markdown punctuation (including `|`, links, and
/// `@mentions`) is backslash-escaped, and newlines are flattened, so the text
/// is safe inside a table cell or blockquote of a PR comment.
fn escape_untrusted_markdown(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '&' => escaped.push_str("&amp;"),
            '\r' | '\n' => escaped.push(' '),
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '!' | '|' | '@'
            | '~' => {
                escaped.push('\\');
                escaped.push(character);
            }
            other => escaped.push(other),
        }
    }
    escaped
}

/// Markdown rendering suitable for `gh pr comment --body-file -`.
#[must_use]
pub fn render_markdown(comparison: &RunComparison) -> String {
    use std::fmt::Write as _;
    let esc = escape_untrusted_markdown;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "## cntryl-stress compare: {}",
        outcome_label(comparison.outcome)
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Baseline {} vs candidate {}.",
        esc(&comparison.baseline_suite),
        esc(&comparison.candidate_suite)
    );
    let _ = writeln!(out);
    if let Some(reason) = &comparison.environment_mismatch {
        let heading = if comparison.environment_ignored {
            "Environment mismatch ignored (--ignore-env); results may not be meaningful"
        } else {
            "Incompatible environment; rows were not compared"
        };
        let _ = writeln!(out, "> [!WARNING]");
        let _ = writeln!(out, "> **{heading}.**");
        let _ = writeln!(out, "> {}", esc(reason));
        let _ = writeln!(out);
    }
    let _ = writeln!(out, "| Row | Baseline | Candidate | Change | Class |");
    let _ = writeln!(out, "| --- | ---: | ---: | ---: | --- |");
    for row in &comparison.rows {
        let c = &row.comparison;
        let mut name = esc(&c.benchmark_id);
        if let Some(source) = &row.source {
            let _ = write!(name, " ({})", esc(&source.to_string()));
        }
        let mut class = row_class(row);
        if let Some(reason) = &c.reason {
            let _ = write!(class, ": {reason}");
        }
        let _ = writeln!(
            out,
            "| {name} | {} | {} | {} | {} |",
            format_value(c.baseline_value),
            format_value(c.current_value),
            format_change(c.change_percent),
            esc(&class)
        );
    }
    out
}

#[derive(Serialize)]
struct JsonEnvironment<'a> {
    mismatch: Option<&'a str>,
    ignored: bool,
}

#[derive(Serialize)]
struct JsonRow<'a> {
    benchmark_id: &'a str,
    primary_metric: crate::artifact::PrimaryMetric,
    baseline_value: Option<f64>,
    candidate_value: Option<f64>,
    change_percent: Option<f64>,
    threshold: f64,
    confidence_intervals_overlap: Option<bool>,
    classification: ComparisonClass,
    gating: bool,
    reason: Option<&'a str>,
    source: Option<&'a SourceLocation>,
}

#[derive(Serialize)]
struct JsonDocument<'a> {
    schema: &'static str,
    outcome: CompareOutcome,
    exit_code: i32,
    baseline_suite: &'a str,
    candidate_suite: &'a str,
    environment: JsonEnvironment<'a>,
    rows: Vec<JsonRow<'a>>,
}

/// JSON rendering (see the README for the documented shape).
#[must_use]
pub fn render_json(comparison: &RunComparison) -> String {
    let document = JsonDocument {
        schema: COMPARE_JSON_SCHEMA,
        outcome: comparison.outcome,
        exit_code: comparison.outcome.exit_code(),
        baseline_suite: &comparison.baseline_suite,
        candidate_suite: &comparison.candidate_suite,
        environment: JsonEnvironment {
            mismatch: comparison.environment_mismatch.as_deref(),
            ignored: comparison.environment_ignored,
        },
        rows: comparison
            .rows
            .iter()
            .map(|row| JsonRow {
                benchmark_id: &row.comparison.benchmark_id,
                primary_metric: row.comparison.primary_metric,
                baseline_value: row.comparison.baseline_value,
                candidate_value: row.comparison.current_value,
                change_percent: row.comparison.change_percent,
                threshold: row.comparison.threshold,
                confidence_intervals_overlap: row.comparison.confidence_intervals_overlap,
                classification: row.comparison.classification,
                gating: row.gating,
                reason: row.comparison.reason.as_deref(),
                source: row.source.as_ref(),
            })
            .collect(),
    };
    let mut json = serde_json::to_string_pretty(&document).unwrap_or_default();
    json.push('\n');
    json
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{QualityClass, RunProfile};
    use crate::{StressRunner, StressRunnerConfig};
    use std::time::Duration;

    pub(crate) fn run_with(suite: &str, id: &str, millis: u64) -> StressRun {
        let config = StressRunnerConfig::for_profile(RunProfile::Default)
            .samples(10)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config(suite, config);
        runner.reporters(Vec::new());
        runner.run(id, |ctx| {
            ctx.record_external("work", Duration::from_millis(millis), 500);
        });
        let run = runner.finish();
        assert!(run.meets_min_quality(QualityClass::Acceptable));
        run
    }

    fn run_two(suite: &str, first: &str, second: &str) -> StressRun {
        let config = StressRunnerConfig::for_profile(RunProfile::Default)
            .samples(10)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config(suite, config);
        runner.reporters(Vec::new());
        for id in [first, second] {
            runner.run(id, |ctx| {
                ctx.record_external("work", Duration::from_millis(10), 500);
            });
        }
        runner.finish()
    }

    #[test]
    fn uncompared_intended_gate_row_is_not_a_pass() {
        let base = run_with("s", "a", 10);
        let cand = run_two("s", "a", "b");
        let result = compare_runs(&base, &cand, &CompareOptions::new()).expect("compare");
        assert_eq!(result.outcome, CompareOutcome::Inconclusive);
    }

    #[test]
    fn regression_budget_rows_gate_regardless_of_trust() {
        let base = run_with("s", "bench", 10);
        let mut cand = run_with("s", "bench", 20);
        cand.summaries[0].trust_class = crate::artifact::TrustClass::Diagnostic;
        cand.summaries[0].budgets.max_regression_pct = Some(5.0);
        cand.summaries[0]
            .metadata
            .insert("trust_class".to_string(), "diagnostic".to_string());
        for spec in &mut cand.benchmark_specs {
            spec.budgets.max_regression_pct = Some(5.0);
            spec.metadata
                .insert("trust_class".to_string(), "diagnostic".to_string());
        }
        let result = compare_runs(&base, &cand, &CompareOptions::new()).expect("compare");
        assert_eq!(result.outcome, CompareOutcome::Regression);
    }

    #[test]
    fn markdown_neutralizes_untrusted_html_and_links() {
        let base = run_with("s", "bench", 10);
        let mut cand = run_with("s", "bench", 20);
        let evil = "<!-- [x](http://e) @team";
        cand.environment.cpu_model = evil.to_string();
        for sample in &mut cand.samples {
            sample.environment.cpu_model = evil.to_string();
        }
        cand.summaries[0].source = Some(SourceLocation::new("<b>x</b>.rs", 1));
        let result = compare_runs(&base, &cand, &CompareOptions::new()).expect("compare");
        let md = render_markdown(&result);
        assert!(!md.contains('<'), "{md}");
        assert!(!md.contains("[x]("), "{md}");
        assert!(!md.contains(" @team"), "{md}");
        assert!(!render_text(&result).contains("error:"));
    }

    fn change_cpu(run: &mut StressRun) {
        run.environment.cpu_model = "Other CPU".to_string();
        for sample in &mut run.samples {
            sample.environment.cpu_model = "Other CPU".to_string();
        }
    }

    #[test]
    fn identical_runs_do_not_regress() {
        let base = run_with("s", "bench", 10);
        let cand = run_with("s", "bench", 10);
        let result = compare_runs(&base, &cand, &CompareOptions::new()).expect("compare");
        assert_eq!(result.outcome, CompareOutcome::NoRegression);
        assert_eq!(result.outcome.exit_code(), 0);
        assert_eq!(result.rows.len(), 1);
        assert!(result.rows[0].gating);
    }

    #[test]
    fn slower_candidate_is_a_gating_regression() {
        let base = run_with("s", "bench", 10);
        let cand = run_with("s", "bench", 20);
        let result = compare_runs(&base, &cand, &CompareOptions::new()).expect("compare");
        assert_eq!(result.outcome, CompareOutcome::Regression);
        assert_eq!(result.outcome.exit_code(), 1);
        assert_eq!(
            result.rows[0].comparison.classification,
            ComparisonClass::Regression
        );
    }

    #[test]
    fn threshold_option_is_applied() {
        let base = run_with("s", "bench", 10);
        let cand = run_with("s", "bench", 11);
        let strict =
            compare_runs(&base, &cand, &CompareOptions::new().threshold(0.05)).expect("compare");
        assert_eq!(strict.outcome, CompareOutcome::Regression);
        let loose =
            compare_runs(&base, &cand, &CompareOptions::new().threshold(0.5)).expect("compare");
        assert_eq!(loose.outcome, CompareOutcome::NoRegression);
        assert!(compare_runs(&base, &cand, &CompareOptions::new().threshold(1.5)).is_err());
    }

    #[test]
    fn only_missing_rows_is_inconclusive() {
        let base = run_with("s", "bench", 10);
        let cand = run_with("s", "renamed", 20);
        let result = compare_runs(&base, &cand, &CompareOptions::new()).expect("compare");
        assert_eq!(result.outcome, CompareOutcome::Inconclusive);
        assert_eq!(result.outcome.exit_code(), 2);
    }

    #[test]
    fn environment_mismatch_is_exit_two_unless_ignored() {
        let base = run_with("s", "bench", 10);
        let mut cand = run_with("s", "bench", 20);
        change_cpu(&mut cand);
        let result = compare_runs(&base, &cand, &CompareOptions::new()).expect("compare");
        assert_eq!(result.outcome, CompareOutcome::IncompatibleEnvironment);
        let reason = result.environment_mismatch.clone().expect("reason");
        assert!(reason.contains("CPU model"), "{reason}");
        assert!(!result.environment_ignored);

        let ignored = compare_runs(
            &base,
            &cand,
            &CompareOptions::new().ignore_environment(true),
        )
        .expect("compare");
        assert_eq!(ignored.outcome, CompareOutcome::Regression);
        assert!(ignored.environment_ignored);
        assert_eq!(ignored.environment_mismatch, Some(reason));
        assert!(render_text(&ignored).contains("ENVIRONMENT MISMATCH IGNORED"));
        assert!(render_markdown(&ignored).contains("Environment mismatch ignored"));
        let json: serde_json::Value = serde_json::from_str(&render_json(&ignored)).expect("json");
        assert_eq!(json["environment"]["ignored"], true);
    }

    #[test]
    fn invalid_artifacts_are_rejected() {
        let base = run_with("s", "bench", 10);
        let mut tampered = run_with("s", "bench", 10);
        tampered.samples.clear();
        assert!(compare_runs(&base, &tampered, &CompareOptions::new()).is_err());
        assert!(compare_runs(&tampered, &base, &CompareOptions::new()).is_err());
    }

    #[test]
    fn markdown_escapes_cells_and_prints_source() {
        let base = run_with("s", "a|b", 10);
        let mut cand = run_with("s", "a|b", 20);
        cand.summaries[0].source = Some(SourceLocation::new("benches/x.rs", 42));
        let result = compare_runs(&base, &cand, &CompareOptions::new()).expect("compare");
        let md = render_markdown(&result);
        assert!(md.contains("a\\|b"), "{md}");
        assert!(md.contains("benches/x.rs:42"), "{md}");
        assert!(
            md.contains("| Row | Baseline | Candidate | Change | Class |"),
            "{md}"
        );
        assert!(md.contains("regression"), "{md}");
    }

    #[test]
    fn json_shape_is_stable() {
        let base = run_with("s", "bench", 10);
        let cand = run_with("s", "bench", 20);
        let result = compare_runs(&base, &cand, &CompareOptions::new()).expect("compare");
        let json: serde_json::Value = serde_json::from_str(&render_json(&result)).expect("json");
        assert_eq!(json["schema"], COMPARE_JSON_SCHEMA);
        assert_eq!(json["outcome"], "regression");
        assert_eq!(json["exit_code"], 1);
        assert_eq!(json["baseline_suite"], "s");
        assert_eq!(json["environment"]["mismatch"], serde_json::Value::Null);
        let row = &json["rows"][0];
        assert_eq!(row["benchmark_id"], "s/bench/work");
        assert_eq!(row["classification"], "regression");
        assert_eq!(row["gating"], true);
        assert!(row["change_percent"].as_f64().expect("change") < -40.0);
        assert!(row.get("source").is_some());
    }

    #[test]
    fn directories_resolve_to_latest_json() {
        let dir = std::env::temp_dir().join(format!("stress-compare-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        assert_eq!(resolve_artifact_path(&dir), dir.join("latest.json"));
        let file = dir.join("run.json");
        assert_eq!(resolve_artifact_path(&file), file);
        let _ = std::fs::remove_dir_all(dir);
    }
}
