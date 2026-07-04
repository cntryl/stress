//! # cntryl-stress
//!
//! A raw-sample-first stress benchmarking framework for Tier 2 through Tier N
//! performance work. Tier 1 microbenchmarks stay in Criterion.
//!
//! The common benchmark body is intentionally small:
//!
//! ```rust,ignore
//! use cntryl_stress::{stress_test, StressContext};
//!
//! #[stress_test(tier = 2)]
//! fn write_batch(ctx: &mut StressContext) {
//!     let batch = build_batch();
//!     ctx.parameter("payload_size", batch.len());
//!     ctx.measure(|| write_to_system(&batch));
//! }
//!
//! cntryl_stress::stress_main!();
//! ```

mod config;
mod context;
mod harness;
mod report;
mod result;
mod runner;

pub use config::StressRunnerConfig;
pub use context::{CorrectnessRecorder, StressContext};
pub use report::{ConsoleReporter, GitHubActionsReporter, JsonReporter, MultiReporter, Reporter};
pub use result::{
    BenchmarkMode, BenchmarkModeKind, BenchmarkSpec, BenchmarkSummary, ComparisonClass,
    ComparisonResult, ConfidenceInterval, CorrectnessCounters, CorrectnessSummary, EnvironmentInfo,
    PrimaryMetric, ProfileConfig, QualityClass, RunProfile, Sample, SamplePhase, StressRun,
    SummaryStats, SCHEMA_VERSION,
};
pub use runner::{evaluate_run_gate, RunGate, StressRunner};

pub use cntryl_stress_macros::{stress_main, stress_test};
pub use harness::stress_binary_main;
pub use harness::{benchmark_count, list_benchmarks};
pub use harness::{
    run_from_env_and_args, run_registered_benchmarks, run_with_options, StressRunnerOptions,
};

/// Private module for macro internals.
#[doc(hidden)]
pub mod __private {
    pub use crate::harness::{linkme, BenchmarkEntry, STRESS_BENCHMARKS};
}

/// Prelude module for benchmark files.
pub mod prelude {
    pub use crate::{
        stress_main, stress_test, RunProfile, StressContext, StressRunner, StressRunnerConfig,
        StressRunnerOptions,
    };
}
