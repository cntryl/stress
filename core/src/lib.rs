//! # cntryl-stress
//!
//! A raw-sample-first stress benchmarking framework for Tier 1 through Tier N
//! performance work.
//!
//! Tier labels:
//!
//! - Tier 1: hot path
//! - Tier 2: subsystem
//! - Tier 3: system
//! - Tier 4: integration
//! - Tier 5: saturation/scaling
//! - Tier 6: soak/endurance
//!
//! The common benchmark body is intentionally small:
//!
//! ```rust,ignore
//! use cntryl_stress::{black_box, stress_test, StressContext};
//!
//! #[stress_test(tier = 1)]
//! fn parse_hot_path(ctx: &mut StressContext) {
//!     let header = b"content-type:application/json";
//!     ctx.measure_micro(|| black_box(header.iter().position(|byte| *byte == b':')));
//! }
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

mod allocation;
mod config;
mod context;
mod harness;
mod report;
mod result;
mod runner;

#[cfg(test)]
#[global_allocator]
static CNTRYL_STRESS_TEST_ALLOCATOR: allocation::StressAllocator =
    allocation::StressAllocator::new();

#[cfg(test)]
#[allow(non_upper_case_globals)]
#[linkme::distributed_slice(allocation::STRESS_ALLOCATOR_INSTALLATIONS)]
#[linkme(crate = linkme)]
static CNTRYL_STRESS_TEST_ALLOCATOR_INSTALLATION: fn() =
    allocation::stress_allocator_installed_marker;

pub use allocation::StressAllocator;
pub use config::{ConsoleMode, StressRunnerConfig};
pub use context::{CorrectnessRecorder, StressContext};
pub use report::{ConsoleReporter, GitHubActionsReporter, JsonReporter, MultiReporter, Reporter};
pub use result::{
    BenchmarkBudgets, BenchmarkMode, BenchmarkModeKind, BenchmarkSpec, BenchmarkSummary,
    BudgetResult, ComparisonClass, ComparisonResult, ConfidenceInterval, CorrectnessCounters,
    CorrectnessSummary, EnvironmentInfo, PrimaryMetric, ProfileConfig, QualityClass, RunProfile,
    Sample, SamplePhase, StressRun, SummaryStats, SCHEMA_VERSION,
};
pub use runner::{evaluate_run_gate, RunGate, StressRunner};
pub use std::hint::black_box;

pub use cntryl_stress_macros::{stress_main, stress_test};
pub use harness::stress_binary_main;
pub use harness::{benchmark_count, list_benchmarks};
pub use harness::{
    run_from_env_and_args, run_registered_benchmarks, run_with_options, StressRunnerOptions,
};

/// Private module for macro internals.
#[doc(hidden)]
pub mod __private {
    pub use crate::allocation::{
        stress_allocator_installed_marker, STRESS_ALLOCATOR_INSTALLATIONS,
    };
    pub use crate::harness::{linkme, BenchmarkEntry, STRESS_BENCHMARKS};
}

/// Install the stress allocation-counting global allocator.
#[macro_export]
macro_rules! stress_allocator {
    () => {
        #[global_allocator]
        static CNTRYL_STRESS_ALLOCATOR: $crate::StressAllocator = $crate::StressAllocator::new();

        #[allow(non_upper_case_globals)]
        #[::cntryl_stress::__private::linkme::distributed_slice(
            ::cntryl_stress::__private::STRESS_ALLOCATOR_INSTALLATIONS
        )]
        #[linkme(crate = ::cntryl_stress::__private::linkme)]
        static CNTRYL_STRESS_ALLOCATOR_INSTALLATION: fn() =
            $crate::__private::stress_allocator_installed_marker;
    };
}

/// Prelude module for benchmark files.
pub mod prelude {
    pub use crate::{
        black_box, stress_allocator, stress_main, stress_test, ConsoleMode, RunProfile,
        StressContext, StressRunner, StressRunnerConfig, StressRunnerOptions,
    };
}
