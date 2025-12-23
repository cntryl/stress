//! # cntryl-stress
//!
//! A lightweight single-shot benchmark runner for system-level stress tests.
//!
//! Unlike Criterion (which uses statistical sampling), this crate is designed for
//! expensive operations where each iteration matters: disk I/O, network calls,
//! database transactions, compaction, recovery, etc.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use cntryl_stress::{BenchRunner, BenchContext};
//!
//! let mut runner = BenchRunner::new("my_suite");
//!
//! runner.run("expensive_operation", |ctx| {
//!     // Setup (not timed)
//!     let data = prepare_data();
//!     
//!     // Measure exactly one operation
//!     ctx.measure(|| {
//!         expensive_operation(&data);
//!     });
//!     
//!     // Teardown (not timed)
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
//! - **`hdr`**: Enable HDR histogram for latency percentiles
//! - **`async`**: Enable async benchmark support

mod context;
mod result;
mod runner;
mod config;
mod report;
mod benches;

pub use context::BenchContext;
pub use result::{BenchResult, SuiteResult};
pub use runner::BenchRunner;
pub use config::BenchRunnerConfig;
pub use report::{Reporter, ConsoleReporter, JsonReporter, MultiReporter};

// Re-export benches helper so the `cargo-stress` binary (or users) can call it
pub use benches::register_benchmarks;

#[cfg(feature = "hdr")]
pub mod histogram;

#[cfg(feature = "async")]
pub mod async_runner;
