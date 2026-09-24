//! RFC 4180 CSV rendering of a run's benchmark summaries.
//!
//! One row per benchmark summary. Text cells are quoted when they contain a
//! comma, quote, or line break, and cells that a spreadsheet would evaluate
//! as a formula (leading `=`, `+`, `-`, `@`, tab, or carriage return) are
//! prefixed with `'`. Numeric cells are written by this crate and never
//! guarded, so negative changes stay numeric.
//!
//! Parameters are flattened into a single `parameters` column as
//! `key=value` pairs sorted by key and joined by `;`. Within keys and
//! values, `\`, `;`, and `=` are escaped with a backslash.

use crate::artifact::{
    BenchmarkSummary, ComparisonResult, ConfidenceInterval, PrimaryMetric, StressRun,
};
use std::fmt::Write as _;

/// Column header of [`format_csv_report`].
pub const CSV_HEADER: [&str; 17] = [
    "suite",
    "benchmark_id",
    "name",
    "parameters",
    "primary_metric",
    "value",
    "unit",
    "mean",
    "p50",
    "p95",
    "ci_lower",
    "ci_upper",
    "samples",
    "quality",
    "trust",
    "regression_class",
    "change_percent",
];

/// Escape one text cell for CSV (RFC 4180 quoting plus a formula-injection
/// guard).
#[must_use]
pub fn csv_text_cell(value: &str) -> String {
    let guarded = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value.to_string()
    };
    if guarded.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", guarded.replace('"', "\"\""))
    } else {
        guarded
    }
}

/// Format a number cell; empty when absent or non-finite.
#[must_use]
pub fn csv_number_cell(value: Option<f64>) -> String {
    value
        .filter(|value| value.is_finite())
        .map(|value| value.to_string())
        .unwrap_or_default()
}

/// Join cells into one CRLF-terminated CSV record. Cells must already be
/// escaped.
#[must_use]
pub fn csv_record(cells: &[String]) -> String {
    let mut line = cells.join(",");
    line.push_str("\r\n");
    line
}

fn escape_parameter_part(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace('=', "\\=")
}

fn flatten_parameters(summary: &BenchmarkSummary) -> String {
    summary
        .parameters
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                escape_parameter_part(key),
                escape_parameter_part(value)
            )
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn serde_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToString::to_string))
        .unwrap_or_default()
}

/// Unit of the primary value, e.g. `op/s` or `ns/op`.
fn primary_unit(summary: &BenchmarkSummary) -> String {
    let unit = crate::reporting::display_unit(summary);
    match summary.primary_metric {
        PrimaryMetric::Throughput => format!("{unit}/s"),
        PrimaryMetric::LatencyP95 | PrimaryMetric::NsPerOp => format!("ns/{unit}"),
    }
}

/// Confidence interval matching the primary value.
fn primary_interval(summary: &BenchmarkSummary) -> Option<ConfidenceInterval> {
    let stats = summary.stats.as_ref()?;
    let value = summary.primary_value()?;
    if summary.primary_metric == PrimaryMetric::LatencyP95 && value.to_bits() == stats.p95.to_bits()
    {
        if let Some(interval) = &stats.p95_confidence_interval_95 {
            return Some(*interval);
        }
    }
    Some(stats.confidence_interval_95)
}

fn summary_row(run: &StressRun, summary: &BenchmarkSummary) -> String {
    let stats = summary.stats.as_ref();
    let interval = primary_interval(summary);
    let comparison: Option<&ComparisonResult> = run
        .comparisons
        .iter()
        .find(|comparison| comparison.benchmark_id == summary.benchmark_id);
    csv_record(&[
        csv_text_cell(&run.suite),
        csv_text_cell(&summary.benchmark_id),
        csv_text_cell(&summary.name),
        csv_text_cell(&flatten_parameters(summary)),
        csv_text_cell(&serde_name(&summary.primary_metric)),
        csv_number_cell(summary.primary_value()),
        csv_text_cell(&primary_unit(summary)),
        csv_number_cell(stats.map(|stats| stats.mean)),
        csv_number_cell(stats.map(|stats| stats.p50)),
        csv_number_cell(stats.map(|stats| stats.p95)),
        csv_number_cell(interval.as_ref().map(|interval| interval.lower)),
        csv_number_cell(interval.as_ref().map(|interval| interval.upper)),
        summary.measured_samples.to_string(),
        csv_text_cell(&serde_name(&summary.quality)),
        csv_text_cell(&serde_name(&summary.trust_class)),
        comparison
            .map(|comparison| csv_text_cell(&serde_name(&comparison.classification)))
            .unwrap_or_default(),
        csv_number_cell(comparison.and_then(|comparison| comparison.change_percent)),
    ])
}

/// Render the run as CSV: a header plus one row per benchmark summary.
#[must_use]
pub fn format_csv_report(run: &StressRun) -> String {
    let mut output = csv_record(&CSV_HEADER.map(ToString::to_string));
    for summary in &run.summaries {
        let _ = write!(output, "{}", summary_row(run, summary));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_cells_are_unquoted() {
        assert_eq!(csv_text_cell("queue::fast"), "queue::fast");
        assert_eq!(csv_text_cell(""), "");
    }

    #[test]
    fn rfc4180_quotes_commas_quotes_and_newlines() {
        assert_eq!(csv_text_cell("a,b"), "\"a,b\"");
        assert_eq!(csv_text_cell("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_text_cell("line\nbreak"), "\"line\nbreak\"");
        assert_eq!(csv_text_cell("cr\rhere"), "\"cr\rhere\"");
    }

    #[test]
    fn formula_prefixes_are_neutralized() {
        for (input, expected) in [
            ("=SUM(A1)", "'=SUM(A1)"),
            ("+1", "'+1"),
            ("-1", "'-1"),
            ("@cmd", "'@cmd"),
            ("\tx", "'\tx"),
            ("=a,b", "\"'=a,b\""),
        ] {
            assert_eq!(csv_text_cell(input), expected, "{input:?}");
        }
        assert_eq!(csv_text_cell("a=b"), "a=b");
    }

    #[test]
    fn numbers_are_plain_and_non_finite_is_empty() {
        assert_eq!(csv_number_cell(Some(-3.5)), "-3.5");
        assert_eq!(csv_number_cell(Some(f64::NAN)), "");
        assert_eq!(csv_number_cell(None), "");
    }

    #[test]
    fn parameters_are_sorted_and_escaped() {
        let mut summary = BenchmarkSummary::new("b", "b", 2, PrimaryMetric::Throughput);
        summary.parameters.insert("z".into(), "1".into());
        summary.parameters.insert("a;k".into(), "x=y\\z".into());
        assert_eq!(flatten_parameters(&summary), "a\\;k=x\\=y\\\\z;z=1");
    }
}
