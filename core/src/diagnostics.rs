//! Catalog of stable diagnostic codes.
//!
//! Diagnostic codes stay plain strings on the wire. The catalog is a const
//! table rather than an enum so new codes can land in minor releases without
//! breaking exhaustive matches downstream.

use crate::artifact::DiagnosticSeverity;

/// Static description of one stable diagnostic code.
///
/// Fields may be added in minor releases; read entries through
/// [`DIAGNOSTIC_CATALOG`] or [`diagnostic_info`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DiagnosticInfo {
    /// Stable diagnostic code as written to artifacts.
    pub code: &'static str,
    /// Severity the code is usually emitted with. Some codes escalate.
    pub default_severity: DiagnosticSeverity,
    /// One-line summary of what the diagnostic means.
    pub summary: &'static str,
    /// Common causes.
    pub causes: &'static str,
    /// Canonical fix text, used for diagnostic suggestions and console fixes.
    pub fix: &'static str,
    /// Documentation anchor, relative to the repository `docs/` directory.
    pub docs_anchor: &'static str,
}

/// Every diagnostic code cntryl-stress can emit, sorted by code.
pub const DIAGNOSTIC_CATALOG: &[DiagnosticInfo] = &[
    DiagnosticInfo {
        code: "async_misuse",
        default_severity: DiagnosticSeverity::Info,
        summary: "Async measurement showed no observable scheduling or await overhead.",
        causes: "The measured future completes synchronously, or spawns detached work instead of awaiting it.",
        fix: "Make sure the measured future awaits the real async operation instead of spawning detached work.",
        docs_anchor: "diagnostics/async_misuse.md",
    },
    DiagnosticInfo {
        code: "baseline_semantics_changed",
        default_severity: DiagnosticSeverity::Warning,
        summary: "The row could not be compared because its semantics changed relative to the baseline.",
        causes: "Tier, mode, logical unit, parameters, or measurement intent differ from the baseline row.",
        fix: "Refresh the baseline after confirming the semantic change is intentional.",
        docs_anchor: "diagnostics/baseline_semantics_changed.md",
    },
    DiagnosticInfo {
        code: "batch_unit_ambiguous",
        default_severity: DiagnosticSeverity::Warning,
        summary: "Batched work is missing explicit logical-unit normalization metadata.",
        causes: "measure_batch is used without logical_unit or *_per_logical_operation parameters.",
        fix: "Add logical_unit and any *_per_logical_operation parameter so the report can state the measured question directly.",
        docs_anchor: "diagnostics/batch_unit_ambiguous.md",
    },
    DiagnosticInfo {
        code: "budget_failure",
        default_severity: DiagnosticSeverity::Error,
        summary: "An explicit benchmark budget failed or could not be evaluated.",
        causes: "The measured cost exceeds a max_* budget, or an allocation budget is set without the stress allocator installed.",
        fix: "Inspect the failing budget, then either reduce measured cost or intentionally update the budget.",
        docs_anchor: "diagnostics/budget_failure.md",
    },
    DiagnosticInfo {
        code: "correctness_failure",
        default_severity: DiagnosticSeverity::Error,
        summary: "Correctness counters did not pass for this benchmark row.",
        causes: "Failures, timeouts, duplicates, dropped results, validation errors, or attempted/completed mismatches were recorded.",
        fix: "Inspect correctness counters before using this performance number.",
        docs_anchor: "diagnostics/correctness_failure.md",
    },
    DiagnosticInfo {
        code: "fixed_ops_throughput",
        default_severity: DiagnosticSeverity::Warning,
        summary: "A throughput row uses fixed-op timing instead of a fixed-duration window.",
        causes: "A throughput-tier row runs a fixed operation count per sample.",
        fix: "Use duration-based throughput for main rows, or split the fixed-op probe into an explicit diagnostic row.",
        docs_anchor: "diagnostics/fixed_ops_throughput.md",
    },
    DiagnosticInfo {
        code: "flat_or_capped_throughput",
        default_severity: DiagnosticSeverity::Warning,
        summary: "Throughput is near-perfectly flat while completed work is effectively fixed.",
        causes: "The workload hits a capacity cap, a rate limiter, or a fixed amount of work per window.",
        fix: "Confirm whether this row is an intentional capped-capacity probe; otherwise inspect local bottlenecks or move it out of the gate set.",
        docs_anchor: "diagnostics/flat_or_capped_throughput.md",
    },
    DiagnosticInfo {
        code: "high_allocations",
        default_severity: DiagnosticSeverity::Warning,
        summary: "The benchmark allocated during measured work.",
        causes: "Buffers, collections, or strings are allocated inside the measured closure.",
        fix: "Move reusable allocations into setup or make the allocation budget explicit.",
        docs_anchor: "diagnostics/high_allocations.md",
    },
    DiagnosticInfo {
        code: "high_variance",
        default_severity: DiagnosticSeverity::Warning,
        summary: "Measured samples varied by more than 10% relative standard deviation.",
        causes: "Short windows, one-off setup, cache or I/O effects, background load, or scheduler contention.",
        fix: "Use deterministic fixtures and move setup outside the measured work.",
        docs_anchor: "diagnostics/high_variance.md",
    },
    DiagnosticInfo {
        code: "insufficient_warmup",
        default_severity: DiagnosticSeverity::Info,
        summary: "The warmup tail and the first measured samples sit at different levels.",
        causes: "Caches, JIT-like lazy initialization, allocator growth, or CPU frequency ramp are still settling when measurement starts.",
        fix: "Increase warmup samples until the first measured samples match the steady state.",
        docs_anchor: "diagnostics/insufficient_warmup.md",
    },
    DiagnosticInfo {
        code: "invalid_timing",
        default_severity: DiagnosticSeverity::Error,
        summary: "At least one measured sample recorded zero or invalid timing.",
        causes: "The measured closure did no work, or timing was recorded outside a measurement.",
        fix: "Measure exactly one non-empty workload for this row.",
        docs_anchor: "diagnostics/invalid_timing.md",
    },
    DiagnosticInfo {
        code: "likely_optimized_away",
        default_severity: DiagnosticSeverity::Warning,
        summary: "Tier 1 timing is below 5 ns/op without explicit validation.",
        causes: "The compiler removed the measured work because its inputs are constant or its output is unused.",
        fix: "Vary inputs, accumulate observable outputs, and use #[stress(metadata(validated_micro = \"true\"))] only after anti-DCE is explicit.",
        docs_anchor: "diagnostics/likely_optimized_away.md",
    },
    DiagnosticInfo {
        code: "measurement_drift",
        default_severity: DiagnosticSeverity::Info,
        summary: "Measured samples trend steadily in one direction over the run.",
        causes: "State accumulates across samples (growing collections, fragmentation, leaks), or thermal throttling and background load change during the run.",
        fix: "Reset per-sample state in setup, or investigate thermal and background load before trusting the row.",
        docs_anchor: "diagnostics/measurement_drift.md",
    },
    DiagnosticInfo {
        code: "measurement_mode_mismatch",
        default_severity: DiagnosticSeverity::Warning,
        summary: "Sibling rows in the same workload family mix throughput measurement semantics.",
        causes: "Some rows in a family use fixed-duration windows while others use fixed operation counts.",
        fix: "Use one measurement_mode per workload family, or split fixed-op probes into explicit diagnostic rows.",
        docs_anchor: "diagnostics/measurement_mode_mismatch.md",
    },
    DiagnosticInfo {
        code: "non_finite_samples_dropped",
        default_severity: DiagnosticSeverity::Warning,
        summary: "Non-finite metric values were dropped before computing statistics.",
        causes: "Zero-length timings or overflowing counters produced infinite or NaN metric values.",
        fix: "Measure non-empty work in every sample so each metric value is finite.",
        docs_anchor: "diagnostics/non_finite_samples_dropped.md",
    },
    DiagnosticInfo {
        code: "peak_rss_exceeded",
        default_severity: DiagnosticSeverity::Warning,
        summary: "Process peak RSS exceeded the row's max_peak_rss_mb budget.",
        causes: "This row, or an earlier row in the same process, grew the resident set past the budget; peak RSS is process-wide and monotonic.",
        fix: "Reduce peak memory in the benchmark, run memory-heavy rows in their own suite, or raise max_peak_rss_mb.",
        docs_anchor: "diagnostics/peak_rss_exceeded.md",
    },
    DiagnosticInfo {
        code: "regression",
        default_severity: DiagnosticSeverity::Error,
        summary: "The row regressed against the selected baseline.",
        causes: "The measured cost moved past the regression threshold with non-overlapping confidence intervals.",
        fix: "Inspect the same benchmark row before updating the baseline.",
        docs_anchor: "diagnostics/regression.md",
    },
    DiagnosticInfo {
        code: "setup_dominates_measurement",
        default_severity: DiagnosticSeverity::Error,
        summary: "Timing overhead or setup dominates the measured work.",
        causes: "Setup runs inside the measured closure, or each iteration does too little work.",
        fix: "Increase measured work per iteration and keep setup outside the measurement closure.",
        docs_anchor: "diagnostics/setup_dominates_measurement.md",
    },
    DiagnosticInfo {
        code: "single_op_throughput",
        default_severity: DiagnosticSeverity::Warning,
        summary: "A throughput-tier row completed only one operation per sample.",
        causes: "ctx.measure is used for throughput work instead of measure_batch or record_external.",
        fix: "Use measure_batch or record_external for throughput work, or move a single-operation row to Tier 2.",
        docs_anchor: "diagnostics/single_op_throughput.md",
    },
    DiagnosticInfo {
        code: "tiny_micro_timing",
        default_severity: DiagnosticSeverity::Warning,
        summary: "Tier 1 timing is below 15 ns/op without explicit validation.",
        causes: "The measured operation is close to timer resolution and loop overhead.",
        fix: "Batch more logical work per sample, or declare role = \"diagnostic\" after validating the microbenchmark shape.",
        docs_anchor: "diagnostics/tiny_micro_timing.md",
    },
    DiagnosticInfo {
        code: "too_fast",
        default_severity: DiagnosticSeverity::Warning,
        summary: "The measured work is too small for a useful timing sample.",
        causes: "Each sample finishes within a few timer ticks, so timer resolution dominates.",
        fix: "Batch more logical work per measurement or use Tier 1 for hot-path micro timing.",
        docs_anchor: "diagnostics/too_fast.md",
    },
    DiagnosticInfo {
        code: "too_few_samples",
        default_severity: DiagnosticSeverity::Warning,
        summary: "The row has too few measured samples to make a stable decision.",
        causes: "The profile or --samples override collects fewer than five measured samples.",
        fix: "Collect at least five measured samples, or use the release profile for gate-quality rows.",
        docs_anchor: "diagnostics/too_few_samples.md",
    },
    DiagnosticInfo {
        code: "zero_completed_ops",
        default_severity: DiagnosticSeverity::Error,
        summary: "At least one measured sample completed zero logical operations.",
        causes: "The workload returned early, or completed work was not recorded.",
        fix: "Record completed logical work with measure_batch, operations, or record_external.",
        docs_anchor: "diagnostics/zero_completed_ops.md",
    },
];

/// Look up the catalog entry for `code`.
#[must_use]
pub fn diagnostic_info(code: &str) -> Option<&'static DiagnosticInfo> {
    DIAGNOSTIC_CATALOG
        .binary_search_by(|info| info.code.cmp(code))
        .ok()
        .map(|index| &DIAGNOSTIC_CATALOG[index])
}

/// Canonical fix text for a cataloged code, or an empty string.
pub(crate) fn catalog_fix(code: &str) -> &'static str {
    diagnostic_info(code).map_or("", |info| info.fix)
}

/// Return up to three catalog codes closest to `code` by edit distance.
#[must_use]
pub fn nearest_diagnostic_codes(code: &str) -> Vec<&'static str> {
    let code = code.trim().to_ascii_lowercase();
    let limit = (code.chars().count() / 3).max(2);
    let mut scored = DIAGNOSTIC_CATALOG
        .iter()
        .filter_map(|info| {
            let distance = edit_distance(&code, info.code);
            let related =
                !code.is_empty() && (info.code.contains(code.as_str()) || code.contains(info.code));
            (distance <= limit || related).then_some((distance, info.code))
        })
        .collect::<Vec<_>>();
    scored.sort_unstable();
    scored.into_iter().take(3).map(|(_, code)| code).collect()
}

/// Validate a diagnostic code, returning an error that lists near matches.
///
/// # Errors
///
/// Returns an error when `code` is not in [`DIAGNOSTIC_CATALOG`].
pub fn validate_diagnostic_code(code: &str) -> Result<&'static DiagnosticInfo, String> {
    diagnostic_info(code).ok_or_else(|| unknown_code_message(code))
}

fn unknown_code_message(code: &str) -> String {
    let nearest = nearest_diagnostic_codes(code);
    let mut message = format!("unknown diagnostic code '{code}'");
    if nearest.is_empty() {
        message.push_str("; run `cargo stress explain --list` to see every code");
    } else {
        let quoted = nearest
            .iter()
            .map(|code| format!("'{code}'"))
            .collect::<Vec<_>>()
            .join(", ");
        message.push_str("; did you mean ");
        message.push_str(&quoted);
        message.push('?');
    }
    message
}

/// Parse a comma-separated list of diagnostic codes, validating each one.
///
/// # Errors
///
/// Returns an error naming the first unknown code and its near matches.
pub fn parse_diagnostic_codes(value: &str) -> Result<Vec<String>, String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .map(|code| validate_diagnostic_code(code).map(|info| info.code.to_string()))
        .collect()
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    for (i, left_char) in left.chars().enumerate() {
        let mut current = vec![i + 1; right.len() + 1];
        for (j, right_char) in right.iter().enumerate() {
            let substitution = previous[j] + usize::from(left_char != *right_char);
            current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        previous = current;
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_codes_are_unique_sorted_and_documented() {
        assert!(!DIAGNOSTIC_CATALOG.is_empty());
        for pair in DIAGNOSTIC_CATALOG.windows(2) {
            assert!(pair[0].code < pair[1].code, "{} out of order", pair[1].code);
        }
        for info in DIAGNOSTIC_CATALOG {
            assert!(
                info.code
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
                "{}",
                info.code
            );
            assert!(!info.summary.is_empty(), "{}", info.code);
            assert!(!info.causes.is_empty(), "{}", info.code);
            assert!(!info.fix.is_empty(), "{}", info.code);
            assert_eq!(info.docs_anchor, format!("diagnostics/{}.md", info.code));
        }
    }

    #[test]
    fn lookup_finds_known_codes_only() {
        assert_eq!(
            diagnostic_info("too_fast").map(|info| info.code),
            Some("too_fast")
        );
        assert!(diagnostic_info("not_a_code").is_none());
    }

    #[test]
    fn typos_suggest_nearest_codes() {
        assert_eq!(
            nearest_diagnostic_codes("to_fast").first(),
            Some(&"too_fast")
        );
        assert_eq!(
            nearest_diagnostic_codes("high_varience").first(),
            Some(&"high_variance")
        );
        assert!(nearest_diagnostic_codes("zzzzzzzzzzzzzzzzzzzzzz").is_empty());
        let error = validate_diagnostic_code("to_fast").expect_err("unknown code");
        assert!(
            error.contains("unknown diagnostic code 'to_fast'"),
            "{error}"
        );
        assert!(error.contains("did you mean 'too_fast'"), "{error}");
    }

    #[test]
    fn code_lists_split_on_commas_and_trim() {
        assert_eq!(
            parse_diagnostic_codes(" too_fast, high_variance ,,").expect("valid"),
            vec!["too_fast".to_string(), "high_variance".to_string()]
        );
        assert!(parse_diagnostic_codes("too_fast,nope")
            .expect_err("unknown")
            .contains("'nope'"));
    }

    fn production_source(source: &'static str) -> &'static str {
        source
            .find("#[cfg(test)]\nmod tests")
            .map_or(source, |end| &source[..end])
    }

    fn literal_after(rest: &str) -> Option<&str> {
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('"')?;
        let end = rest.find('"')?;
        Some(&rest[..end])
    }

    fn emitted_codes() -> Vec<String> {
        let sources = [
            production_source(include_str!("artifact.rs")),
            production_source(include_str!("reporting.rs")),
            production_source(include_str!("runner.rs")),
            production_source(include_str!("context.rs")),
            production_source(include_str!("harness.rs")),
            production_source(include_str!("config.rs")),
        ];
        let patterns = [
            "diagnostic(",
            "diagnostic_with_evidence(",
            "catalog_diagnostic(",
            "catalog_fix(",
            "code: \"",
            "code == \"",
            "has_diagnostic(summary,",
            "diagnostic_attention(",
        ];
        let mut codes = Vec::new();
        for source in sources {
            for pattern in patterns {
                for (index, _) in source.match_indices(pattern) {
                    let after = &source[index + pattern.len()..];
                    let after = if pattern.ends_with('"') {
                        &source[index + pattern.len() - 1..]
                    } else {
                        after
                    };
                    if let Some(code) = literal_after(after) {
                        codes.push(code.to_string());
                    }
                }
            }
        }
        codes.sort();
        codes.dedup();
        codes
    }

    #[test]
    fn every_emitted_code_is_in_the_catalog() {
        let codes = emitted_codes();
        assert!(codes.len() >= 15, "scan found too few codes: {codes:?}");
        for code in &codes {
            assert!(
                diagnostic_info(code).is_some(),
                "diagnostic code {code:?} is emitted but missing from DIAGNOSTIC_CATALOG"
            );
        }
    }
}
