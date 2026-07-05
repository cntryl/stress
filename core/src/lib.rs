//! # cntryl-stress
//!
//! A raw-sample-first stress benchmarking framework for Tier 1 through Tier 6
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
//! use cntryl_stress::{black_box, stress, StressContext};
//!
//! #[stress(tier = 1)]
//! fn parse_hot_path(ctx: &mut StressContext) {
//!     let header = b"content-type:application/json";
//!     ctx.measure("colon lookup", || black_box(header.iter().position(|byte| *byte == b':')));
//! }
//!
//! #[stress(tier = 2)]
//! fn write_batch(ctx: &mut StressContext) {
//!     let batch = build_batch();
//!     ctx.parameter("payload_size", batch.len());
//!     ctx.measure("write batch", || write_to_system(&batch));
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
pub use config::StressRunnerConfig;
pub use context::{CorrectnessRecorder, StressContext};
pub use report::{
    format_console_run, format_console_runs, ConsoleReporter, GitHubActionsReporter, JsonReporter,
    MultiReporter, Reporter,
};
pub use result::{
    BenchmarkBudgets, BenchmarkDiagnostic, BenchmarkMode, BenchmarkModeKind, BenchmarkSpec,
    BenchmarkSummary, BudgetResult, ComparisonClass, ComparisonResult, ConfidenceInterval,
    CorrectnessCounters, CorrectnessSummary, DiagnosticSeverity, EnvironmentInfo,
    MeasurementIntent, PrimaryMetric, ProfileConfig, QualityClass, RunProfile, Sample, SamplePhase,
    StressRun, SummaryStats, MAX_TIER, SCHEMA_VERSION,
};
pub use runner::{evaluate_run_gate, RunGate, StressRunner};
pub use std::hint::black_box;

pub use cntryl_stress_macros::{stress, stress_main};
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

    /// Run a future to completion without requiring a runtime dependency.
    pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
        use std::pin::pin;
        use std::sync::Arc;
        use std::task::{Context, Poll, Wake, Waker};
        use std::time::Duration;

        struct ThreadWaker(std::thread::Thread);

        impl Wake for ThreadWaker {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }

            fn wake_by_ref(self: &Arc<Self>) {
                self.0.unpark();
            }
        }

        let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
        let mut context = Context::from_waker(&waker);
        let mut future = pin!(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::park_timeout(Duration::from_millis(1)),
            }
        }
    }
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
        black_box, stress, stress_allocator, stress_main, RunProfile, StressContext, StressRunner,
        StressRunnerConfig, StressRunnerOptions,
    };
}
