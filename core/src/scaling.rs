//! Sweep grouping shared by the report's sweep tables and the
//! `scaling_anomaly` diagnostic.

use crate::artifact::{BenchmarkDiagnostic, BenchmarkSummary, DiagnosticSeverity, PrimaryMetric};
use std::collections::{BTreeMap, BTreeSet};

/// One row of a sweep: the parameter value, the row's primary value, and the
/// row's index in the summaries slice.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SweepPoint {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) index: usize,
}

/// One sweep: rows that differ only in `parameter`, sorted by its value.
#[derive(Debug, Clone)]
pub(crate) struct SweepGroup {
    pub(crate) parameter: String,
    pub(crate) key: SweepGroupKey,
    pub(crate) points: Vec<SweepPoint>,
}

/// Groups rows by every numeric parameter that takes more than one value,
/// exactly as the report's sweep tables do. Groups keep every size; callers
/// apply their own minimum.
pub(crate) fn sweep_groups(summaries: &[BenchmarkSummary]) -> Vec<SweepGroup> {
    let mut result = Vec::new();
    for parameter in numeric_parameter_keys(summaries) {
        let mut groups: BTreeMap<SweepGroupKey, Vec<SweepPoint>> = BTreeMap::new();
        for (index, summary) in summaries.iter().enumerate() {
            let Some(raw) = summary.parameters.get(&parameter) else {
                continue;
            };
            let Ok(x) = raw.parse::<f64>() else {
                continue;
            };
            let Some(y) = summary.primary_value() else {
                continue;
            };
            groups
                .entry(sweep_group_key(summary, &parameter, raw))
                .or_default()
                .push(SweepPoint { x, y, index });
        }
        for (key, mut points) in groups {
            points.sort_by(|left, right| left.x.total_cmp(&right.x));
            result.push(SweepGroup {
                parameter: parameter.clone(),
                key,
                points,
            });
        }
    }
    result
}

pub(crate) fn numeric_parameter_keys(summaries: &[BenchmarkSummary]) -> Vec<String> {
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

/// Identity of one sweep: the same benchmark (name with the swept value
/// removed, all other parameters equal) measured with the same metric and unit.
/// Speedup is only ever computed within one group.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SweepGroupKey {
    pub(crate) base_name: String,
    pub(crate) measurement: String,
    pub(crate) other_parameters: Vec<(String, String)>,
}

pub(crate) fn sweep_group_key(
    summary: &BenchmarkSummary,
    key: &str,
    raw_value: &str,
) -> SweepGroupKey {
    let base_name = name_without_swept_value(&summary.name, key, raw_value);
    SweepGroupKey {
        base_name,
        measurement: format!(
            "{:?} {}",
            summary.primary_metric,
            crate::reporting::human_measurement_label(summary)
        ),
        other_parameters: summary
            .parameters
            .iter()
            .filter(|(name, _)| name.as_str() != key)
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
    }
}

/// Replaces the swept value in a benchmark name with `*`, matching only a
/// delimiter-anchored token: `{key}={value}`, `{key}_{value}`,
/// `{key}-{value}`, or a bare `{value}` bounded by non-alphanumeric characters
/// (or the ends of the name). Key-qualified tokens win over bare values, and
/// the last match wins within each form. A name without such a token is kept
/// unchanged; grouping still also requires every other parameter to match.
pub(crate) fn name_without_swept_value(name: &str, key: &str, raw_value: &str) -> String {
    fn is_boundary(ch: Option<char>) -> bool {
        ch.is_none_or(|ch| !ch.is_ascii_alphanumeric())
    }
    fn last_bounded(name: &str, needle: &str) -> Option<usize> {
        if needle.is_empty() {
            return None;
        }
        name.match_indices(needle)
            .filter(|(index, _)| {
                is_boundary(name[..*index].chars().next_back())
                    && is_boundary(name[index + needle.len()..].chars().next())
            })
            .map(|(index, _)| index)
            .last()
    }

    if raw_value.is_empty() {
        return name.to_string();
    }
    for separator in ["=", "_", "-"] {
        let token = format!("{key}{separator}{raw_value}");
        if let Some(index) = last_bounded(name, &token) {
            let value_start = index + key.len() + separator.len();
            let mut base = name.to_string();
            base.replace_range(value_start..index + token.len(), "*");
            return base;
        }
    }
    if let Some(index) = last_bounded(name, raw_value) {
        let mut base = name.to_string();
        base.replace_range(index..index + raw_value.len(), "*");
        return base;
    }
    name.to_string()
}

/// Attaches the Info `scaling_anomaly` diagnostic to rows of sweeps whose
/// primary value measurably changes with the swept parameter.
///
/// The pass reuses the sweep-table grouping and evaluates groups with at
/// least [`MIN_SCALING_POINTS`] positive points. A group is reported when
/// two of its points differ beyond both their 95% confidence intervals and
/// [`MIN_RELATIVE_STEP`]; it is `non_monotonic` when such differences go
/// both up and down. For a `threads` sweep, counts above
/// `available_parallelism` are skipped (and listed), and throughput rows
/// carry the parallel efficiency `T(n) / (n / n0 * T(n0))`. The diagnostic is
/// Info and never changes measurements.
pub(crate) fn attach_scaling_diagnostics(
    summaries: &mut [BenchmarkSummary],
    available_parallelism: Option<usize>,
) {
    for group in sweep_groups(summaries) {
        let Some((diagnostic, evaluated)) =
            scaling_diagnostic(summaries, &group, available_parallelism)
        else {
            continue;
        };
        for index in evaluated {
            let summary = &mut summaries[index];
            let duplicate = summary.diagnostics.iter().any(|existing| {
                existing.code == "scaling_anomaly"
                    && existing.evidence.get("parameter") == Some(&group.parameter)
            });
            if !duplicate {
                summary.diagnostics.push(diagnostic.clone());
            }
        }
    }
}

const SCALING_ANOMALY: &str = "scaling_anomaly";
/// Fewest evaluated points for a fit.
const MIN_SCALING_POINTS: usize = 3;
/// Smallest relative difference between two points that counts as a change.
const MIN_RELATIVE_STEP: f64 = 0.05;
/// Diagnostics on member rows that explain scheduler-driven scaling.
const RELATED_CODES: [&str; 2] = ["flat_or_capped_throughput", "fixed_ops_throughput"];

#[allow(clippy::too_many_lines)]
fn scaling_diagnostic(
    summaries: &[BenchmarkSummary],
    group: &SweepGroup,
    available_parallelism: Option<usize>,
) -> Option<(BenchmarkDiagnostic, Vec<usize>)> {
    let threads = group.parameter == "threads";
    let limit = available_parallelism.filter(|_| threads);
    #[allow(clippy::cast_precision_loss)]
    let (evaluated, skipped): (Vec<SweepPoint>, Vec<SweepPoint>) = group
        .points
        .iter()
        .partition(|point| limit.is_none_or(|limit| point.x <= limit as f64));
    let evaluated = evaluated
        .into_iter()
        .filter(|point| {
            point.x > 0.0 && point.y > 0.0 && point.x.is_finite() && point.y.is_finite()
        })
        .collect::<Vec<_>>();
    if evaluated.len() < MIN_SCALING_POINTS {
        return None;
    }

    let intervals = evaluated
        .iter()
        .map(|point| primary_interval(&summaries[point.index], point.y))
        .collect::<Vec<_>>();
    let mut rises = false;
    let mut falls = false;
    for i in 0..evaluated.len() {
        for j in i + 1..evaluated.len() {
            let (earlier, later) = (evaluated[i].y, evaluated[j].y);
            let relative = (later - earlier).abs() / earlier;
            if relative <= MIN_RELATIVE_STEP {
                continue;
            }
            if intervals[j].0 > intervals[i].1 {
                rises = true;
            } else if intervals[j].1 < intervals[i].0 {
                falls = true;
            }
        }
    }
    if !rises && !falls {
        return None;
    }
    let pattern = match (rises, falls) {
        (true, true) => "non_monotonic",
        (true, false) => "monotonic_increasing",
        _ => "monotonic_decreasing",
    };
    let (exponent, r_squared) = log_log_fit(&evaluated);

    let format_x = |point: &SweepPoint| format!("{}", point.x);
    let mut evidence = BTreeMap::from([
        ("parameter".to_string(), group.parameter.clone()),
        ("group".to_string(), group.key.base_name.clone()),
        ("measurement".to_string(), group.key.measurement.clone()),
        ("points".to_string(), evaluated.len().to_string()),
        (
            "parameter_values".to_string(),
            evaluated
                .iter()
                .map(format_x)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        ("exponent".to_string(), format!("{exponent:.3}")),
        ("r_squared".to_string(), format!("{r_squared:.3}")),
        ("pattern".to_string(), pattern.to_string()),
    ]);
    if let Some(limit) = limit {
        evidence.insert("available_parallelism".to_string(), limit.to_string());
        if !skipped.is_empty() {
            evidence.insert(
                "skipped_above_parallelism".to_string(),
                skipped.iter().map(format_x).collect::<Vec<_>>().join(", "),
            );
        }
    }
    let first = &summaries[evaluated[0].index];
    if threads && first.primary_metric == PrimaryMetric::Throughput {
        let (base_x, base_y) = (evaluated[0].x, evaluated[0].y);
        let efficiency = evaluated
            .iter()
            .map(|point| {
                let value = (point.y / base_y) / (point.x / base_x);
                format!("{}={value:.2}", point.x)
            })
            .collect::<Vec<_>>()
            .join(", ");
        evidence.insert("thread_efficiency".to_string(), efficiency);
    }
    let related = group
        .points
        .iter()
        .flat_map(|point| {
            let summary = &summaries[point.index];
            summary.diagnostics.iter().filter_map(move |diagnostic| {
                let scheduler = diagnostic
                    .evidence
                    .get("scheduler_sensitive")
                    .is_some_and(|value| value == "true");
                let label = if RELATED_CODES.contains(&diagnostic.code.as_str()) {
                    diagnostic.code.clone()
                } else if scheduler {
                    "scheduler_sensitive".to_string()
                } else {
                    return None;
                };
                Some(format!("{label} ({}={})", group.parameter, point.x))
            })
        })
        .collect::<BTreeSet<_>>();
    if !related.is_empty() {
        evidence.insert(
            "related_diagnostics".to_string(),
            related.into_iter().collect::<Vec<_>>().join(", "),
        );
    }

    let reason = if pattern == "non_monotonic" {
        format!(
            "The primary value reverses direction across `{}` beyond confidence-interval noise.",
            group.parameter
        )
    } else {
        format!(
            "The primary value scales with `{}` as roughly {}^{exponent:.2}.",
            group.parameter, group.parameter
        )
    };
    let mut diagnostic =
        BenchmarkDiagnostic::new(SCALING_ANOMALY, DiagnosticSeverity::Info, reason);
    diagnostic.evidence = evidence;
    let fix = crate::diagnostics::catalog_fix(SCALING_ANOMALY);
    if !fix.is_empty() {
        diagnostic.suggestions.push(fix.to_string());
    }
    Some((
        diagnostic,
        evaluated.iter().map(|point| point.index).collect(),
    ))
}

/// The 95% interval that belongs to the row's primary value.
fn primary_interval(summary: &BenchmarkSummary, value: f64) -> (f64, f64) {
    let Some(stats) = summary.stats.as_ref() else {
        return (value, value);
    };
    #[allow(clippy::float_cmp)]
    let interval = if stats.mean == value {
        Some(stats.confidence_interval_95)
    } else {
        stats.p95_confidence_interval_95
    };
    interval
        .filter(|interval| interval.lower.is_finite() && interval.upper.is_finite())
        .map_or((value, value), |interval| (interval.lower, interval.upper))
}

/// Least-squares slope of `ln y` on `ln x`, and its coefficient of
/// determination.
#[allow(clippy::cast_precision_loss)]
fn log_log_fit(points: &[SweepPoint]) -> (f64, f64) {
    let n = points.len() as f64;
    let xs = points.iter().map(|point| point.x.ln()).collect::<Vec<_>>();
    let ys = points.iter().map(|point| point.y.ln()).collect::<Vec<_>>();
    let mean_x = xs.iter().sum::<f64>() / n;
    let mean_y = ys.iter().sum::<f64>() / n;
    let sxx = xs.iter().map(|x| (x - mean_x).powi(2)).sum::<f64>();
    let sxy = xs
        .iter()
        .zip(&ys)
        .map(|(x, y)| (x - mean_x) * (y - mean_y))
        .sum::<f64>();
    let syy = ys.iter().map(|y| (y - mean_y).powi(2)).sum::<f64>();
    if sxx == 0.0 {
        return (0.0, 0.0);
    }
    let slope = sxy / sxx;
    let r_squared = if syy == 0.0 {
        1.0
    } else {
        (sxy * sxy) / (sxx * syy)
    };
    (slope, r_squared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{
        BenchmarkBudgets, BenchmarkDiagnostic, CorrectnessSummary, DiagnosticSeverity,
        MeasurementIntent, PrimaryMetric, QualityClass, SummaryStats, TrustClass,
    };

    fn row(
        base: &str,
        key: &str,
        x: u64,
        metric: PrimaryMetric,
        value: f64,
        noise: f64,
    ) -> BenchmarkSummary {
        let values = (0..10)
            .map(|index| value * (1.0 + noise * (f64::from(index % 5) - 2.0) / 2.0))
            .collect::<Vec<_>>();
        let mut parameters = BTreeMap::new();
        parameters.insert(key.to_string(), x.to_string());
        BenchmarkSummary {
            benchmark_id: format!("{base}/{key}={x}"),
            name: format!("{base}/{key}={x}"),
            tier: 2,
            intent: MeasurementIntent::General,
            primary_metric: metric,
            measured_samples: values.len(),
            warmup_samples: 1,
            cooldown_samples: 0,
            stats: SummaryStats::from_values(&values),
            wall_clock: None,
            total_wall_clock_ns: 0,
            ns_per_op: None,
            gross_ns_per_op: None,
            overhead_ns_per_op: None,
            allocs_per_op: None,
            bytes_per_op: None,
            observations: Vec::new(),
            quality: QualityClass::Acceptable,
            trust_class: TrustClass::Gate,
            budgets: BenchmarkBudgets::default(),
            budget_results: Vec::new(),
            diagnostics: Vec::new(),
            correctness: CorrectnessSummary::new(true),
            parameters,
            metadata: BTreeMap::new(),
            source: None,
        }
    }

    fn sweep(
        key: &str,
        metric: PrimaryMetric,
        noise: f64,
        points: &[(u64, f64)],
    ) -> Vec<BenchmarkSummary> {
        points
            .iter()
            .map(|(x, y)| row("sort", key, *x, metric, *y, noise))
            .collect()
    }

    fn scaling(summary: &BenchmarkSummary) -> Option<&BenchmarkDiagnostic> {
        summary
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "scaling_anomaly")
    }

    fn exponent(summary: &BenchmarkSummary) -> f64 {
        scaling(summary).expect("scaling diagnostic").evidence["exponent"]
            .parse()
            .expect("numeric exponent")
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn linear_and_quadratic_sweeps_report_their_exponents() {
        let sizes = [10_u64, 100, 1_000, 10_000];
        let linear = sizes.map(|n| (n, 3.0 * n as f64));
        let quadratic = sizes.map(|n| (n, 0.5 * (n as f64).powi(2)));
        let mut rows = sweep("size", PrimaryMetric::NsPerOp, 0.01, &linear);
        attach_scaling_diagnostics(&mut rows, None);
        for summary in &rows {
            let diagnostic = scaling(summary).expect("every row of the sweep");
            assert_eq!(diagnostic.severity, DiagnosticSeverity::Info);
            assert_eq!(diagnostic.evidence["parameter"], "size");
            assert_eq!(diagnostic.evidence["pattern"], "monotonic_increasing");
            assert!((exponent(summary) - 1.0).abs() < 0.02);
            let r_squared: f64 = diagnostic.evidence["r_squared"].parse().unwrap();
            assert!(r_squared > 0.99);
        }
        let mut rows = sweep("size", PrimaryMetric::NsPerOp, 0.01, &quadratic);
        attach_scaling_diagnostics(&mut rows, None);
        assert!((exponent(&rows[0]) - 2.0).abs() < 0.02);
    }

    #[test]
    fn two_point_sweeps_are_not_evaluated() {
        let mut rows = sweep(
            "size",
            PrimaryMetric::NsPerOp,
            0.01,
            &[(10, 10.0), (100, 100.0)],
        );
        attach_scaling_diagnostics(&mut rows, None);
        assert!(rows.iter().all(|summary| scaling(summary).is_none()));
    }

    #[test]
    fn reversals_beyond_noise_are_flagged_non_monotonic() {
        let mut rows = sweep(
            "size",
            PrimaryMetric::NsPerOp,
            0.01,
            &[(1, 100.0), (2, 200.0), (4, 400.0), (8, 150.0)],
        );
        attach_scaling_diagnostics(&mut rows, None);
        let diagnostic = scaling(&rows[3]).expect("non-monotonic group");
        assert_eq!(diagnostic.evidence["pattern"], "non_monotonic");
        assert!(diagnostic.reason.contains("reverses"));
    }

    #[test]
    fn flat_noisy_sweeps_stay_silent() {
        let mut rows = sweep(
            "size",
            PrimaryMetric::Throughput,
            0.3,
            &[
                (1, 1_000.0),
                (2, 1_040.0),
                (4, 970.0),
                (8, 1_020.0),
                (16, 990.0),
            ],
        );
        attach_scaling_diagnostics(&mut rows, None);
        assert!(rows.iter().all(|summary| scaling(summary).is_none()));
    }

    #[test]
    fn small_steps_inside_the_noise_floor_stay_silent() {
        // Tight intervals, but the whole sweep moves by under 5%.
        let mut rows = sweep(
            "size",
            PrimaryMetric::NsPerOp,
            0.0,
            &[(1, 100.0), (2, 101.0), (4, 102.0), (8, 103.0)],
        );
        attach_scaling_diagnostics(&mut rows, None);
        assert!(rows.iter().all(|summary| scaling(summary).is_none()));
    }

    #[test]
    fn thread_sweeps_report_efficiency_and_skip_counts_above_parallelism() {
        let mut rows = sweep(
            "threads",
            PrimaryMetric::Throughput,
            0.01,
            &[
                (1, 1_000.0),
                (2, 1_900.0),
                (4, 3_200.0),
                (8, 4_000.0),
                (16, 4_100.0),
            ],
        );
        rows[1].diagnostics.push(BenchmarkDiagnostic::new(
            "flat_or_capped_throughput",
            DiagnosticSeverity::Warning,
            "flat",
        ));
        attach_scaling_diagnostics(&mut rows, Some(8));
        let diagnostic = scaling(&rows[0]).expect("thread sweep");
        assert_eq!(
            diagnostic.evidence["thread_efficiency"],
            "1=1.00, 2=0.95, 4=0.80, 8=0.50"
        );
        assert_eq!(diagnostic.evidence["available_parallelism"], "8");
        assert_eq!(diagnostic.evidence["skipped_above_parallelism"], "16");
        assert_eq!(diagnostic.evidence["points"], "4");
        assert!(diagnostic.evidence["related_diagnostics"].contains("flat_or_capped_throughput"));
        assert!(
            scaling(&rows[4]).is_none(),
            "skipped rows are listed in evidence, not annotated"
        );
    }

    #[test]
    fn thread_efficiency_is_only_reported_for_throughput() {
        let mut rows = sweep(
            "threads",
            PrimaryMetric::NsPerOp,
            0.01,
            &[(1, 100.0), (2, 200.0), (4, 400.0)],
        );
        attach_scaling_diagnostics(&mut rows, Some(8));
        let diagnostic = scaling(&rows[0]).expect("thread sweep");
        assert!(!diagnostic.evidence.contains_key("thread_efficiency"));
    }

    #[test]
    fn repeated_attachment_does_not_duplicate() {
        let mut rows = sweep(
            "size",
            PrimaryMetric::NsPerOp,
            0.01,
            &[(1, 1.0), (2, 2.0), (4, 4.0)],
        );
        attach_scaling_diagnostics(&mut rows, None);
        attach_scaling_diagnostics(&mut rows, None);
        assert_eq!(
            rows[0]
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == "scaling_anomaly")
                .count(),
            1
        );
    }
}
