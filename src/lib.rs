//! # cntryl-stress
//!
//! A lightweight single-shot benchmark runner for system-level stress tests.
//!
//! Unlike Criterion (which uses statistical sampling), this crate is designed for
//! expensive operations where each iteration matters: disk I/O, network calls,
//! database transactions, compaction, recovery, etc.
//!
//! ## Quick Start (Attribute Style - Recommended)
//!
//! The easiest way to write stress tests is with the `#[stress_test]` attribute:
//!
//! ```rust,ignore
//! use cntryl_stress::{stress_test, BenchContext};
//!
//! #[stress_test]
//! fn write_1mb_file(ctx: &mut BenchContext) {
//!     let data = vec![0u8; 1024 * 1024];
//!     ctx.set_bytes(data.len() as u64);
//!     
//!     ctx.measure(|| {
//!         std::fs::write("/tmp/test", &data).unwrap();
//!     });
//!     
//!     std::fs::remove_file("/tmp/test").ok();
//! }
//!
//! #[stress_test]
//! fn database_insert(ctx: &mut BenchContext) {
//!     let db = setup_database();
//!     ctx.measure(|| {
//!         db.insert("key", "value");
//!     });
//! }
//!
//! // Generate main function
//! cntryl_stress::stress_main!();
//! # fn setup_database() -> FakeDb { FakeDb }
//! # struct FakeDb;
//! # impl FakeDb { fn insert(&self, _: &str, _: &str) {} }
//! ```
//!
//! Then run with:
//!
//! ```bash
//! cargo stress                          # Run all stress tests
//! cargo stress --workload "database*"   # Run matching tests
//! cargo stress --workload "*insert*"    # Glob patterns supported
//! ```
//!
//! ## Manual Runner Style
//!
//! For more control, use the `BenchRunner` directly:
//!
//! ```rust,no_run
//! use cntryl_stress::{BenchRunner, BenchContext};
//!
//! let mut runner = BenchRunner::new("my_suite");
//!
//! runner.run("expensive_operation", |ctx| {
//!     let data = prepare_data();
//!     ctx.measure(|| {
//!         expensive_operation(&data);
//!     });
//!     cleanup(&data);
//! });
//!
//! runner.finish();
//!
//! fn prepare_data() -> Vec<u8> { vec![0; 1024] }
//! fn expensive_operation(_: &[u8]) {}
//! fn cleanup(_: &[u8]) {}
//! ```
//!
//! ## Features
//!
//! - **Single-shot measurements** — no statistical sampling overhead
//! - **Glob filtering** — run subsets with `--workload "pattern*"`

mod config;
mod context;
mod harness;
mod report;
mod result;
mod runner;

pub use config::BenchRunnerConfig;
pub use context::BenchContext;
pub use report::{ConsoleReporter, JsonReporter, MultiReporter, Reporter};
pub use result::{BenchResult, SuiteResult};
pub use runner::BenchRunner;

// Harness exports for auto-discovery
pub use harness::{benchmark_count, list_benchmarks};
pub use harness::{run_registered_benchmarks, run_with_options, StressRunnerOptions};

// Re-export the proc macro
pub use cntryl_stress_macros::{stress_main, stress_test};

/// Private module for macro internals - do not use directly.
#[doc(hidden)]
pub mod __private {
    pub use crate::harness::{linkme, BenchmarkEntry, STRESS_BENCHMARKS};
}

/// Prelude module for convenient imports.
///
/// ```rust,ignore
/// use cntryl_stress::prelude::*;
/// ```
pub mod prelude {
    pub use crate::{
        stress_main, stress_test, BenchContext, BenchResult, BenchRunner, BenchRunnerConfig,
        StressRunnerOptions,
    };
}
