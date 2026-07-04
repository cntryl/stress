//! Benchmark context for timing, workload facts, and correctness counters.

use crate::allocation;
use crate::result::{BenchmarkMode, CorrectnessCounters, MAX_TIER};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Context passed to benchmark closures.
///
/// A benchmark must record exactly one measured duration, usually with
/// the timing helper that matches the benchmark tier.
pub struct StressContext {
    pub(crate) tier: u32,
    pub(crate) mode: BenchmarkMode,
    pub(crate) duration: Option<Duration>,
    pub(crate) parameters: BTreeMap<String, String>,
    pub(crate) metadata: BTreeMap<String, String>,
    pub(crate) latency_ns: Vec<u128>,
    pub(crate) counters: CorrectnessCounters,
    pub(crate) operations_hint: Option<u64>,
    pub(crate) micro: Option<MicroMeasurement>,
    pub(crate) allocation: Option<AllocationMeasurement>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MicroMeasurement {
    pub iterations: u64,
    pub gross_elapsed: Duration,
    pub overhead: Duration,
    pub net_elapsed: Duration,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AllocationMeasurement {
    pub allocs: u64,
    pub bytes: u64,
}

impl StressContext {
    pub(crate) fn new(tier: u32, mode: BenchmarkMode) -> Self {
        Self {
            tier,
            mode,
            duration: None,
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
            latency_ns: Vec::new(),
            counters: CorrectnessCounters::default(),
            operations_hint: None,
            micro: None,
            allocation: None,
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

    /// Record the number of successful logical operations represented by this
    /// sample.
    ///
    /// Use this after a measured closure returns a logical operation count,
    /// or prefer [`StressContext::measure_counted`] for Tier 2 counted work.
    ///
    /// # Panics
    ///
    /// Panics when `completed` is zero.
    pub fn operations(&mut self, completed: u64) -> &mut Self {
        assert!(completed != 0, "ctx.operations() requires completed > 0");
        self.set_successful_operations(completed);
        self
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

    fn set_successful_operations(&mut self, operations: u64) {
        self.operations_hint = Some(operations);
        self.counters.attempted = operations;
        self.counters.completed = operations;
    }

    fn set_allocation_measurement(&mut self, delta: Option<allocation::AllocationDelta>) {
        self.allocation = delta.map(|delta| AllocationMeasurement {
            allocs: delta.allocs,
            bytes: delta.bytes,
        });
    }

    fn require_non_micro_timing_helper(&self, method: &str) {
        if matches!(self.mode, BenchmarkMode::Micro { .. }) {
            panic!(
                "ctx.{method}() cannot be used with mode = \"micro\"; use ctx.measure_micro() or ctx.measure_workload()"
            );
        }
    }

    fn require_one_shot_timing_helper(&self, method: &str) {
        self.require_non_micro_timing_helper(method);
        assert!(
            !(3..=MAX_TIER).contains(&self.tier),
            "ctx.{method}() cannot be used in Tier {} fixed-duration benchmarks; use ctx.measure_batch(...), ctx.measure_workload(...), or ctx.record_external(...)",
            self.tier
        );
    }

    fn require_tier2_counted_helper(&self, method: &str) {
        self.require_non_micro_timing_helper(method);
        match self.tier {
            2 => {}
            3..=MAX_TIER => {
                panic!(
                    "ctx.{method}() cannot be used in Tier {} fixed-duration benchmarks; use ctx.measure_batch(...), ctx.measure_workload(...), or ctx.record_external(...)",
                    self.tier
                );
            }
            _ => {
                panic!(
                    "ctx.{method}() is for Tier 2 counted work; use ctx.measure_micro(...) for Tier 1"
                );
            }
        }
    }

    fn measure_once<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let allocation_start = allocation::snapshot();
        let start = Instant::now();
        let result = f();
        let duration = start.elapsed();
        let allocation_delta = allocation_start.map(allocation::delta_since);
        self.set_duration(duration);
        self.set_allocation_measurement(allocation_delta);
        result
    }

    /// Time exactly one sample according to the active [`BenchmarkMode`].
    ///
    /// For `fixed_duration`, the closure is called until the sample duration
    /// elapses. For `fixed_operations`, the closure is called
    /// `operations_per_sample` times.
    ///
    /// # Panics
    ///
    /// Panics if the benchmark records timing more than once.
    #[must_use = "use the operation count for additional validation when needed"]
    pub fn measure_workload<F>(&mut self, mut f: F) -> u64
    where
        F: FnMut(),
    {
        match self.mode.clone() {
            BenchmarkMode::Micro { .. } => {
                self.measure_micro(&mut f);
                self.operations_hint.unwrap_or_default()
            }
            BenchmarkMode::FixedDuration { sample_duration } => {
                let allocation_start = allocation::snapshot();
                let start = Instant::now();
                let mut operations = 0_u64;
                loop {
                    f();
                    operations = operations.saturating_add(1);
                    if start.elapsed() >= sample_duration {
                        break;
                    }
                }
                let duration = start.elapsed();
                let allocation_delta = allocation_start.map(allocation::delta_since);
                self.set_duration(duration);
                self.set_allocation_measurement(allocation_delta);
                self.set_successful_operations_if_unset(operations);
                operations
            }
            BenchmarkMode::FixedOperations {
                operations_per_sample,
            } => {
                let allocation_start = allocation::snapshot();
                let start = Instant::now();
                for _ in 0..operations_per_sample {
                    f();
                }
                let duration = start.elapsed();
                let allocation_delta = allocation_start.map(allocation::delta_since);
                self.set_duration(duration);
                self.set_allocation_measurement(allocation_delta);
                self.set_successful_operations_if_unset(operations_per_sample);
                operations_per_sample
            }
        }
    }

    /// Time batched logical work according to the active non-micro
    /// [`BenchmarkMode`].
    ///
    /// The closure is executed once per framework iteration. The recorded
    /// attempted and completed operation counts are
    /// `iterations * logical_operations_per_iteration`.
    ///
    /// # Panics
    ///
    /// Panics in `mode = "micro"`, when timing was already recorded, or when
    /// `logical_operations_per_iteration` is zero.
    #[must_use = "use the completed logical operation count for additional validation when needed"]
    pub fn measure_batch<F>(&mut self, logical_operations_per_iteration: u64, f: F) -> u64
    where
        F: FnMut(),
    {
        self.require_non_micro_timing_helper("measure_batch");
        assert!(
            logical_operations_per_iteration != 0,
            "ctx.measure_batch() requires logical_operations_per_iteration > 0"
        );

        let iterations = self.measure_workload(f);
        let completed = iterations.saturating_mul(logical_operations_per_iteration);
        self.operations(completed);
        completed
    }

    /// Time a single-shot operation.
    pub fn measure<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        self.require_one_shot_timing_helper("measure");
        let result = self.measure_once(f);
        self.set_successful_operations_if_unset(1);
        result
    }

    /// Time one Tier 2 operation and record the logical operation count it
    /// returns.
    ///
    /// This is the compact form of timing one subsystem call and then calling
    /// [`StressContext::operations`] with the returned count.
    ///
    /// # Panics
    ///
    /// Panics outside Tier 2, when timing was already recorded, or when the
    /// closure returns zero completed operations.
    #[must_use = "use the completed logical operation count for additional validation when needed"]
    pub fn measure_counted<F>(&mut self, f: F) -> u64
    where
        F: FnOnce() -> u64,
    {
        self.require_tier2_counted_helper("measure_counted");
        let completed = self.measure_once(f);
        assert!(
            completed != 0,
            "ctx.measure_counted() requires completed_operations > 0"
        );
        self.set_successful_operations(completed);
        completed
    }

    /// Time a calibrated microbenchmark sample.
    ///
    /// The closure is batched until the gross sample duration reaches the
    /// profile's micro target window, then an empty-loop overhead batch is
    /// measured and subtracted from the recorded net duration.
    ///
    /// # Panics
    ///
    /// Panics when called outside `mode = "micro"` or when timing was already
    /// recorded for this sample.
    pub fn measure_micro<F, R>(&mut self, mut f: F) -> R
    where
        F: FnMut() -> R,
    {
        let target = match self.mode {
            BenchmarkMode::Micro {
                target_sample_duration,
            } => target_sample_duration,
            BenchmarkMode::FixedDuration { .. } | BenchmarkMode::FixedOperations { .. } => {
                panic!("ctx.measure_micro() requires mode = \"micro\"");
            }
        };

        let iterations = calibrate_iterations(target, &mut f);
        let allocation_start = allocation::snapshot();
        let (gross_elapsed, result) = time_operation_batch(iterations, &mut f);
        let allocation_delta = allocation_start.map(allocation::delta_since);
        let overhead = time_empty_batch(iterations);
        let net_elapsed = gross_elapsed.saturating_sub(overhead);
        let allocation_measurement = allocation_delta.map(|delta| AllocationMeasurement {
            allocs: delta.allocs,
            bytes: delta.bytes,
        });

        self.set_duration(net_elapsed);
        self.set_successful_operations_if_unset(iterations);
        self.allocation = allocation_measurement;
        self.micro = Some(MicroMeasurement {
            iterations,
            gross_elapsed,
            overhead,
            net_elapsed,
        });
        result
    }

    /// Time a repeated operation until the requested wall-clock budget is met.
    #[must_use = "use the iteration count to report throughput totals"]
    pub fn measure_for<F>(&mut self, duration: Duration, mut f: F) -> usize
    where
        F: FnMut(),
    {
        self.require_one_shot_timing_helper("measure_for");
        let allocation_start = allocation::snapshot();
        let start = Instant::now();
        let mut iterations = 0_usize;

        loop {
            f();
            iterations = iterations.saturating_add(1);

            if start.elapsed() >= duration {
                break;
            }
        }

        let elapsed = start.elapsed();
        let allocation_delta = allocation_start.map(allocation::delta_since);
        self.set_duration(elapsed);
        self.set_allocation_measurement(allocation_delta);
        self.set_successful_operations_if_unset(iterations as u64);
        iterations
    }

    /// Time an operation on a borrowed reference.
    pub fn measure_ref<F, T, R>(&mut self, target: &T, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        self.require_one_shot_timing_helper("measure_ref");
        let allocation_start = allocation::snapshot();
        let start = Instant::now();
        let result = f(target);
        let duration = start.elapsed();
        let allocation_delta = allocation_start.map(allocation::delta_since);
        self.set_duration(duration);
        self.set_allocation_measurement(allocation_delta);
        self.set_successful_operations_if_unset(1);
        result
    }

    /// Time an operation on a mutable reference.
    pub fn measure_mut<F, T, R>(&mut self, target: &mut T, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        self.require_one_shot_timing_helper("measure_mut");
        let allocation_start = allocation::snapshot();
        let start = Instant::now();
        let result = f(target);
        let duration = start.elapsed();
        let allocation_delta = allocation_start.map(allocation::delta_since);
        self.set_duration(duration);
        self.set_allocation_measurement(allocation_delta);
        self.set_successful_operations_if_unset(1);
        result
    }

    /// Manually record a duration for externally timed systems under test.
    pub fn record_duration(&mut self, duration: Duration) {
        self.require_one_shot_timing_helper("record_duration");
        self.set_duration(duration);
        self.set_successful_operations_if_unset(1);
    }

    /// Record externally measured work with explicit logical operation count.
    ///
    /// This is intended for systems that perform their own timing. Allocation
    /// counters remain unavailable because the framework did not bracket the
    /// measured workload.
    ///
    /// # Panics
    ///
    /// Panics in `mode = "micro"`, when timing was already recorded, or when
    /// `completed_operations` is zero.
    pub fn record_external(&mut self, duration: Duration, completed_operations: u64) -> &mut Self {
        self.require_non_micro_timing_helper("record_external");
        assert!(
            completed_operations != 0,
            "ctx.record_external() requires completed_operations > 0"
        );
        self.set_duration(duration);
        self.set_successful_operations(completed_operations);
        self
    }
}

fn calibrate_iterations<F, R>(target: Duration, f: &mut F) -> u64
where
    F: FnMut() -> R,
{
    let mut iterations = 1_u64;
    loop {
        let (elapsed, _) = time_operation_batch(iterations, f);
        if elapsed >= target || iterations >= 1 << 32 {
            return iterations;
        }

        let elapsed_ns = elapsed.as_nanos().max(1);
        let target_ns = target.as_nanos().max(1);
        let scale = (target_ns / elapsed_ns).clamp(2, 16);
        iterations = iterations.saturating_mul(u64::try_from(scale).unwrap_or(16));
        if iterations == 0 {
            return 1;
        }
    }
}

fn time_operation_batch<F, R>(iterations: u64, f: &mut F) -> (Duration, R)
where
    F: FnMut() -> R,
{
    let start = Instant::now();
    let mut result = None;
    for _ in 0..iterations {
        result = Some(std::hint::black_box(f()));
    }
    let elapsed = start.elapsed();
    (
        elapsed,
        std::hint::black_box(result.expect("micro batches always run at least once")),
    )
}

fn time_empty_batch(iterations: u64) -> Duration {
    let start = Instant::now();
    for index in 0..iterations {
        std::hint::black_box(index);
    }
    start.elapsed()
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
        StressContext::new(
            2,
            BenchmarkMode::FixedOperations {
                operations_per_sample: 3,
            },
        )
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
    fn measure_micro_records_calibrated_net_sample() {
        let mut ctx = StressContext::new(
            1,
            BenchmarkMode::Micro {
                target_sample_duration: Duration::from_millis(1),
            },
        );

        let result = ctx.measure_micro(|| std::hint::black_box(7_u64));

        let micro = ctx.micro.expect("micro measurement");
        assert_eq!(result, 7);
        assert!(micro.iterations > 0);
        assert_eq!(ctx.counters.attempted, micro.iterations);
        assert_eq!(ctx.counters.completed, micro.iterations);
        assert!(micro.gross_elapsed >= micro.net_elapsed);
        assert_eq!(ctx.duration, Some(micro.net_elapsed));
    }

    #[test]
    #[should_panic(expected = "ctx.measure_micro() requires mode = \"micro\"")]
    fn measure_micro_requires_micro_mode() {
        let mut ctx = ctx();

        ctx.measure_micro(|| std::hint::black_box(1_u64));
    }

    #[test]
    #[should_panic(expected = "ctx.operations() requires completed > 0")]
    fn operations_rejects_zero_completed_operations() {
        let mut ctx = ctx();

        ctx.operations(0);
    }

    #[test]
    fn operations_overrides_default_single_operation_count() {
        let mut ctx = ctx();

        let completed = ctx.measure(|| 256_u64);
        ctx.operations(completed);

        assert_eq!(ctx.operations_hint, Some(256));
        assert_eq!(ctx.counters.attempted, 256);
        assert_eq!(ctx.counters.completed, 256);
    }

    fn micro_ctx() -> StressContext {
        StressContext::new(
            1,
            BenchmarkMode::Micro {
                target_sample_duration: Duration::from_millis(1),
            },
        )
    }

    fn tier3_ctx() -> StressContext {
        StressContext::new(
            3,
            BenchmarkMode::FixedDuration {
                sample_duration: Duration::from_millis(1),
            },
        )
    }

    #[test]
    #[should_panic(
        expected = "ctx.measure() cannot be used with mode = \"micro\"; use ctx.measure_micro() or ctx.measure_workload()"
    )]
    fn measure_requires_non_micro_mode() {
        let mut ctx = micro_ctx();

        ctx.measure(|| std::hint::black_box(1_u64));
    }

    #[test]
    #[should_panic(
        expected = "ctx.measure_for() cannot be used with mode = \"micro\"; use ctx.measure_micro() or ctx.measure_workload()"
    )]
    fn measure_for_requires_non_micro_mode() {
        let mut ctx = micro_ctx();

        let _ = ctx.measure_for(Duration::from_millis(1), || {
            std::hint::black_box(1_u64);
        });
    }

    #[test]
    #[should_panic(
        expected = "ctx.measure_batch() cannot be used with mode = \"micro\"; use ctx.measure_micro() or ctx.measure_workload()"
    )]
    fn measure_batch_requires_non_micro_mode() {
        let mut ctx = micro_ctx();

        let _ = ctx.measure_batch(1, || {
            std::hint::black_box(1_u64);
        });
    }

    #[test]
    #[should_panic(
        expected = "ctx.measure_ref() cannot be used with mode = \"micro\"; use ctx.measure_micro() or ctx.measure_workload()"
    )]
    fn measure_ref_requires_non_micro_mode() {
        let mut ctx = micro_ctx();

        ctx.measure_ref(&1_u64, |value| std::hint::black_box(*value));
    }

    #[test]
    #[should_panic(
        expected = "ctx.measure_mut() cannot be used with mode = \"micro\"; use ctx.measure_micro() or ctx.measure_workload()"
    )]
    fn measure_mut_requires_non_micro_mode() {
        let mut ctx = micro_ctx();
        let mut value = 1_u64;

        ctx.measure_mut(&mut value, |value| {
            *value = value.saturating_add(1);
        });
    }

    #[test]
    #[should_panic(
        expected = "ctx.record_duration() cannot be used with mode = \"micro\"; use ctx.measure_micro() or ctx.measure_workload()"
    )]
    fn record_duration_requires_non_micro_mode() {
        let mut ctx = micro_ctx();

        ctx.record_duration(Duration::from_nanos(1));
    }

    #[test]
    #[should_panic(
        expected = "ctx.record_external() cannot be used with mode = \"micro\"; use ctx.measure_micro() or ctx.measure_workload()"
    )]
    fn record_external_requires_non_micro_mode() {
        let mut ctx = micro_ctx();

        ctx.record_external(Duration::from_nanos(1), 1);
    }

    #[test]
    fn measure_counted_records_returned_logical_operations() {
        let mut ctx = ctx();

        let completed = ctx.measure_counted(|| 256);

        assert_eq!(completed, 256);
        assert_eq!(ctx.operations_hint, Some(256));
        assert_eq!(ctx.counters.attempted, 256);
        assert_eq!(ctx.counters.completed, 256);
        assert!(ctx.duration.expect("duration") > Duration::ZERO);
    }

    #[test]
    #[should_panic(expected = "ctx.measure_counted() requires completed_operations > 0")]
    fn measure_counted_rejects_zero_completed_operations() {
        let mut ctx = ctx();

        let _ = ctx.measure_counted(|| 0);
    }

    #[test]
    #[should_panic(
        expected = "ctx.measure() cannot be used in Tier 3 fixed-duration benchmarks; use ctx.measure_batch(...), ctx.measure_workload(...), or ctx.record_external(...)"
    )]
    fn tier3_rejects_measure() {
        let mut ctx = tier3_ctx();

        ctx.measure(|| std::hint::black_box(1_u64));
    }

    #[test]
    #[should_panic(
        expected = "ctx.measure_ref() cannot be used in Tier 3 fixed-duration benchmarks; use ctx.measure_batch(...), ctx.measure_workload(...), or ctx.record_external(...)"
    )]
    fn tier3_rejects_measure_ref() {
        let mut ctx = tier3_ctx();

        ctx.measure_ref(&1_u64, |value| std::hint::black_box(*value));
    }

    #[test]
    #[should_panic(
        expected = "ctx.measure_mut() cannot be used in Tier 3 fixed-duration benchmarks; use ctx.measure_batch(...), ctx.measure_workload(...), or ctx.record_external(...)"
    )]
    fn tier3_rejects_measure_mut() {
        let mut ctx = tier3_ctx();
        let mut value = 1_u64;

        ctx.measure_mut(&mut value, |value| {
            *value = value.saturating_add(1);
        });
    }

    #[test]
    #[should_panic(
        expected = "ctx.measure_for() cannot be used in Tier 3 fixed-duration benchmarks; use ctx.measure_batch(...), ctx.measure_workload(...), or ctx.record_external(...)"
    )]
    fn tier3_rejects_measure_for() {
        let mut ctx = tier3_ctx();

        let _ = ctx.measure_for(Duration::from_millis(1), || {
            std::hint::black_box(1_u64);
        });
    }

    #[test]
    #[should_panic(
        expected = "ctx.record_duration() cannot be used in Tier 3 fixed-duration benchmarks; use ctx.measure_batch(...), ctx.measure_workload(...), or ctx.record_external(...)"
    )]
    fn tier3_rejects_record_duration() {
        let mut ctx = tier3_ctx();

        ctx.record_duration(Duration::from_nanos(1));
    }

    #[test]
    #[should_panic(
        expected = "ctx.measure_counted() cannot be used in Tier 3 fixed-duration benchmarks; use ctx.measure_batch(...), ctx.measure_workload(...), or ctx.record_external(...)"
    )]
    fn tier3_rejects_measure_counted() {
        let mut ctx = tier3_ctx();

        let _ = ctx.measure_counted(|| 1);
    }

    #[test]
    fn tier3_accepts_measure_workload_measure_batch_and_record_external() {
        let mut workload = tier3_ctx();
        let operations = workload.measure_workload(|| {
            std::hint::black_box(1_u64);
        });
        assert!(operations > 0);

        let mut batch = tier3_ctx();
        let completed = batch.measure_batch(8, || {
            std::hint::black_box(1_u64);
        });
        assert!(completed >= 8);
        assert_eq!(batch.operations_hint, Some(completed));

        let mut external = tier3_ctx();
        external.record_external(Duration::from_millis(1), 8);
        assert_eq!(external.operations_hint, Some(8));
    }

    #[test]
    fn measure_for_records_operation_hint() {
        let mut ctx = ctx();
        let iterations = ctx.measure_for(Duration::from_millis(1), || {
            std::hint::black_box(1_usize);
        });

        assert!(iterations > 0);
        assert_eq!(ctx.operations_hint, Some(iterations as u64));
        assert_eq!(ctx.counters.completed, iterations as u64);
    }

    #[test]
    #[should_panic(expected = "ctx.measure_batch() requires logical_operations_per_iteration > 0")]
    fn measure_batch_rejects_zero_logical_operations() {
        let mut ctx = ctx();

        let _ = ctx.measure_batch(0, || {
            std::hint::black_box(1_u64);
        });
    }

    #[test]
    fn measure_batch_records_fixed_operations_logical_counts() {
        let mut ctx = ctx();

        let completed = ctx.measure_batch(256, || {
            std::hint::black_box(1_u64);
        });

        assert_eq!(completed, 768);
        assert_eq!(ctx.operations_hint, Some(768));
        assert_eq!(ctx.counters.attempted, 768);
        assert_eq!(ctx.counters.completed, 768);
        assert!(ctx.duration.expect("duration") > Duration::ZERO);
    }

    #[test]
    fn measure_batch_records_fixed_duration_logical_counts() {
        let mut ctx = tier3_ctx();

        let completed = ctx.measure_batch(256, || {
            std::hint::black_box(1_u64);
        });

        assert!(completed >= 256);
        assert_eq!(completed % 256, 0);
        assert_eq!(ctx.operations_hint, Some(completed));
        assert_eq!(ctx.counters.attempted, completed);
        assert_eq!(ctx.counters.completed, completed);
        assert!(ctx.duration.expect("duration") > Duration::ZERO);
    }

    #[test]
    #[should_panic(expected = "ctx.record_external() requires completed_operations > 0")]
    fn record_external_rejects_zero_completed_operations() {
        let mut ctx = ctx();

        ctx.record_external(Duration::from_nanos(1), 0);
    }

    #[test]
    fn record_external_records_duration_and_logical_counts_without_allocation_measurement() {
        let mut ctx = ctx();

        ctx.record_external(Duration::from_millis(2), 500);

        assert_eq!(ctx.duration, Some(Duration::from_millis(2)));
        assert_eq!(ctx.operations_hint, Some(500));
        assert_eq!(ctx.counters.attempted, 500);
        assert_eq!(ctx.counters.completed, 500);
        assert!(ctx.allocation.is_none());
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
