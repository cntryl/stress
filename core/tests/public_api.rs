#![allow(dead_code)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::too_many_lines)]

use cntryl_stress::{
    black_box, stress, stress_allocator, LogicalUnit, OperationOutcome, RunProfile, StressContext,
    StressError, StressResult, StressRunner, StressRunnerConfig, StressRunnerOptions,
};
use std::collections::BTreeMap;
use std::time::Duration;

stress_allocator!();

#[stress(tier = 1, max_allocs_per_op = 0, metadata(component = "api"))]
fn macro_expansion_uses_namespaced_artifact_types(ctx: &mut StressContext) {
    ctx.measure("hot path", || black_box(1_u64));
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
        BenchmarkBudgets, BenchmarkMode, BenchmarkSpec, BenchmarkSummary, ConsoleNameMode,
        CorrectnessCounters, CorrectnessSummary, EnvironmentInfo, MeasurementIntent, PrimaryMetric,
        ProfileConfig, QualityClass, RunProfile, Sample, SamplePhase, StressRun, SummaryStats,
        SCHEMA_VERSION,
    };

    let profile_config = ProfileConfig {
        profile: RunProfile::Smoke,
        measured_samples: 1,
        warmup_samples: 0,
        cooldown_samples: 0,
        min_quality: QualityClass::Untrustworthy,
        fail_on_quality: false,
        fail_on_regression: false,
        deny_diagnostics: None,
        regression_threshold: 0.05,
        sample_duration: Duration::from_millis(10),
        operations_per_sample: 1,
        micro_sample_duration: Duration::from_millis(5),
        report_depth: "summary".to_string(),
        console_names: ConsoleNameMode::Compact,
        progress: true,
    };
    let environment = EnvironmentInfo::unknown(profile_config);

    StressRun {
        schema_version: SCHEMA_VERSION.to_string(),
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        suite: "suite".to_string(),
        run_profile: RunProfile::Smoke,
        environment: environment.clone(),
        benchmark_specs: vec![BenchmarkSpec {
            id: "suite/bench".to_string(),
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
            benchmark_id: "suite/bench".to_string(),
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
            observations: Vec::new(),
            parameters: BTreeMap::new(),
            counters: CorrectnessCounters {
                attempted: 1,
                completed: 1,
                ..CorrectnessCounters::default()
            },
            environment,
        }],
        summaries: vec![BenchmarkSummary {
            benchmark_id: "suite/bench".to_string(),
            name: "bench".to_string(),
            tier: 2,
            intent: MeasurementIntent::General,
            primary_metric: PrimaryMetric::Throughput,
            measured_samples: 1,
            warmup_samples: 0,
            cooldown_samples: 0,
            stats: SummaryStats::from_values(&[1.0]),
            wall_clock: None,
            total_wall_clock_ns: 1,
            ns_per_op: None,
            gross_ns_per_op: None,
            overhead_ns_per_op: None,
            allocs_per_op: None,
            bytes_per_op: None,
            observations: Vec::new(),
            quality: QualityClass::Untrustworthy,
            trust_class: cntryl_stress::artifact::TrustClass::Gate,
            budgets: BenchmarkBudgets::default(),
            budget_results: Vec::new(),
            diagnostics: Vec::new(),
            correctness: CorrectnessSummary {
                passed: true,
                counters: CorrectnessCounters {
                    attempted: 1,
                    completed: 1,
                    ..CorrectnessCounters::default()
                },
                errors: Vec::new(),
            },
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
        }],
        comparisons: Vec::new(),
        diagnostics_summary: Vec::new(),
        started_at: "1".to_string(),
        total_elapsed_ns: 1,
        metadata: BTreeMap::new(),
    }
}
