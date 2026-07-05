//! Benchmark context for named measurements, workload facts, and correctness counters.

use crate::allocation;
use crate::artifact::{BenchmarkMode, CorrectnessCounters, MeasurementIntent};
use std::collections::BTreeMap;
use std::future::Future;
use std::time::{Duration, Instant};

/// Context passed to benchmark functions.
///
/// A benchmark function may record one or more named measurements. Each named
/// measurement becomes its own benchmark row.
pub struct StressContext {
    pub(crate) mode: BenchmarkMode,
    pub(crate) measurements: Vec<MeasurementRecord>,
    parameters: BTreeMap<String, String>,
    metadata: BTreeMap<String, String>,
    pending_latency_ns: Vec<u128>,
    pending_counters: CorrectnessCounters,
    pending_has_counters: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct MeasurementRecord {
    pub name: String,
    pub intent: MeasurementIntent,
    pub mode: BenchmarkMode,
    pub duration: Duration,
    pub latency_ns: Vec<u128>,
    pub counters: CorrectnessCounters,
    pub operations_hint: Option<u64>,
    pub micro: Option<MicroMeasurement>,
    pub allocation: Option<AllocationMeasurement>,
    pub parameters: BTreeMap<String, String>,
    pub metadata: BTreeMap<String, String>,
    pub overrides: MeasurementOverrides,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MeasurementOverrides {
    pub samples: Option<usize>,
    pub warmup_samples: Option<usize>,
    pub cooldown_samples: Option<usize>,
}

impl MeasurementOverrides {
    pub fn target_for_phase(self, phase: crate::artifact::SamplePhase, default: usize) -> usize {
        match phase {
            crate::artifact::SamplePhase::Warmup => self.warmup_samples.unwrap_or(default),
            crate::artifact::SamplePhase::Measured => self.samples.unwrap_or(default),
            crate::artifact::SamplePhase::Cooldown => self.cooldown_samples.unwrap_or(default),
        }
    }
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

#[derive(Debug, Clone, Copy)]
struct MeasurementState {
    duration: Duration,
    counters: CorrectnessCounters,
    operations_hint: Option<u64>,
    micro: Option<MicroMeasurement>,
    allocation: Option<AllocationMeasurement>,
}

impl StressContext {
    pub(crate) fn new(_tier: u32, mode: BenchmarkMode) -> Self {
        Self {
            mode,
            measurements: Vec::new(),
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
            pending_latency_ns: Vec::new(),
            pending_counters: CorrectnessCounters::default(),
            pending_has_counters: false,
        }
    }

    /// Add a structured workload parameter, such as `client_count`,
    /// `payload_size`, `transport`, `operation`, or `scenario`.
    #[allow(clippy::needless_pass_by_value)]
    pub fn parameter(&mut self, key: impl Into<String>, value: impl ToString) -> &mut Self {
        let key = key.into();
        let value = value.to_string();
        self.parameters.insert(key.clone(), value.clone());
        if let Some(record) = self.measurements.last_mut() {
            record.parameters.insert(key, value);
        }
        self
    }

    /// Add descriptive benchmark metadata.
    #[allow(clippy::needless_pass_by_value)]
    pub fn metadata(&mut self, key: impl Into<String>, value: impl ToString) -> &mut Self {
        let key = key.into();
        let value = value.to_string();
        self.metadata.insert(key.clone(), value.clone());
        if let Some(record) = self.measurements.last_mut() {
            record.metadata.insert(key, value);
        }
        self
    }

    /// Record one latency observation for the latest measurement, or the next
    /// measurement if none has been recorded yet.
    pub fn record_latency(&mut self, duration: Duration) -> &mut Self {
        if let Some(record) = self.measurements.last_mut() {
            record.latency_ns.push(duration.as_nanos());
        } else {
            self.pending_latency_ns.push(duration.as_nanos());
        }
        self
    }

    /// Record canonical correctness counters for the latest measurement, or
    /// the next measurement if none has been recorded yet.
    #[must_use]
    pub fn correctness(&mut self) -> CorrectnessRecorder<'_> {
        if let Some(record) = self.measurements.last_mut() {
            CorrectnessRecorder {
                counters: &mut record.counters,
                touched: None,
            }
        } else {
            CorrectnessRecorder {
                counters: &mut self.pending_counters,
                touched: Some(&mut self.pending_has_counters),
            }
        }
    }

    /// Record the number of successful logical operations for the latest
    /// measurement, or for the next measurement if none has been recorded yet.
    ///
    /// # Panics
    ///
    /// Panics when `completed` is zero.
    pub fn operations(&mut self, completed: u64) -> &mut Self {
        assert!(completed != 0, "ctx.operations() requires completed > 0");
        if let Some(record) = self.measurements.last_mut() {
            set_successful_operations(&mut record.counters, completed);
            record.operations_hint = Some(completed);
        } else {
            set_successful_operations(&mut self.pending_counters, completed);
            self.pending_has_counters = true;
        }
        self
    }

    /// Start an advanced benchmark builder.
    pub fn benchmark(&mut self, name: impl Into<String>) -> BenchmarkBuilder<'_> {
        BenchmarkBuilder {
            ctx: self,
            name: name.into(),
            overrides: MeasurementOverrides::default(),
            intent: MeasurementIntent::General,
        }
    }

    /// Measure named work using the tier-derived default strategy.
    pub fn measure<F, R>(&mut self, name: impl Into<String>, f: F) -> R
    where
        F: FnMut() -> R,
    {
        self.measure_with_intent(
            name,
            MeasurementIntent::General,
            MeasurementOverrides::default(),
            f,
        )
    }

    /// Measure named batched work and record logical operation counts.
    ///
    /// # Panics
    ///
    /// Panics when `logical_operations_per_iteration` is zero.
    pub fn measure_batch<F, R>(
        &mut self,
        name: impl Into<String>,
        logical_operations_per_iteration: u64,
        f: F,
    ) -> u64
    where
        F: FnMut() -> R,
    {
        self.measure_batch_with_overrides(
            name,
            logical_operations_per_iteration,
            MeasurementIntent::Batch,
            MeasurementOverrides::default(),
            f,
        )
    }

    /// Measure named async work using the tier-derived default strategy.
    pub async fn measure_async<F, Fut, R>(&mut self, name: impl Into<String>, f: F) -> R
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = R>,
    {
        self.measure_async_with_overrides(
            name,
            MeasurementIntent::Async,
            MeasurementOverrides::default(),
            f,
        )
        .await
    }

    /// Measure named threaded work and tag it with threaded intent.
    pub fn measure_threaded<F, R>(&mut self, name: impl Into<String>, f: F) -> R
    where
        F: FnMut() -> R,
    {
        self.measure_with_intent(
            name,
            MeasurementIntent::Threaded,
            MeasurementOverrides::default(),
            f,
        )
    }

    /// Measure named pipeline work and tag it with pipeline intent.
    pub fn measure_pipeline<F, R>(&mut self, name: impl Into<String>, f: F) -> R
    where
        F: FnMut() -> R,
    {
        self.measure_with_intent(
            name,
            MeasurementIntent::Pipeline,
            MeasurementOverrides::default(),
            f,
        )
    }

    /// Measure named I/O work and tag it with I/O intent.
    pub fn measure_io<F, R>(&mut self, name: impl Into<String>, f: F) -> R
    where
        F: FnMut() -> R,
    {
        self.measure_with_intent(
            name,
            MeasurementIntent::Io,
            MeasurementOverrides::default(),
            f,
        )
    }

    /// Record externally measured named work.
    ///
    /// # Panics
    ///
    /// Panics when `completed_operations` is zero.
    pub fn record_external(
        &mut self,
        name: impl Into<String>,
        duration: Duration,
        completed_operations: u64,
    ) -> &mut Self {
        assert!(
            completed_operations != 0,
            "ctx.record_external() requires completed_operations > 0"
        );
        let mut counters = CorrectnessCounters::default();
        set_successful_operations(&mut counters, completed_operations);
        let state = MeasurementState {
            duration,
            counters,
            operations_hint: Some(completed_operations),
            micro: None,
            allocation: None,
        };
        self.push_measurement(
            name.into(),
            MeasurementIntent::External,
            state,
            MeasurementOverrides::default(),
        );
        self
    }

    pub(crate) fn take_measurements(self) -> Vec<MeasurementRecord> {
        self.measurements
    }

    fn measure_with_intent<F, R>(
        &mut self,
        name: impl Into<String>,
        intent: MeasurementIntent,
        overrides: MeasurementOverrides,
        mut f: F,
    ) -> R
    where
        F: FnMut() -> R,
    {
        let (state, result) = self.time_sync_workload(intent, &mut f);
        self.push_measurement(name.into(), intent, state, overrides);
        result
    }

    fn measure_batch_with_overrides<F, R>(
        &mut self,
        name: impl Into<String>,
        logical_operations_per_iteration: u64,
        intent: MeasurementIntent,
        overrides: MeasurementOverrides,
        mut f: F,
    ) -> u64
    where
        F: FnMut() -> R,
    {
        assert!(
            logical_operations_per_iteration != 0,
            "ctx.measure_batch() requires logical_operations_per_iteration > 0"
        );
        let (mut state, _) = self.time_sync_workload(intent, &mut f);
        let iterations = state.operations_hint.unwrap_or_default();
        let completed = iterations.saturating_mul(logical_operations_per_iteration);
        set_successful_operations(&mut state.counters, completed);
        state.operations_hint = Some(completed);
        self.push_measurement(name.into(), intent, state, overrides);
        completed
    }

    async fn measure_async_with_overrides<F, Fut, R>(
        &mut self,
        name: impl Into<String>,
        intent: MeasurementIntent,
        overrides: MeasurementOverrides,
        mut f: F,
    ) -> R
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = R>,
    {
        let (state, result) = self.time_async_workload(&mut f).await;
        self.push_measurement(name.into(), intent, state, overrides);
        result
    }

    fn time_sync_workload<F, R>(
        &self,
        intent: MeasurementIntent,
        f: &mut F,
    ) -> (MeasurementState, R)
    where
        F: FnMut() -> R,
    {
        match &self.mode {
            BenchmarkMode::Micro {
                target_sample_duration,
            } => Self::time_micro(*target_sample_duration, f),
            BenchmarkMode::FixedDuration { sample_duration } => {
                Self::time_fixed_duration(*sample_duration, intent, f)
            }
            BenchmarkMode::FixedOperations {
                operations_per_sample,
            } => Self::time_fixed_operations(*operations_per_sample, f),
        }
    }

    fn time_micro<F, R>(target: Duration, f: &mut F) -> (MeasurementState, R)
    where
        F: FnMut() -> R,
    {
        let iterations = calibrate_iterations(target, f);
        let allocation_start = allocation::snapshot();
        let (gross_elapsed, result) = time_operation_batch(iterations, f);
        let allocation_delta = allocation_start.map(allocation::delta_since);
        let overhead = time_empty_batch(iterations);
        let net_elapsed = gross_elapsed.saturating_sub(overhead);
        let mut counters = CorrectnessCounters::default();
        set_successful_operations_if_unset(&mut counters, iterations);
        (
            MeasurementState {
                duration: net_elapsed,
                counters,
                operations_hint: Some(iterations),
                micro: Some(MicroMeasurement {
                    iterations,
                    gross_elapsed,
                    overhead,
                    net_elapsed,
                }),
                allocation: allocation_delta.map(allocation_measurement),
            },
            result,
        )
    }

    fn time_fixed_duration<F, R>(
        sample_duration: Duration,
        intent: MeasurementIntent,
        f: &mut F,
    ) -> (MeasurementState, R)
    where
        F: FnMut() -> R,
    {
        let allocation_start = allocation::snapshot();
        let start = Instant::now();
        let mut operations = 1_u64;
        let mut result = Some(std::hint::black_box(f()));
        loop {
            if start.elapsed() >= sample_duration {
                break;
            }
            if intent == MeasurementIntent::Io {
                std::thread::yield_now();
            }
            result = Some(std::hint::black_box(f()));
            operations = operations.saturating_add(1);
        }
        let duration = start.elapsed();
        let allocation_delta = allocation_start.map(allocation::delta_since);
        let mut counters = CorrectnessCounters::default();
        set_successful_operations_if_unset(&mut counters, operations);
        (
            MeasurementState {
                duration,
                counters,
                operations_hint: Some(operations),
                micro: None,
                allocation: allocation_delta.map(allocation_measurement),
            },
            std::hint::black_box(result.expect("fixed-duration measurement runs at least once")),
        )
    }

    fn time_fixed_operations<F, R>(operations_per_sample: u64, f: &mut F) -> (MeasurementState, R)
    where
        F: FnMut() -> R,
    {
        let allocation_start = allocation::snapshot();
        let start = Instant::now();
        let mut result = None;
        for _ in 0..operations_per_sample {
            result = Some(std::hint::black_box(f()));
        }
        let duration = start.elapsed();
        let allocation_delta = allocation_start.map(allocation::delta_since);
        let mut counters = CorrectnessCounters::default();
        set_successful_operations_if_unset(&mut counters, operations_per_sample);
        (
            MeasurementState {
                duration,
                counters,
                operations_hint: Some(operations_per_sample),
                micro: None,
                allocation: allocation_delta.map(allocation_measurement),
            },
            std::hint::black_box(result.expect("fixed-operation measurement runs at least once")),
        )
    }

    async fn time_async_workload<F, Fut, R>(&self, f: &mut F) -> (MeasurementState, R)
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = R>,
    {
        match &self.mode {
            BenchmarkMode::Micro {
                target_sample_duration,
            } => self.time_async_duration(*target_sample_duration, f).await,
            BenchmarkMode::FixedDuration { sample_duration } => {
                self.time_async_duration(*sample_duration, f).await
            }
            BenchmarkMode::FixedOperations {
                operations_per_sample,
            } => self.time_async_operations(*operations_per_sample, f).await,
        }
    }

    async fn time_async_duration<F, Fut, R>(
        &self,
        sample_duration: Duration,
        f: &mut F,
    ) -> (MeasurementState, R)
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = R>,
    {
        let allocation_start = allocation::snapshot();
        let start = Instant::now();
        let mut operations = 1_u64;
        let mut result = Some(std::hint::black_box(f().await));
        loop {
            if start.elapsed() >= sample_duration {
                break;
            }
            result = Some(std::hint::black_box(f().await));
            operations = operations.saturating_add(1);
        }
        let duration = start.elapsed();
        let allocation_delta = allocation_start.map(allocation::delta_since);
        let mut counters = CorrectnessCounters::default();
        set_successful_operations_if_unset(&mut counters, operations);
        let micro = matches!(&self.mode, BenchmarkMode::Micro { .. }).then_some(MicroMeasurement {
            iterations: operations,
            gross_elapsed: duration,
            overhead: Duration::ZERO,
            net_elapsed: duration,
        });
        (
            MeasurementState {
                duration,
                counters,
                operations_hint: Some(operations),
                micro,
                allocation: allocation_delta.map(allocation_measurement),
            },
            std::hint::black_box(result.expect("async duration measurement runs at least once")),
        )
    }

    async fn time_async_operations<F, Fut, R>(
        &self,
        operations_per_sample: u64,
        f: &mut F,
    ) -> (MeasurementState, R)
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = R>,
    {
        let allocation_start = allocation::snapshot();
        let start = Instant::now();
        let mut result = None;
        for _ in 0..operations_per_sample {
            result = Some(std::hint::black_box(f().await));
        }
        let duration = start.elapsed();
        let allocation_delta = allocation_start.map(allocation::delta_since);
        let mut counters = CorrectnessCounters::default();
        set_successful_operations_if_unset(&mut counters, operations_per_sample);
        (
            MeasurementState {
                duration,
                counters,
                operations_hint: Some(operations_per_sample),
                micro: None,
                allocation: allocation_delta.map(allocation_measurement),
            },
            std::hint::black_box(result.expect("async operation measurement runs at least once")),
        )
    }

    fn push_measurement(
        &mut self,
        name: String,
        intent: MeasurementIntent,
        mut state: MeasurementState,
        overrides: MeasurementOverrides,
    ) {
        assert!(!name.is_empty(), "measurement name cannot be empty");
        if self.pending_has_counters {
            state.counters = self.pending_counters;
            state.operations_hint = Some(state.counters.completed);
            self.pending_counters = CorrectnessCounters::default();
            self.pending_has_counters = false;
        }
        let latency_ns = std::mem::take(&mut self.pending_latency_ns);
        self.measurements.push(MeasurementRecord {
            name,
            intent,
            mode: self.mode.clone(),
            duration: state.duration,
            latency_ns,
            counters: state.counters,
            operations_hint: state.operations_hint,
            micro: state.micro,
            allocation: state.allocation,
            parameters: self.parameters.clone(),
            metadata: self.metadata.clone(),
            overrides,
        });
    }
}

/// Builder for advanced per-measurement overrides.
pub struct BenchmarkBuilder<'a> {
    ctx: &'a mut StressContext,
    name: String,
    overrides: MeasurementOverrides,
    intent: MeasurementIntent,
}

impl BenchmarkBuilder<'_> {
    /// Override measured sample count for this measurement.
    #[must_use]
    pub const fn samples(mut self, value: usize) -> Self {
        self.overrides.samples = Some(value);
        self
    }

    /// Override warmup sample count for this measurement.
    #[must_use]
    pub const fn warmup(mut self, value: usize) -> Self {
        self.overrides.warmup_samples = Some(value);
        self
    }

    /// Override cooldown sample count for this measurement.
    #[must_use]
    pub const fn cooldown(mut self, value: usize) -> Self {
        self.overrides.cooldown_samples = Some(value);
        self
    }

    /// Set the measurement intent.
    #[must_use]
    pub const fn intent(mut self, intent: MeasurementIntent) -> Self {
        self.intent = intent;
        self
    }

    /// Measure using the builder overrides.
    pub fn measure<F, R>(self, f: F) -> R
    where
        F: FnMut() -> R,
    {
        self.ctx
            .measure_with_intent(self.name, self.intent, self.overrides, f)
    }

    /// Measure batched work using the builder overrides.
    pub fn measure_batch<F, R>(self, logical_operations_per_iteration: u64, f: F) -> u64
    where
        F: FnMut() -> R,
    {
        self.ctx.measure_batch_with_overrides(
            self.name,
            logical_operations_per_iteration,
            MeasurementIntent::Batch,
            self.overrides,
            f,
        )
    }

    /// Measure async work using the builder overrides.
    pub async fn measure_async<F, Fut, R>(self, f: F) -> R
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = R>,
    {
        self.ctx
            .measure_async_with_overrides(self.name, MeasurementIntent::Async, self.overrides, f)
            .await
    }
}

fn allocation_measurement(delta: allocation::AllocationDelta) -> AllocationMeasurement {
    AllocationMeasurement {
        allocs: delta.allocs,
        bytes: delta.bytes,
    }
}

fn set_successful_operations_if_unset(counters: &mut CorrectnessCounters, operations: u64) {
    if counters.attempted == 0 && counters.completed == 0 {
        counters.attempted = operations;
        counters.completed = operations;
    }
}

fn set_successful_operations(counters: &mut CorrectnessCounters, operations: u64) {
    counters.attempted = operations;
    counters.completed = operations;
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
        std::hint::black_box(result.expect("measurement batches always run at least once")),
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
    touched: Option<&'a mut bool>,
}

impl CorrectnessRecorder<'_> {
    /// Set attempted operations.
    #[must_use]
    pub fn attempted(self, value: u64) -> Self {
        self.counters.attempted = value;
        self.mark_touched()
    }

    /// Set completed operations.
    #[must_use]
    pub fn completed(self, value: u64) -> Self {
        self.counters.completed = value;
        self.mark_touched()
    }

    /// Set failed operations.
    #[must_use]
    pub fn failures(self, value: u64) -> Self {
        self.counters.failures = value;
        self.mark_touched()
    }

    /// Set timed out operations.
    #[must_use]
    pub fn timeouts(self, value: u64) -> Self {
        self.counters.timeouts = value;
        self.mark_touched()
    }

    /// Set duplicate operations/results.
    #[must_use]
    pub fn duplicates(self, value: u64) -> Self {
        self.counters.duplicates = value;
        self.mark_touched()
    }

    /// Set dropped operations/results.
    #[must_use]
    pub fn dropped(self, value: u64) -> Self {
        self.counters.dropped = value;
        self.mark_touched()
    }

    /// Set validation errors.
    #[must_use]
    pub fn validation_errors(self, value: u64) -> Self {
        self.counters.validation_errors = value;
        self.mark_touched()
    }

    fn mark_touched(mut self) -> Self {
        if let Some(touched) = self.touched.as_deref_mut() {
            *touched = true;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_ops_ctx(operations_per_sample: u64) -> StressContext {
        StressContext::new(
            2,
            BenchmarkMode::FixedOperations {
                operations_per_sample,
            },
        )
    }

    #[test]
    fn measure_records_named_fixed_operation_measurement() {
        let mut ctx = fixed_ops_ctx(3);

        let result = ctx.measure("lookup", || std::hint::black_box(7_u64));

        let records = ctx.take_measurements();
        assert_eq!(result, 7);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "lookup");
        assert_eq!(records[0].counters.completed, 3);
        assert!(records[0].duration > Duration::ZERO);
    }

    #[test]
    fn multiple_named_measurements_are_distinct() {
        let mut ctx = fixed_ops_ctx(1);

        ctx.measure("read", || std::hint::black_box(1_u64));
        ctx.measure("write", || std::hint::black_box(2_u64));

        let names = ctx
            .take_measurements()
            .into_iter()
            .map(|record| record.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["read", "write"]);
    }

    #[test]
    fn measure_batch_records_logical_operation_count() {
        let mut ctx = fixed_ops_ctx(4);

        let completed = ctx.measure_batch("flush", 256, || std::hint::black_box(1_u64));

        let record = ctx.take_measurements().remove(0);
        assert_eq!(completed, 1024);
        assert_eq!(record.intent, MeasurementIntent::Batch);
        assert_eq!(record.counters.completed, 1024);
    }

    #[test]
    fn measure_batch_excludes_setup_by_accepting_prebuilt_inputs() {
        let setup = [1_u64, 2, 3];
        let mut ctx = fixed_ops_ctx(1);

        let completed = ctx.measure_batch("sum", setup.len() as u64, || {
            std::hint::black_box(setup.iter().sum::<u64>());
        });

        assert_eq!(completed, 3);
        assert_eq!(ctx.take_measurements()[0].counters.completed, 3);
    }

    #[test]
    fn tier1_measure_records_calibrated_net_sample() {
        let mut ctx = StressContext::new(
            1,
            BenchmarkMode::Micro {
                target_sample_duration: Duration::from_millis(1),
            },
        );

        let result = ctx.measure("hot_path", || std::hint::black_box(7_u64));

        let record = ctx.take_measurements().remove(0);
        let micro = record.micro.expect("micro measurement");
        assert_eq!(result, 7);
        assert!(micro.iterations > 0);
        assert_eq!(record.counters.completed, micro.iterations);
        assert!(micro.gross_elapsed >= micro.net_elapsed);
        assert_eq!(record.duration, micro.net_elapsed);
    }

    #[test]
    fn fixed_duration_measure_repeats_until_window() {
        let mut ctx = StressContext::new(
            3,
            BenchmarkMode::FixedDuration {
                sample_duration: Duration::from_millis(1),
            },
        );

        ctx.measure("poll", || std::hint::black_box(1_u64));

        let record = ctx.take_measurements().remove(0);
        assert!(record.counters.completed > 0);
        assert!(record.duration >= Duration::from_millis(1));
    }

    #[test]
    fn intent_methods_tag_measurements() {
        let mut ctx = fixed_ops_ctx(1);

        ctx.measure_threaded("workers", || {});
        ctx.measure_pipeline("pipe", || {});
        ctx.measure_io("read", || {});

        let intents = ctx
            .take_measurements()
            .into_iter()
            .map(|record| record.intent)
            .collect::<Vec<_>>();
        assert_eq!(
            intents,
            vec![
                MeasurementIntent::Threaded,
                MeasurementIntent::Pipeline,
                MeasurementIntent::Io
            ]
        );
    }

    #[test]
    fn measure_async_records_async_work() {
        let mut ctx = fixed_ops_ctx(1);

        let result = crate::__private::block_on(
            ctx.measure_async("ready future", || async { std::hint::black_box(11_u64) }),
        );

        let record = ctx.take_measurements().remove(0);
        assert_eq!(result, 11);
        assert_eq!(record.intent, MeasurementIntent::Async);
        assert_eq!(record.counters.completed, 1);
        assert!(record.duration > Duration::ZERO);
    }

    #[test]
    fn external_measurement_records_explicit_duration_and_operations() {
        let mut ctx = fixed_ops_ctx(1);

        ctx.record_external("remote", Duration::from_millis(10), 500);

        let record = ctx.take_measurements().remove(0);
        assert_eq!(record.duration, Duration::from_millis(10));
        assert_eq!(record.counters.completed, 500);
        assert_eq!(record.intent, MeasurementIntent::External);
    }

    #[test]
    fn builder_records_overrides_before_measurement() {
        let mut ctx = fixed_ops_ctx(1);

        ctx.benchmark("lookup")
            .samples(7)
            .warmup(2)
            .cooldown(1)
            .measure(|| std::hint::black_box(1_u64));

        let record = ctx.take_measurements().remove(0);
        assert_eq!(record.overrides.samples, Some(7));
        assert_eq!(record.overrides.warmup_samples, Some(2));
        assert_eq!(record.overrides.cooldown_samples, Some(1));
    }

    #[test]
    fn operations_and_correctness_update_latest_measurement() {
        let mut ctx = fixed_ops_ctx(1);

        let completed = ctx.measure("batch", || 64_u64);
        ctx.operations(completed);
        let _ = ctx.correctness().attempted(64).completed(63).failures(1);

        let record = ctx.take_measurements().remove(0);
        assert_eq!(record.counters.attempted, 64);
        assert_eq!(record.counters.completed, 63);
        assert_eq!(record.counters.failures, 1);
    }

    #[test]
    fn pending_latency_and_correctness_apply_to_next_measurement() {
        let mut ctx = fixed_ops_ctx(1);

        ctx.record_latency(Duration::from_micros(5));
        let _ = ctx.correctness().attempted(10).completed(10);
        ctx.measure("later", || {});

        let record = ctx.take_measurements().remove(0);
        assert_eq!(record.latency_ns, vec![5_000]);
        assert_eq!(record.counters.attempted, 10);
        assert_eq!(record.counters.completed, 10);
    }

    #[test]
    #[should_panic(expected = "ctx.measure_batch() requires logical_operations_per_iteration > 0")]
    fn measure_batch_rejects_zero_logical_operations() {
        let mut ctx = fixed_ops_ctx(1);
        let _ = ctx.measure_batch("bad", 0, || {});
    }

    #[test]
    #[should_panic(expected = "measurement name cannot be empty")]
    fn measure_rejects_empty_names() {
        let mut ctx = fixed_ops_ctx(1);
        ctx.measure("", || {});
    }
}
