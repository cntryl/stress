//! Benchmark context for timing, workload facts, and correctness counters.

use crate::result::{BenchmarkMode, CorrectnessCounters};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Context passed to benchmark closures.
///
/// A benchmark must record exactly one measured duration, usually with
/// [`StressContext::measure_workload`], [`StressContext::measure`], or
/// [`StressContext::measure_for`].
pub struct StressContext {
    pub(crate) mode: BenchmarkMode,
    pub(crate) duration: Option<Duration>,
    pub(crate) parameters: BTreeMap<String, String>,
    pub(crate) metadata: BTreeMap<String, String>,
    pub(crate) latency_ns: Vec<u128>,
    pub(crate) counters: CorrectnessCounters,
    pub(crate) operations_hint: Option<u64>,
}

impl StressContext {
    pub(crate) fn new(mode: BenchmarkMode) -> Self {
        Self {
            mode,
            duration: None,
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
            latency_ns: Vec::new(),
            counters: CorrectnessCounters::default(),
            operations_hint: None,
        }
    }

    /// Add a structured workload parameter, such as `client_count`,
    /// `payload_size`, `transport`, `operation`, or `scenario`.
    #[allow(clippy::needless_pass_by_value)]
    pub fn parameter(&mut self, key: impl Into<String>, value: impl ToString) -> &mut Self {
        self.parameters.insert(key.into(), value.to_string());
        self
    }

    /// Add descriptive benchmark metadata.
    #[allow(clippy::needless_pass_by_value)]
    pub fn metadata(&mut self, key: impl Into<String>, value: impl ToString) -> &mut Self {
        self.metadata.insert(key.into(), value.to_string());
        self
    }

    /// Record one latency observation.
    pub fn record_latency(&mut self, duration: Duration) -> &mut Self {
        self.latency_ns.push(duration.as_nanos());
        self
    }

    /// Record canonical correctness counters.
    #[must_use]
    pub fn correctness(&mut self) -> CorrectnessRecorder<'_> {
        CorrectnessRecorder {
            counters: &mut self.counters,
        }
    }

    fn set_duration(&mut self, duration: Duration) {
        assert!(
            self.duration.is_none(),
            "Timing was recorded more than once for this benchmark."
        );
        self.duration = Some(duration);
    }

    fn set_successful_operations_if_unset(&mut self, operations: u64) {
        self.operations_hint = Some(operations);
        if self.counters.attempted == 0 && self.counters.completed == 0 {
            self.counters.attempted = operations;
            self.counters.completed = operations;
        }
    }

    /// Time exactly one sample according to the active [`BenchmarkMode`].
    ///
    /// For `fixed_duration`, the closure is called until the sample duration
    /// elapses. For `fixed_operations`, the closure is called
    /// `operations_per_sample` times.
    #[must_use = "use the operation count for additional validation when needed"]
    pub fn measure_workload<F>(&mut self, mut f: F) -> u64
    where
        F: FnMut(),
    {
        match self.mode.clone() {
            BenchmarkMode::FixedDuration { sample_duration } => {
                let start = Instant::now();
                let mut operations = 0_u64;
                loop {
                    f();
                    operations = operations.saturating_add(1);
                    if start.elapsed() >= sample_duration {
                        break;
                    }
                }
                self.set_duration(start.elapsed());
                self.set_successful_operations_if_unset(operations);
                operations
            }
            BenchmarkMode::FixedOperations {
                operations_per_sample,
            } => {
                let start = Instant::now();
                for _ in 0..operations_per_sample {
                    f();
                }
                self.set_duration(start.elapsed());
                self.set_successful_operations_if_unset(operations_per_sample);
                operations_per_sample
            }
        }
    }

    /// Time a single-shot operation.
    pub fn measure<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let start = Instant::now();
        let result = f();
        self.set_duration(start.elapsed());
        self.set_successful_operations_if_unset(1);
        result
    }

    /// Time a repeated operation until the requested wall-clock budget is met.
    #[must_use = "use the iteration count to report throughput totals"]
    pub fn measure_for<F>(&mut self, duration: Duration, mut f: F) -> usize
    where
        F: FnMut(),
    {
        let start = Instant::now();
        let mut iterations = 0_usize;

        loop {
            f();
            iterations = iterations.saturating_add(1);

            if start.elapsed() >= duration {
                break;
            }
        }

        self.set_duration(start.elapsed());
        self.set_successful_operations_if_unset(iterations as u64);
        iterations
    }

    /// Time an operation on a borrowed reference.
    pub fn measure_ref<F, T, R>(&mut self, target: &T, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        let start = Instant::now();
        let result = f(target);
        self.set_duration(start.elapsed());
        self.set_successful_operations_if_unset(1);
        result
    }

    /// Time an operation on a mutable reference.
    pub fn measure_mut<F, T, R>(&mut self, target: &mut T, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        let start = Instant::now();
        let result = f(target);
        self.set_duration(start.elapsed());
        self.set_successful_operations_if_unset(1);
        result
    }

    /// Manually record a duration for externally timed systems under test.
    pub fn record_duration(&mut self, duration: Duration) {
        self.set_duration(duration);
        self.set_successful_operations_if_unset(1);
    }
}

/// Fluent recorder for correctness counters.
pub struct CorrectnessRecorder<'a> {
    counters: &'a mut CorrectnessCounters,
}

impl CorrectnessRecorder<'_> {
    /// Set attempted operations.
    #[must_use]
    pub fn attempted(self, value: u64) -> Self {
        self.counters.attempted = value;
        self
    }

    /// Set completed operations.
    #[must_use]
    pub fn completed(self, value: u64) -> Self {
        self.counters.completed = value;
        self
    }

    /// Set failed operations.
    #[must_use]
    pub fn failures(self, value: u64) -> Self {
        self.counters.failures = value;
        self
    }

    /// Set timed out operations.
    #[must_use]
    pub fn timeouts(self, value: u64) -> Self {
        self.counters.timeouts = value;
        self
    }

    /// Set duplicate operations/results.
    #[must_use]
    pub fn duplicates(self, value: u64) -> Self {
        self.counters.duplicates = value;
        self
    }

    /// Set dropped operations/results.
    #[must_use]
    pub fn dropped(self, value: u64) -> Self {
        self.counters.dropped = value;
        self
    }

    /// Set validation errors.
    #[must_use]
    pub fn validation_errors(self, value: u64) -> Self {
        self.counters.validation_errors = value;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> StressContext {
        StressContext::new(BenchmarkMode::FixedOperations {
            operations_per_sample: 3,
        })
    }

    #[test]
    fn measure_workload_records_fixed_operations_sample() {
        let mut ctx = ctx();
        let operations = ctx.measure_workload(|| {
            std::hint::black_box(1_u64);
        });

        assert_eq!(operations, 3);
        assert_eq!(ctx.counters.attempted, 3);
        assert_eq!(ctx.counters.completed, 3);
        assert!(ctx.duration.expect("duration") > Duration::ZERO);
    }

    #[test]
    fn measure_for_records_operation_hint() {
        let mut ctx = StressContext::new(BenchmarkMode::FixedDuration {
            sample_duration: Duration::from_millis(1),
        });
        let iterations = ctx.measure_for(Duration::from_millis(1), || {
            std::hint::black_box(1_usize);
        });

        assert!(iterations > 0);
        assert_eq!(ctx.operations_hint, Some(iterations as u64));
        assert_eq!(ctx.counters.completed, iterations as u64);
    }

    #[test]
    fn records_parameters_metadata_latency_and_correctness() {
        let mut ctx = ctx();
        ctx.parameter("client_count", 4)
            .metadata("scenario", "fanout")
            .record_latency(Duration::from_micros(25));
        let _ = ctx
            .correctness()
            .attempted(10)
            .completed(9)
            .failures(1)
            .timeouts(2)
            .duplicates(3)
            .dropped(4)
            .validation_errors(5);

        assert_eq!(ctx.parameters.get("client_count"), Some(&"4".to_string()));
        assert_eq!(ctx.metadata.get("scenario"), Some(&"fanout".to_string()));
        assert_eq!(ctx.latency_ns, vec![25_000]);
        assert_eq!(ctx.counters.attempted, 10);
        assert_eq!(ctx.counters.completed, 9);
        assert_eq!(ctx.counters.failures, 1);
        assert_eq!(ctx.counters.timeouts, 2);
        assert_eq!(ctx.counters.duplicates, 3);
        assert_eq!(ctx.counters.dropped, 4);
        assert_eq!(ctx.counters.validation_errors, 5);
    }

    #[test]
    #[should_panic(expected = "Timing was recorded more than once")]
    fn panics_when_timing_recorded_twice() {
        let mut ctx = ctx();
        ctx.measure(|| {});
        ctx.measure(|| {});
    }
}
