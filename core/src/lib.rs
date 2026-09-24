//! # cntryl-stress
//!
//! Performance benchmarks for engineers who need trustworthy artifacts, not
//! just timing numbers.
//!
//! cntryl-stress is an opinionated Rust benchmarking framework for performance
//! engineering loops. It keeps benchmark authoring low ceremony while producing
//! structured artifacts, diagnostics, and gates that can support real
//! optimization decisions.
//!
//! The framework is designed around one core question:
//!
//! - Can this benchmark row be trusted?
//!
//! It helps answer that by recording raw samples, deriving summaries from
//! measured samples only, preserving correctness counters, and calling out
//! common benchmark-shape mistakes automatically.
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
//! ```rust,no_run
//! use cntryl_stress::{black_box, stress, LogicalUnit, OperationOutcome, StressContext};
//!
//! #[stress(tier = 1)]
//! fn parse_hot_path(ctx: &mut StressContext) {
//!     let header = b"content-type:application/json";
//!     ctx.measure("colon lookup", || black_box(header.iter().position(|byte| *byte == b':')));
//! }
//!
//! #[stress(tier = 3)]
//! fn process_batch(ctx: &mut StressContext) {
//!     let records = [1_u64, 2, 3];
//!     ctx.measure_outcome("process batch", LogicalUnit::new("record"), || {
//!         black_box(records.iter().sum::<u64>());
//!         OperationOutcome::success(records.len() as u64)
//!     });
//! }
//! ```
//!
//! A `harness = false` benchmark binary ends with
//! `cntryl_stress::stress_main!();`.

mod allocation;
pub mod artifact;
pub mod compare;
mod config;
pub mod context;
pub mod diagnostics;
mod environment;
mod error;
mod harness;
pub mod reporting;
pub mod runner;
mod stationarity;

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

pub use artifact::{
    ObservationDirection, ObservationUnit, RunProfile, TrustClass as BenchmarkRole,
};
pub use config::StressRunnerConfig;
pub use context::{LogicalUnit, OperationOutcome, StressContext};
pub use error::{StressError, StressResult};
pub use runner::StressRunner;
pub use std::hint::black_box;

pub use cntryl_stress_macros::{stress, stress_main};
pub use harness::StressRunnerOptions;

/// Re-export of the `tokio` crate used by `#[stress(runtime = "tokio")]`.
///
/// Available with the `tokio` cargo feature, so benchmarks can use
/// `cntryl_stress::tokio::time::sleep` without a direct tokio dependency.
#[cfg(feature = "tokio")]
pub use tokio;

/// Macro-internal dispatch for `#[stress(runtime = "tokio" | "tokio-multi")]`.
#[cfg(feature = "tokio")]
#[doc(hidden)]
#[macro_export]
macro_rules! __stress_tokio_block_on {
    (current_thread, $future:expr) => {
        $crate::__private::block_on_tokio($future)
    };
    (multi_thread, $future:expr) => {
        $crate::__private::block_on_tokio_multi_thread($future)
    };
}

/// Macro-internal dispatch for `#[stress(runtime = "tokio" | "tokio-multi")]`.
#[cfg(not(feature = "tokio"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __stress_tokio_block_on {
    ($($tokens:tt)*) => {
        ::core::compile_error!(
            "#[stress(runtime = \"tokio\")] requires the `tokio` feature of cntryl-stress; add `features = [\"tokio\"]` to the cntryl-stress dependency"
        )
    };
}

/// Private module for macro internals.
#[doc(hidden)]
pub mod __private {
    pub use crate::allocation::{
        stress_allocator_installed_marker, StressAllocator, STRESS_ALLOCATOR_INSTALLATIONS,
    };
    pub use crate::error::IntoStressResult;
    pub use crate::harness::{
        canonical_suite_name, linkme, stress_binary_main, BenchmarkEntry, STRESS_BENCHMARKS,
    };

    /// Run a future on a fresh current-thread tokio runtime with timers enabled.
    ///
    /// The runtime is built on the calling thread, so it works on the isolated
    /// worker thread used for `--timeout-secs` deadlines.
    ///
    /// # Panics
    ///
    /// Panics when tokio cannot build the runtime, or when called from inside
    /// an existing tokio runtime.
    #[cfg(feature = "tokio")]
    pub fn block_on_tokio<F: std::future::Future>(future: F) -> F::Output {
        assert_outside_tokio_runtime();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("cntryl-stress could not build a current-thread tokio runtime")
            .block_on(future)
    }

    /// Run a future on a fresh multi-thread tokio runtime with timers enabled.
    ///
    /// # Panics
    ///
    /// Panics when tokio cannot build the runtime, or when called from inside
    /// an existing tokio runtime.
    #[cfg(feature = "tokio")]
    pub fn block_on_tokio_multi_thread<F: std::future::Future>(future: F) -> F::Output {
        assert_outside_tokio_runtime();
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("cntryl-stress could not build a multi-thread tokio runtime")
            .block_on(future)
    }

    #[cfg(feature = "tokio")]
    fn assert_outside_tokio_runtime() {
        assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "#[stress(runtime = \"tokio\")] benchmark invoked while already inside a tokio runtime; \
             run it from synchronous code (for example cargo stress or StressRunner::run outside #[tokio::test])"
        );
    }

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
        static CNTRYL_STRESS_ALLOCATOR: $crate::__private::StressAllocator =
            $crate::__private::StressAllocator::new();

        #[allow(non_upper_case_globals)]
        #[$crate::__private::linkme::distributed_slice(
            $crate::__private::STRESS_ALLOCATOR_INSTALLATIONS
        )]
        #[linkme(crate = $crate::__private::linkme)]
        static CNTRYL_STRESS_ALLOCATOR_INSTALLATION: fn() =
            $crate::__private::stress_allocator_installed_marker;
    };
}

/// Prelude module for benchmark files.
pub mod prelude {
    pub use crate::{
        black_box, stress, stress_allocator, stress_main, BenchmarkRole, ObservationDirection,
        ObservationUnit, RunProfile, StressContext, StressError, StressResult, StressRunner,
        StressRunnerConfig, StressRunnerOptions,
    };
    pub use crate::{LogicalUnit, OperationOutcome};
}
