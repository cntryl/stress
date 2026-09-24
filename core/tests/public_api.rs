#![allow(dead_code)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::too_many_lines)]

use cntryl_stress::{
    black_box, stress, stress_allocator, LogicalUnit, OperationOutcome, RunProfile, StressContext,
    StressError, StressResult, StressRunner, StressRunnerConfig, StressRunnerOptions,
};
use std::time::Duration;

stress_allocator!();

#[stress(tier = 1, max_allocs_per_op = 0, metadata(component = "api"))]
fn macro_expansion_uses_namespaced_artifact_types(ctx: &mut StressContext) {
    ctx.measure("hot path", || black_box(1_u64));
}

#[stress(tier = 2, max_peak_rss_mb = 4096)]
fn peak_rss_budget_attribute_compiles(ctx: &mut StressContext) {
    ctx.measure("rss", || black_box(1_u64));
}

#[stress(tier = 2, metadata(component = "api"))]
fn fallible_macro_benchmark(ctx: &mut StressContext) -> StressResult {
    let value = "42"
        .parse::<u64>()
        .map_err(|error| StressError::new(error.to_string()))?;
    ctx.measure_outcome("fallible work", LogicalUnit::new("item"), || {
        black_box(value);
        OperationOutcome::success(1)
    });
    Ok(())
}

#[stress(tier = 2, metadata(component = "api"))]
fn explicit_fallible_measurement_apis_compile(ctx: &mut StressContext) -> StressResult {
    let _: u64 = ctx.measure_result("result", || Ok::<_, StressError>(black_box(1_u64)))?;
    let _: u64 = ctx.measure_result_with_setup(
        "result setup",
        || vec![1_u64],
        |input| Ok::<_, StressError>(black_box(input[0])),
    )?;
    ctx.measure_outcome_with_setup(
        "outcome setup",
        LogicalUnit::new("item"),
        || vec![1_u64],
        |input| {
            black_box(input);
            OperationOutcome::success(1)
        },
    );
    ctx.benchmark("builder outcome setup")
        .operations_per_sample(2)
        .measure_outcome_with_setup(
            LogicalUnit::new("item"),
            || vec![1_u64],
            |input| {
                black_box(input);
                OperationOutcome::success(1)
            },
        );
    let _: u64 = ctx
        .benchmark("builder result")
        .operations_per_sample(2)
        .measure_result(|| Ok::<_, StressError>(black_box(1_u64)))?;
    let _: u64 = ctx
        .benchmark("builder result setup")
        .operations_per_sample(2)
        .measure_result_with_setup(
            || vec![1_u64],
            |input| Ok::<_, StressError>(black_box(input[0])),
        )?;
    Ok(())
}

#[stress(tier = 2, metadata(component = "api"))]
async fn explicit_fallible_async_measurement_apis_compile(ctx: &mut StressContext) -> StressResult {
    let _: u64 = ctx
        .measure_result_async("async result", || async {
            Ok::<_, StressError>(black_box(1_u64))
        })
        .await?;
    let _: u64 = ctx
        .measure_result_async_with_setup(
            "async result setup",
            || vec![1_u64],
            |input| async move { Ok::<_, StressError>(black_box(input[0])) },
        )
        .await?;
    let _: u64 = ctx
        .benchmark("builder async result")
        .operations_per_sample(2)
        .measure_result_async(|| async { Ok::<_, StressError>(black_box(1_u64)) })
        .await?;
    let _: u64 = ctx
        .benchmark("builder async result setup")
        .operations_per_sample(2)
        .measure_result_async_with_setup(
            || vec![1_u64],
            |input| async move { Ok::<_, StressError>(black_box(input[0])) },
        )
        .await?;
    Ok(())
}

mod benchmark_binary {
    use cntryl_stress::stress_main;

    stress_main!();
}

#[test]
fn root_common_authoring_imports_compile() {
    let profile = RunProfile::Smoke;
    let config = StressRunnerConfig::for_profile(profile);
    let mut runner = StressRunner::with_config("public-api", config);
    let _options = StressRunnerOptions::new()
        .profile(profile)
        .warmup_samples(1)
        .cooldown_samples(1)
        .threshold_percent(5.0)
        .output_dir("target/stress-public-api");

    runner.reporters(Vec::new());
    runner.run("bench", |ctx| {
        ctx.measure_with_setup("work", || 1_u64, black_box);
    });
    let run = runner.finish();

    assert_eq!(run.run_profile, RunProfile::Smoke);
}

#[test]
fn advanced_imports_compile_from_modules() {
    use cntryl_stress::artifact::{
        BenchmarkBudgets, BenchmarkSummary, Sample, SamplePhase, StressRun, SCHEMA_VERSION,
    };
    use cntryl_stress::reporting::{format_console_run, ConsoleReporter, Reporter};
    use cntryl_stress::runner::{evaluate_run_gate, RunGate};

    let run = current_schema_run();
    let _budgets = BenchmarkBudgets::default();
    let _summary_count = run
        .summaries
        .iter()
        .filter(|summary: &&BenchmarkSummary| summary.measured_samples == 1)
        .count();
    let _sample_count = run
        .samples
        .iter()
        .filter(|sample: &&Sample| sample.phase == SamplePhase::Measured)
        .count();
    let _reporter: Box<dyn Reporter> = Box::new(ConsoleReporter::new());
    let console = format_console_run(&run);

    assert!(console.contains("@cntryl/stress"));
    assert_eq!(evaluate_run_gate(&run), RunGate::Passed);

    let json = serde_json::to_string(&run).expect("serialize current run");
    let parsed = StressRun::from_json_str(&json).expect("current schema parses");
    assert_eq!(parsed.schema_version, SCHEMA_VERSION);

    let wrong_schema = json.replace(SCHEMA_VERSION, "cntryl-stress.v999");
    assert!(StressRun::from_json_str(&wrong_schema).is_err());
}

fn current_schema_run() -> cntryl_stress::artifact::StressRun {
    use cntryl_stress::artifact::{
        BenchmarkMode, BenchmarkSpec, BenchmarkSummary, EnvironmentInfo, PrimaryMetric,
        ProfileConfig, QualityClass, RunProfile, Sample, SamplePhase, StressRun, SummaryStats,
        TrustClass,
    };

    let mut profile_config = ProfileConfig::new();
    profile_config.profile = RunProfile::Smoke;
    profile_config.measured_samples = 1;
    profile_config.warmup_samples = 0;
    profile_config.min_quality = QualityClass::Untrustworthy;
    profile_config.sample_duration = Duration::from_millis(10);
    profile_config.micro_sample_duration = Duration::from_millis(5);
    profile_config.report_depth = "summary".to_string();
    let environment = EnvironmentInfo::unknown(profile_config);

    let mut run = StressRun::new("suite", RunProfile::Smoke, environment.clone());
    run.started_at = "1".to_string();
    run.total_elapsed_ns = 1;
    run.benchmark_specs.push(BenchmarkSpec::new(
        "suite/bench",
        "bench",
        2,
        BenchmarkMode::FixedOperations {
            operations_per_sample: 1,
        },
    ));

    let mut sample = Sample::new("suite/bench", SamplePhase::Measured, environment);
    sample.elapsed_ns = 1;
    sample.wall_clock_ns = 1;
    sample.operations_attempted = 1;
    sample.operations_completed = 1;
    sample.throughput = 1.0;
    sample.counters.attempted = 1;
    sample.counters.completed = 1;
    run.samples.push(sample);

    let mut summary = BenchmarkSummary::new("suite/bench", "bench", 2, PrimaryMetric::Throughput);
    summary.measured_samples = 1;
    summary.stats = SummaryStats::from_values(&[1.0]);
    summary.total_wall_clock_ns = 1;
    summary.quality = QualityClass::Untrustworthy;
    summary.trust_class = TrustClass::Gate;
    summary.correctness.counters.attempted = 1;
    summary.correctness.counters.completed = 1;
    run.summaries.push(summary);
    run
}

#[test]
fn artifact_types_build_via_constructors_only() {
    use cntryl_stress::artifact::{
        BenchmarkDiagnostic, BenchmarkSummary, ComparisonClass, ComparisonResult,
        DiagnosticSeverity, DiagnosticSummary, EnvironmentInfo, MeasurementIntent, PrimaryMetric,
        ProfileConfig, QualityClass, Sample, SamplePhase, StressRun, SCHEMA_VERSION,
    };

    let profile_config = ProfileConfig::new();
    assert_eq!(profile_config, ProfileConfig::default());

    let environment = EnvironmentInfo::unknown(profile_config.clone());
    assert_eq!(environment.cpu_model, "unknown");
    assert_eq!(environment.profile_config, profile_config);

    let sample = Sample::new("s/b", SamplePhase::Warmup, environment.clone());
    assert_eq!(sample.benchmark_id, "s/b");
    assert_eq!(sample.phase, SamplePhase::Warmup);
    assert_eq!(sample.intent, MeasurementIntent::General);
    assert_eq!(sample.elapsed_ns, 0);
    assert!(sample.latency_ns.is_empty());
    assert!(sample.parameters.is_empty());
    assert_eq!(sample.environment, environment);

    let diagnostic = BenchmarkDiagnostic::new("too_fast", DiagnosticSeverity::Warning, "reason")
        .with_evidence("ns", "1")
        .with_suggestion("do more work");
    assert_eq!(diagnostic.code, "too_fast");
    assert_eq!(diagnostic.severity, DiagnosticSeverity::Warning);
    assert_eq!(diagnostic.reason, "reason");
    assert_eq!(diagnostic.evidence.get("ns").map(String::as_str), Some("1"));
    assert_eq!(diagnostic.suggestions, vec!["do more work".to_string()]);

    let mut summary = BenchmarkSummary::new("s/b", "b", 3, PrimaryMetric::NsPerOp);
    assert_eq!(summary.benchmark_id, "s/b");
    assert_eq!(summary.name, "b");
    assert_eq!(summary.tier, 3);
    assert_eq!(summary.primary_metric, PrimaryMetric::NsPerOp);
    assert!(summary.stats.is_none());
    assert!(summary.diagnostics.is_empty());
    summary.diagnostics.push(diagnostic.clone());

    let ledger = DiagnosticSummary::from_diagnostic("s", &summary, &diagnostic);
    assert_eq!(ledger.suite, "s");
    assert_eq!(ledger.benchmark_id, "s/b");
    assert_eq!(ledger.tier, 3);
    assert_eq!(ledger.code, "too_fast");
    assert_eq!(ledger.suggestions, diagnostic.suggestions);
    assert_eq!(ledger.quality, summary.quality);

    let comparison = ComparisonResult::new(
        "s/b",
        PrimaryMetric::NsPerOp,
        QualityClass::Acceptable,
        ComparisonClass::MissingBaseline,
        0.05,
    );
    assert_eq!(comparison.benchmark_id, "s/b");
    assert_eq!(comparison.classification, ComparisonClass::MissingBaseline);
    assert!(comparison.baseline_value.is_none());
    assert!(comparison.reason.is_none());

    let run = StressRun::new("s", RunProfile::Smoke, environment);
    assert_eq!(run.schema_version, SCHEMA_VERSION);
    assert_eq!(run.tool_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(run.suite, "s");
    assert_eq!(run.run_profile, RunProfile::Smoke);
    assert!(run.samples.is_empty() && run.summaries.is_empty() && run.comparisons.is_empty());
}

#[test]
fn nested_artifact_types_build_via_constructors_only() {
    use cntryl_stress::artifact::{
        BenchmarkBudgets, BenchmarkMode, BenchmarkSpec, BudgetResult, ConfidenceInterval,
        CorrectnessSummary, MeasurementIntent, ObservationDirection, ObservationSummary,
        ObservationUnit, ScalarObservation, SummaryStats,
    };

    let mode = BenchmarkMode::FixedDuration {
        sample_duration: Duration::from_millis(1),
    };
    let spec = BenchmarkSpec::new("s/b", "b", 4, mode.clone());
    assert_eq!(spec.id, "s/b");
    assert_eq!(spec.name, "b");
    assert_eq!(spec.tier, 4);
    assert_eq!(spec.mode, mode);
    assert_eq!(spec.intent, MeasurementIntent::General);
    assert_eq!(spec.budgets, BenchmarkBudgets::default());
    assert!(spec.parameters.is_empty() && spec.metadata.is_empty());

    let interval = ConfidenceInterval::new(1.0, 2.0);
    assert!((interval.lower - 1.0).abs() < f64::EPSILON);
    assert!((interval.upper - 2.0).abs() < f64::EPSILON);

    let budget = BudgetResult::new("max_allocs_per_op", 0.0, true);
    assert_eq!(budget.metric, "max_allocs_per_op");
    assert!(budget.passed);
    assert!(budget.actual.is_none() && budget.reason.is_none());

    let correctness = CorrectnessSummary::new(false);
    assert!(!correctness.passed);
    assert!(correctness.errors.is_empty());

    let observation = ScalarObservation::new(
        "rows",
        3.0,
        ObservationUnit::Count,
        ObservationDirection::HigherIsBetter,
    );
    assert_eq!(observation.name, "rows");

    let stats = SummaryStats::from_values(&[1.0, 2.0]).expect("finite values");
    let summary = ObservationSummary::new(
        "rows",
        ObservationUnit::Count,
        ObservationDirection::HigherIsBetter,
        stats.clone(),
    );
    assert_eq!(summary.name, "rows");
    assert_eq!(summary.stats, stats);
}

#[test]
fn v0_4_0_fixture_deserializes_and_reserializes() {
    use cntryl_stress::artifact::StressRun;

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/baseline-main-v0.4.0.json");
    let original: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read fixture"))
            .expect("fixture is json");
    let run: StressRun = serde_json::from_value(original.clone()).expect("v0.4 fixture loads");
    let reserialized = serde_json::to_value(&run).expect("reserialize");
    let reparsed: StressRun = serde_json::from_value(reserialized.clone()).expect("reparse");
    assert_eq!(reparsed, run);
    assert!(run
        .summaries
        .iter()
        .all(|summary| summary.peak_rss_bytes.is_none()));
    assert_eq!(
        reserialized, original,
        "v0.4 fixture must round-trip losslessly"
    );
}

#[test]
fn non_exhaustive_public_types_remain_usable_downstream() {
    use cntryl_stress::runner::RunGate;

    let mut config = StressRunnerConfig::for_profile(RunProfile::Smoke);
    config.samples = 2;
    config.fail_on_quality = false;
    assert_eq!(config.samples, 2);
    assert_eq!(StressRunnerConfig::default().profile, RunProfile::Default);

    let label = match RunGate::Passed {
        RunGate::Passed => "passed",
        RunGate::CorrectnessFailed => "correctness",
        _ => "other",
    };
    assert_eq!(label, "passed");
}

#[test]
fn budgets_and_counters_build_via_constructors() {
    use cntryl_stress::artifact::{BenchmarkBudgets, CorrectnessCounters};

    const BUDGETS: BenchmarkBudgets = BenchmarkBudgets::new()
        .with_max_ns_per_op(10.0)
        .with_max_allocs_per_op(1.0)
        .with_max_bytes_per_op(64.0)
        .with_max_regression_pct(5.0)
        .with_max_rsd_pct(3.0)
        .with_max_peak_rss_mb(512.0);
    assert_eq!(BUDGETS.max_peak_rss_mb, Some(512.0));
    assert_eq!(BUDGETS.max_ns_per_op, Some(10.0));
    assert_eq!(BUDGETS.max_allocs_per_op, Some(1.0));
    assert_eq!(BUDGETS.max_bytes_per_op, Some(64.0));
    assert_eq!(BUDGETS.max_regression_pct, Some(5.0));
    assert_eq!(BUDGETS.max_rsd_pct, Some(3.0));
    assert_eq!(BenchmarkBudgets::new(), BenchmarkBudgets::default());

    let counters = CorrectnessCounters::new()
        .with_attempted(4)
        .with_completed(3)
        .with_failures(1)
        .with_timeouts(2)
        .with_duplicates(3)
        .with_dropped(4)
        .with_validation_errors(5);
    assert_eq!(counters.attempted, 4);
    assert_eq!(counters.completed, 3);
    assert_eq!(counters.failures, 1);
    assert_eq!(counters.timeouts, 2);
    assert_eq!(counters.duplicates, 3);
    assert_eq!(counters.dropped, 4);
    assert_eq!(counters.validation_errors, 5);
    assert_eq!(CorrectnessCounters::new(), CorrectnessCounters::default());
}

#[test]
fn public_enums_require_wildcard_arms() {
    use cntryl_stress::artifact::{DiagnosticSeverity, RunProfile};

    #[allow(unreachable_patterns)]
    const fn label(profile: RunProfile) -> &'static str {
        match profile {
            RunProfile::Default => "default",
            _ => "other",
        }
    }
    assert_eq!(label(RunProfile::Smoke), "other");
    assert!(DiagnosticSeverity::Error.at_least(DiagnosticSeverity::Warning));
}

#[test]
fn source_location_is_optional_and_additive_on_summaries() {
    use cntryl_stress::artifact::SourceLocation;

    let location = SourceLocation::new("benches/queue.rs", 42);
    assert_eq!(location.file, "benches/queue.rs");
    assert_eq!(location.line, 42);
    assert_eq!(location.to_string(), "benches/queue.rs:42");

    let value = serde_json::to_value(&location).expect("serialize location");
    assert_eq!(
        value,
        serde_json::json!({ "file": "benches/queue.rs", "line": 42 })
    );
}
