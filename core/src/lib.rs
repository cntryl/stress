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
pub mod artifact;
mod config;
pub mod context;
mod harness;
pub mod reporting;
pub mod runner;

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

pub use artifact::RunProfile;
pub use config::StressRunnerConfig;
pub use context::StressContext;
pub use runner::StressRunner;
pub use std::hint::black_box;

pub use cntryl_stress_macros::{stress, stress_main};
pub use harness::StressRunnerOptions;

/// Private module for macro internals.
#[doc(hidden)]
pub mod __private {
    pub use crate::allocation::{
        stress_allocator_installed_marker, StressAllocator, STRESS_ALLOCATOR_INSTALLATIONS,
    };
    pub use crate::harness::{linkme, stress_binary_main, BenchmarkEntry, STRESS_BENCHMARKS};

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
