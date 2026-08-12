//! Benchmark context for named measurements, workload facts, and correctness counters.

use crate::allocation;
use crate::artifact::{
    BenchmarkMode, CorrectnessCounters, MeasurementIntent, ObservationDirection, ObservationUnit,
    ScalarObservation, TrustClass,
};
use std::collections::BTreeMap;
use std::fmt;
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
    pending_observations: Vec<ScalarObservation>,
    pending_counters: CorrectnessCounters,
    pending_has_counters: bool,
}

/// Stable name for the logical operation represented by a measurement.
///
/// Batch and externally driven measurements should use a logical unit so that
/// throughput, correctness counters, and baselines all describe the same work.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LogicalUnit(String);

impl LogicalUnit {
    /// Create a logical unit such as `request`, `record`, or `transaction`.
    ///
    /// # Panics
    ///
    /// Panics when the unit is empty or contains only whitespace.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        assert!(!value.trim().is_empty(), "logical unit cannot be empty");
        Self(value)
    }

    /// Return the canonical string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LogicalUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Observed logical-operation outcome for one workload invocation.
///
/// Unlike `measure_batch`, these counters are supplied by the workload rather
/// than inferred from the requested iteration count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OperationOutcome {
    /// Logical operations attempted.
    pub attempted: u64,
    /// Logical operations completed successfully.
    pub completed: u64,
    /// Logical operations that failed.
    pub failures: u64,
    /// Logical operations that timed out.
    pub timeouts: u64,
    /// Duplicate operations or results.
    pub duplicates: u64,
    /// Dropped operations or results.
    pub dropped: u64,
    /// Results that failed validation.
    pub validation_errors: u64,
}

impl OperationOutcome {
    /// Create an observed outcome.
    ///
    /// # Panics
    ///
    /// Panics when completed work exceeds attempted work.
    #[must_use]
    pub fn new(attempted: u64, completed: u64) -> Self {
        assert!(
            completed <= attempted,
            "completed operations cannot exceed attempted operations"
        );
        Self {
            attempted,
            completed,
            ..Self::default()
        }
    }

    /// Create a fully successful outcome.
    #[must_use]
    pub fn success(completed: u64) -> Self {
        Self::new(completed, completed)
    }

    /// Set failed operations.
    #[must_use]
    pub const fn failures(mut self, value: u64) -> Self {
        self.failures = value;
        self
    }

    /// Set timed-out operations.
    #[must_use]
    pub const fn timeouts(mut self, value: u64) -> Self {
        self.timeouts = value;
        self
    }

    /// Set duplicate operations or results.
    #[must_use]
    pub const fn duplicates(mut self, value: u64) -> Self {
        self.duplicates = value;
        self
    }

    /// Set dropped operations or results.
    #[must_use]
    pub const fn dropped(mut self, value: u64) -> Self {
        self.dropped = value;
        self
    }

    /// Set validation errors.
    #[must_use]
    pub const fn validation_errors(mut self, value: u64) -> Self {
        self.validation_errors = value;
        self
    }

    fn accumulate(&mut self, other: Self) {
        assert!(
            other.completed <= other.attempted,
            "completed operations cannot exceed attempted operations"
        );
        self.attempted = self.attempted.saturating_add(other.attempted);
        self.completed = self.completed.saturating_add(other.completed);
        self.failures = self.failures.saturating_add(other.failures);
        self.timeouts = self.timeouts.saturating_add(other.timeouts);
        self.duplicates = self.duplicates.saturating_add(other.duplicates);
        self.dropped = self.dropped.saturating_add(other.dropped);
        self.validation_errors = self
            .validation_errors
            .saturating_add(other.validation_errors);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MeasurementRecord {
    pub name: String,
    pub intent: MeasurementIntent,
    pub mode: BenchmarkMode,
    pub duration: Duration,
    pub latency_ns: Vec<u128>,
    pub observations: Vec<ScalarObservation>,
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

#[derive(Debug, Clone, Copy, Default)]
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

struct FallibleProgress<R, E> {
    attempted: u64,
    completed: u64,
    last_success: Option<R>,
    error: Option<E>,
}

impl<R, E> Default for FallibleProgress<R, E> {
    fn default() -> Self {
        Self {
            attempted: 0,
            completed: 0,
            last_success: None,
            error: None,
        }
    }
}

impl<R, E> FallibleProgress<R, E> {
    fn observe(&mut self, result: Result<R, E>) -> bool {
        self.attempted = self.attempted.saturating_add(1);
        match result {
            Ok(output) => {
                self.completed = self.completed.saturating_add(1);
                self.last_success = Some(output);
                true
            }
            Err(error) => {
                self.error = Some(error);
                false
            }
        }
    }

    fn finish(self) -> (CorrectnessCounters, Result<R, E>) {
        let counters = CorrectnessCounters {
            attempted: self.attempted,
            completed: self.completed,
            failures: u64::from(self.error.is_some()),
            ..CorrectnessCounters::default()
        };
        let result = match self.error {
            Some(error) => Err(error),
            None => Ok(self
                .last_success
                .expect("fallible measurement runs at least once")),
        };
        (counters, result)
    }
}

impl StressContext {
    pub(crate) fn new(_tier: u32, mode: BenchmarkMode) -> Self {
        Self {
            mode,
            measurements: Vec::new(),
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
            pending_latency_ns: Vec::new(),
            pending_observations: Vec::new(),
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
    ///
    /// # Panics
    ///
    /// Panics when `key` is `trust_class`; use a typed benchmark role instead.
    #[allow(clippy::needless_pass_by_value)]
    pub fn metadata(&mut self, key: impl Into<String>, value: impl ToString) -> &mut Self {
        let key = key.into();
        assert_authorable_metadata_key(&key);
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

    /// Record one finite scalar observation for the latest measurement, or the
    /// next measurement if none has been recorded yet.
    ///
    /// Every invocation must record the same names, units, and directions.
    ///
    /// # Panics
    ///
    /// Panics for an empty name, a non-finite value, or a duplicate name on
    /// the same measurement.
    pub fn record_observation(
        &mut self,
        name: impl Into<String>,
        value: f64,
        unit: ObservationUnit,
        direction: ObservationDirection,
    ) -> &mut Self {
        let observation = ScalarObservation {
            name: name.into(),
            value,
            unit,
            direction,
        };
        assert!(
            !observation.name.trim().is_empty(),
            "observation name cannot be empty"
        );
        assert!(
            observation.value.is_finite(),
            "observation value must be finite"
        );
        let observations = self
            .measurements
            .last_mut()
            .map_or(&mut self.pending_observations, |record| {
                &mut record.observations
            });
        assert!(
            observations
                .iter()
                .all(|existing| existing.name != observation.name),
            "duplicate observation name {:?}",
            observation.name
        );
        observations.push(observation);
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
            operations_per_sample: None,
            intent: MeasurementIntent::General,
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
        }
    }

    /// Measure infallible named work using the tier-derived default strategy.
    ///
    /// Repeated modes return only the final closure value. Use
    /// [`Self::measure_result`] when the operation can fail so an earlier error
    /// cannot be hidden by a later success.
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

    /// Measure fallible named work and stop the sample at the first error.
    ///
    /// Every attempted call is counted. Successful calls increment completed
    /// operations, and the first error increments failures before being
    /// returned to the benchmark function.
    ///
    /// # Errors
    ///
    /// Returns the first error produced by the measured operation. Tier 1
    /// calibration errors are also returned and recorded as failed evidence.
    pub fn measure_result<F, R, E>(&mut self, name: impl Into<String>, f: F) -> Result<R, E>
    where
        F: FnMut() -> Result<R, E>,
    {
        self.measure_result_with_overrides(
            name.into(),
            MeasurementIntent::General,
            MeasurementOverrides::default(),
            f,
        )
    }

    /// Measure infallible named batched work and infer logical operation counts.
    ///
    /// Closure return values are discarded. Use [`Self::measure_result`] (or
    /// its builder equivalent for a row-local operation count) when one
    /// operation can fail, and [`Self::measure_outcome`] when one invocation
    /// can report partial success.
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

    /// Measure work whose logical-operation outcome is observed by the
    /// workload rather than inferred from the iteration count.
    ///
    /// The returned value is the aggregate outcome for the measured sample.
    /// Microbenchmark calibration invocations are intentionally excluded.
    ///
    /// # Panics
    ///
    /// Panics when the name or logical unit is empty, or when an invocation
    /// reports more completed operations than attempted operations.
    #[allow(clippy::needless_pass_by_value)]
    pub fn measure_outcome<F>(
        &mut self,
        name: impl Into<String>,
        logical_unit: LogicalUnit,
        f: F,
    ) -> OperationOutcome
    where
        F: FnMut() -> OperationOutcome,
    {
        self.measure_outcome_with_overrides(name, &logical_unit, MeasurementOverrides::default(), f)
    }

    /// Measure observed outcomes with fresh input for every invocation.
    ///
    /// Setup and output destruction are excluded from timing. Unlike
    /// [`Self::measure_with_setup`], correctness is supplied explicitly for
    /// every invocation and aggregated across the measured sample.
    #[allow(clippy::needless_pass_by_value)]
    pub fn measure_outcome_with_setup<S, F, I>(
        &mut self,
        name: impl Into<String>,
        logical_unit: LogicalUnit,
        setup: S,
        f: F,
    ) -> OperationOutcome
    where
        S: FnMut() -> I,
        F: FnMut(I) -> OperationOutcome,
    {
        self.measure_outcome_with_setup_and_overrides(
            name.into(),
            &logical_unit,
            MeasurementOverrides::default(),
            setup,
            f,
        )
    }

    /// Measure a workload with fresh input for every operation while excluding
    /// input construction and output destruction from the timed interval.
    ///
    /// This is the preferred shape for infallible destructive operations such
    /// as insert, sort, consume, or request construction. Repeated modes return
    /// only the final closure value. Use [`Self::measure_result_with_setup`] for
    /// fallible work, or [`Self::measure_outcome_with_setup`] when partial
    /// outcomes must be observed without stopping at the first error.
    pub fn measure_with_setup<S, F, I, R>(&mut self, name: impl Into<String>, setup: S, f: F) -> R
    where
        S: FnMut() -> I,
        F: FnMut(I) -> R,
    {
        self.measure_with_setup_and_overrides(
            name.into(),
            MeasurementIntent::General,
            MeasurementOverrides::default(),
            setup,
            f,
        )
    }

    /// Measure fallible work with fresh input and stop at the first error.
    ///
    /// Input construction and output destruction are excluded from timing.
    /// Attempted, completed, and failed operation counters describe the calls
    /// actually made before the sample stopped.
    ///
    /// # Errors
    ///
    /// Returns the first error produced by the operation. Tier 1 calibration
    /// errors are also returned and recorded as failed evidence.
    pub fn measure_result_with_setup<S, F, I, R, E>(
        &mut self,
        name: impl Into<String>,
        setup: S,
        f: F,
    ) -> Result<R, E>
    where
        S: FnMut() -> I,
        F: FnMut(I) -> Result<R, E>,
    {
        self.measure_result_with_setup_and_overrides(
            name.into(),
            MeasurementIntent::General,
            MeasurementOverrides::default(),
            setup,
            f,
        )
    }

    /// Measure infallible async work using the tier-derived default strategy.
    ///
    /// Repeated modes return only the final future output. Use
    /// [`Self::measure_result_async`] for fallible async operations.
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

    /// Measure fallible async work and stop the sample at the first error.
    ///
    /// # Errors
    ///
    /// Returns the first error produced by the measured future.
    pub async fn measure_result_async<F, Fut, R, E>(
        &mut self,
        name: impl Into<String>,
        f: F,
    ) -> Result<R, E>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<R, E>>,
    {
        self.measure_result_async_with_overrides(
            name.into(),
            MeasurementIntent::Async,
            MeasurementOverrides::default(),
            f,
        )
        .await
    }

    /// Measure fallible async work with fresh synchronously constructed input.
    ///
    /// Input construction and output destruction are excluded from timing, and
    /// the sample stops at the first error.
    ///
    /// # Errors
    ///
    /// Returns the first error produced by the measured future.
    pub async fn measure_result_async_with_setup<S, F, Fut, I, R, E>(
        &mut self,
        name: impl Into<String>,
        setup: S,
        f: F,
    ) -> Result<R, E>
    where
        S: FnMut() -> I,
        F: FnMut(I) -> Fut,
        Fut: Future<Output = Result<R, E>>,
    {
        self.measure_result_async_with_setup_and_overrides(
            name.into(),
            MeasurementIntent::Async,
            MeasurementOverrides::default(),
            setup,
            f,
        )
        .await
    }

    /// Measure infallible named threaded work and tag it with threaded intent.
    ///
    /// For fallible work, use [`Self::benchmark`], set threaded intent, and
    /// finish with [`BenchmarkBuilder::measure_result`].
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

    /// Measure infallible named pipeline work and tag it with pipeline intent.
    ///
    /// For fallible work, use [`Self::benchmark`], set pipeline intent, and
    /// finish with [`BenchmarkBuilder::measure_result`].
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

    /// Measure infallible named I/O work and tag it with I/O intent.
    ///
    /// For fallible work, use [`Self::benchmark`], set I/O intent, and finish
    /// with [`BenchmarkBuilder::measure_result`].
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

    /// Record externally timed work with observed operation outcomes.
    ///
    /// # Panics
    ///
    /// Panics when the measurement name is empty or completed work exceeds
    /// attempted work.
    #[allow(clippy::needless_pass_by_value)]
    pub fn record_external_outcome(
        &mut self,
        name: impl Into<String>,
        duration: Duration,
        logical_unit: LogicalUnit,
        outcome: OperationOutcome,
    ) -> &mut Self {
        assert!(
            outcome.completed <= outcome.attempted,
            "completed operations cannot exceed attempted operations"
        );
        let state = MeasurementState {
            duration,
            counters: correctness_from_outcome(outcome),
            operations_hint: Some(outcome.completed),
            micro: None,
            allocation: None,
        };
        self.push_measurement(
            name.into(),
            MeasurementIntent::External,
            state,
            MeasurementOverrides::default(),
        );
        self.measurements
            .last_mut()
            .expect("record_external_outcome records one measurement")
            .parameters
            .insert("logical_unit".to_string(), logical_unit.to_string());
        self
    }

    /// Record a benchmark-function error as a structured, untrustworthy row.
    pub(crate) fn record_benchmark_error(&mut self, message: &str) {
        let state = MeasurementState {
            duration: Duration::ZERO,
            counters: CorrectnessCounters {
                attempted: 1,
                failures: 1,
                ..CorrectnessCounters::default()
            },
            operations_hint: Some(0),
            micro: None,
            allocation: None,
        };
        self.push_measurement(
            "benchmark error".to_string(),
            MeasurementIntent::General,
            state,
            MeasurementOverrides::default(),
        );
        self.measurements
            .last_mut()
            .expect("record_benchmark_error records one measurement")
            .metadata
            .insert("benchmark_error".to_string(), message.to_string());
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

    fn measure_result_with_overrides<F, R, E>(
        &mut self,
        name: String,
        intent: MeasurementIntent,
        overrides: MeasurementOverrides,
        mut f: F,
    ) -> Result<R, E>
    where
        F: FnMut() -> Result<R, E>,
    {
        let (state, result) = self.time_result_workload(&mut f);
        self.push_measurement(name, intent, state, overrides);
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

    fn measure_outcome_with_overrides<F>(
        &mut self,
        name: impl Into<String>,
        logical_unit: &LogicalUnit,
        overrides: MeasurementOverrides,
        mut f: F,
    ) -> OperationOutcome
    where
        F: FnMut() -> OperationOutcome,
    {
        let (state, outcome) = self.time_outcome_workload(&mut f);
        self.push_measurement(name.into(), MeasurementIntent::Batch, state, overrides);
        self.measurements
            .last_mut()
            .expect("measure_outcome records one measurement")
            .parameters
            .insert("logical_unit".to_string(), logical_unit.to_string());
        outcome
    }

    fn measure_outcome_with_setup_and_overrides<S, F, I>(
        &mut self,
        name: String,
        logical_unit: &LogicalUnit,
        overrides: MeasurementOverrides,
        mut setup: S,
        mut f: F,
    ) -> OperationOutcome
    where
        S: FnMut() -> I,
        F: FnMut(I) -> OperationOutcome,
    {
        let (state, outcome) = self.time_outcome_workload_with_setup(&mut setup, &mut f);
        self.push_measurement(name, MeasurementIntent::Batch, state, overrides);
        self.measurements
            .last_mut()
            .expect("measure_outcome_with_setup records one measurement")
            .parameters
            .insert("logical_unit".to_string(), logical_unit.to_string());
        outcome
    }

    fn measure_with_setup_and_overrides<S, F, I, R>(
        &mut self,
        name: String,
        intent: MeasurementIntent,
        overrides: MeasurementOverrides,
        mut setup: S,
        mut f: F,
    ) -> R
    where
        S: FnMut() -> I,
        F: FnMut(I) -> R,
    {
        let (state, result) = self.time_workload_with_setup(&mut setup, &mut f);
        self.push_measurement(name, intent, state, overrides);
        result
    }

    fn measure_result_with_setup_and_overrides<S, F, I, R, E>(
        &mut self,
        name: String,
        intent: MeasurementIntent,
        overrides: MeasurementOverrides,
        mut setup: S,
        mut f: F,
    ) -> Result<R, E>
    where
        S: FnMut() -> I,
        F: FnMut(I) -> Result<R, E>,
    {
        let (state, result) = self.time_result_workload_with_setup(&mut setup, &mut f);
        self.push_measurement(name, intent, state, overrides);
        result
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

    async fn measure_result_async_with_overrides<F, Fut, R, E>(
        &mut self,
        name: String,
        intent: MeasurementIntent,
        overrides: MeasurementOverrides,
        mut f: F,
    ) -> Result<R, E>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<R, E>>,
    {
        let (state, result) = self.time_result_async_workload(&mut f).await;
        self.push_measurement(name, intent, state, overrides);
        result
    }

    async fn measure_result_async_with_setup_and_overrides<S, F, Fut, I, R, E>(
        &mut self,
        name: String,
        intent: MeasurementIntent,
        overrides: MeasurementOverrides,
        mut setup: S,
        mut f: F,
    ) -> Result<R, E>
    where
        S: FnMut() -> I,
        F: FnMut(I) -> Fut,
        Fut: Future<Output = Result<R, E>>,
    {
        let (state, result) = self
            .time_result_async_workload_with_setup(&mut setup, &mut f)
            .await;
        self.push_measurement(name, intent, state, overrides);
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

    fn time_result_workload<F, R, E>(&self, f: &mut F) -> (MeasurementState, Result<R, E>)
    where
        F: FnMut() -> Result<R, E>,
    {
        match &self.mode {
            BenchmarkMode::Micro {
                target_sample_duration,
            } => Self::time_result_micro(*target_sample_duration, f),
            BenchmarkMode::FixedDuration { sample_duration } => {
                Self::time_result_fixed_duration(*sample_duration, f)
            }
            BenchmarkMode::FixedOperations {
                operations_per_sample,
            } => Self::time_result_fixed_operations(*operations_per_sample, f),
        }
    }

    fn time_outcome_workload<F>(&self, f: &mut F) -> (MeasurementState, OperationOutcome)
    where
        F: FnMut() -> OperationOutcome,
    {
        match &self.mode {
            BenchmarkMode::Micro {
                target_sample_duration,
            } => {
                let iterations =
                    calibrate_iterations(
                        *target_sample_duration,
                        &mut || std::hint::black_box(f()),
                    );
                let allocation_start = allocation::snapshot();
                let start = Instant::now();
                let mut outcome = OperationOutcome::default();
                for _ in 0..iterations {
                    outcome.accumulate(std::hint::black_box(f()));
                }
                let gross_elapsed = start.elapsed();
                let allocation_delta = allocation_start.map(allocation::delta_since);
                let overhead = time_empty_batch(iterations);
                let net_elapsed = gross_elapsed.saturating_sub(overhead);
                (
                    MeasurementState {
                        duration: net_elapsed,
                        counters: correctness_from_outcome(outcome),
                        operations_hint: Some(outcome.completed),
                        micro: Some(MicroMeasurement {
                            iterations,
                            gross_elapsed,
                            overhead,
                            net_elapsed,
                        }),
                        allocation: allocation_delta.map(allocation_measurement),
                    },
                    outcome,
                )
            }
            BenchmarkMode::FixedDuration { sample_duration } => {
                let allocation_start = allocation::snapshot();
                let start = Instant::now();
                let mut outcome = OperationOutcome::default();
                loop {
                    outcome.accumulate(std::hint::black_box(f()));
                    if start.elapsed() >= *sample_duration {
                        break;
                    }
                }
                let duration = start.elapsed();
                let allocation_delta = allocation_start.map(allocation::delta_since);
                (
                    MeasurementState {
                        duration,
                        counters: correctness_from_outcome(outcome),
                        operations_hint: Some(outcome.completed),
                        micro: None,
                        allocation: allocation_delta.map(allocation_measurement),
                    },
                    outcome,
                )
            }
            BenchmarkMode::FixedOperations {
                operations_per_sample,
            } => {
                let allocation_start = allocation::snapshot();
                let start = Instant::now();
                let mut outcome = OperationOutcome::default();
                for _ in 0..*operations_per_sample {
                    outcome.accumulate(std::hint::black_box(f()));
                }
                let duration = start.elapsed();
                let allocation_delta = allocation_start.map(allocation::delta_since);
                (
                    MeasurementState {
                        duration,
                        counters: correctness_from_outcome(outcome),
                        operations_hint: Some(outcome.completed),
                        micro: None,
                        allocation: allocation_delta.map(allocation_measurement),
                    },
                    outcome,
                )
            }
        }
    }

    fn time_outcome_workload_with_setup<S, F, I>(
        &self,
        setup: &mut S,
        f: &mut F,
    ) -> (MeasurementState, OperationOutcome)
    where
        S: FnMut() -> I,
        F: FnMut(I) -> OperationOutcome,
    {
        match &self.mode {
            BenchmarkMode::Micro {
                target_sample_duration,
            } => {
                let iterations = calibrate_setup_iterations(*target_sample_duration, setup, f);
                let mut gross_elapsed = Duration::ZERO;
                let mut allocation_total = None;
                let mut outcome = OperationOutcome::default();
                for _ in 0..iterations {
                    let (elapsed, allocation_delta, invocation) =
                        time_operation_with_setup(setup, f);
                    gross_elapsed = gross_elapsed.saturating_add(elapsed);
                    accumulate_allocation(&mut allocation_total, allocation_delta);
                    outcome.accumulate(std::hint::black_box(invocation));
                }
                let overhead = time_empty_iterations(iterations);
                let net_elapsed = gross_elapsed.saturating_sub(overhead);
                (
                    MeasurementState {
                        duration: net_elapsed,
                        counters: correctness_from_outcome(outcome),
                        operations_hint: Some(outcome.completed),
                        micro: Some(MicroMeasurement {
                            iterations,
                            gross_elapsed,
                            overhead,
                            net_elapsed,
                        }),
                        allocation: allocation_total,
                    },
                    outcome,
                )
            }
            BenchmarkMode::FixedDuration { sample_duration } => {
                let mut measured_elapsed = Duration::ZERO;
                let mut allocation_total = None;
                let mut outcome = OperationOutcome::default();
                loop {
                    let (elapsed, allocation_delta, invocation) =
                        time_operation_with_setup(setup, f);
                    measured_elapsed = measured_elapsed.saturating_add(elapsed);
                    accumulate_allocation(&mut allocation_total, allocation_delta);
                    outcome.accumulate(std::hint::black_box(invocation));
                    if measured_elapsed >= *sample_duration {
                        break;
                    }
                }
                (
                    MeasurementState {
                        duration: measured_elapsed,
                        counters: correctness_from_outcome(outcome),
                        operations_hint: Some(outcome.completed),
                        micro: None,
                        allocation: allocation_total,
                    },
                    outcome,
                )
            }
            BenchmarkMode::FixedOperations {
                operations_per_sample,
            } => {
                let mut measured_elapsed = Duration::ZERO;
                let mut allocation_total = None;
                let mut outcome = OperationOutcome::default();
                for _ in 0..*operations_per_sample {
                    let (elapsed, allocation_delta, invocation) =
                        time_operation_with_setup(setup, f);
                    measured_elapsed = measured_elapsed.saturating_add(elapsed);
                    accumulate_allocation(&mut allocation_total, allocation_delta);
                    outcome.accumulate(std::hint::black_box(invocation));
                }
                (
                    MeasurementState {
                        duration: measured_elapsed,
                        counters: correctness_from_outcome(outcome),
                        operations_hint: Some(outcome.completed),
                        micro: None,
                        allocation: allocation_total,
                    },
                    outcome,
                )
            }
        }
    }

    fn time_workload_with_setup<S, F, I, R>(
        &self,
        setup: &mut S,
        f: &mut F,
    ) -> (MeasurementState, R)
    where
        S: FnMut() -> I,
        F: FnMut(I) -> R,
    {
        match &self.mode {
            BenchmarkMode::Micro {
                target_sample_duration,
            } => {
                let iterations = calibrate_setup_iterations(*target_sample_duration, setup, f);
                let mut gross_elapsed = Duration::ZERO;
                let mut allocation_total = None;
                let mut result = None;
                for _ in 0..iterations {
                    let (elapsed, allocation_delta, output) = time_operation_with_setup(setup, f);
                    gross_elapsed = gross_elapsed.saturating_add(elapsed);
                    accumulate_allocation(&mut allocation_total, allocation_delta);
                    result = Some(std::hint::black_box(output));
                }
                let overhead = time_empty_iterations(iterations);
                let net_elapsed = gross_elapsed.saturating_sub(overhead);
                let mut counters = CorrectnessCounters::default();
                set_successful_operations(&mut counters, iterations);
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
                        allocation: allocation_total,
                    },
                    result.expect("setup measurement runs at least once"),
                )
            }
            BenchmarkMode::FixedDuration { sample_duration } => {
                let (first_elapsed, first_allocation, first_output) =
                    time_operation_with_setup(setup, f);
                let mut measured_elapsed = first_elapsed;
                let mut allocation_total = None;
                accumulate_allocation(&mut allocation_total, first_allocation);
                let mut operations = 1_u64;
                let mut result = std::hint::black_box(first_output);
                while measured_elapsed < *sample_duration {
                    let (elapsed, allocation_delta, output) = time_operation_with_setup(setup, f);
                    measured_elapsed = measured_elapsed.saturating_add(elapsed);
                    accumulate_allocation(&mut allocation_total, allocation_delta);
                    operations = operations.saturating_add(1);
                    result = std::hint::black_box(output);
                }
                let mut counters = CorrectnessCounters::default();
                set_successful_operations(&mut counters, operations);
                (
                    MeasurementState {
                        duration: measured_elapsed,
                        counters,
                        operations_hint: Some(operations),
                        micro: None,
                        allocation: allocation_total,
                    },
                    result,
                )
            }
            BenchmarkMode::FixedOperations {
                operations_per_sample,
            } => {
                let mut measured_elapsed = Duration::ZERO;
                let mut allocation_total = None;
                let mut result = None;
                for _ in 0..*operations_per_sample {
                    let (elapsed, allocation_delta, output) = time_operation_with_setup(setup, f);
                    measured_elapsed = measured_elapsed.saturating_add(elapsed);
                    accumulate_allocation(&mut allocation_total, allocation_delta);
                    result = Some(std::hint::black_box(output));
                }
                let mut counters = CorrectnessCounters::default();
                set_successful_operations(&mut counters, *operations_per_sample);
                (
                    MeasurementState {
                        duration: measured_elapsed,
                        counters,
                        operations_hint: Some(*operations_per_sample),
                        micro: None,
                        allocation: allocation_total,
                    },
                    result.expect("setup measurement runs at least once"),
                )
            }
        }
    }

    fn time_result_workload_with_setup<S, F, I, R, E>(
        &self,
        setup: &mut S,
        f: &mut F,
    ) -> (MeasurementState, Result<R, E>)
    where
        S: FnMut() -> I,
        F: FnMut(I) -> Result<R, E>,
    {
        match &self.mode {
            BenchmarkMode::Micro {
                target_sample_duration,
            } => Self::time_result_micro_with_setup(*target_sample_duration, setup, f),
            BenchmarkMode::FixedDuration { sample_duration } => {
                Self::time_result_fixed_duration_with_setup(*sample_duration, setup, f)
            }
            BenchmarkMode::FixedOperations {
                operations_per_sample,
            } => Self::time_result_fixed_operations_with_setup(*operations_per_sample, setup, f),
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
        _intent: MeasurementIntent,
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

    fn time_result_micro<F, R, E>(target: Duration, f: &mut F) -> (MeasurementState, Result<R, E>)
    where
        F: FnMut() -> Result<R, E>,
    {
        let iterations = match calibrate_result_iterations(target, f) {
            Ok(iterations) => iterations,
            Err(error) => return failed_micro_result(error),
        };
        let allocation_start = allocation::snapshot();
        let start = Instant::now();
        let mut progress = FallibleProgress::default();
        for _ in 0..iterations {
            if !progress.observe(std::hint::black_box(f())) {
                break;
            }
        }
        let gross_elapsed = start.elapsed();
        let allocation_delta = allocation_start.map(allocation::delta_since);
        let overhead = time_empty_batch(progress.attempted);
        let net_elapsed = gross_elapsed.saturating_sub(overhead);
        let attempted = progress.attempted;
        let (counters, result) = progress.finish();
        (
            MeasurementState {
                duration: net_elapsed,
                operations_hint: Some(counters.completed),
                counters,
                micro: Some(MicroMeasurement {
                    iterations: attempted,
                    gross_elapsed,
                    overhead,
                    net_elapsed,
                }),
                allocation: allocation_delta.map(allocation_measurement),
            },
            result,
        )
    }

    fn time_result_fixed_duration<F, R, E>(
        sample_duration: Duration,
        f: &mut F,
    ) -> (MeasurementState, Result<R, E>)
    where
        F: FnMut() -> Result<R, E>,
    {
        let allocation_start = allocation::snapshot();
        let start = Instant::now();
        let mut progress = FallibleProgress::default();
        loop {
            if !progress.observe(std::hint::black_box(f())) || start.elapsed() >= sample_duration {
                break;
            }
        }
        let duration = start.elapsed();
        let allocation_delta = allocation_start.map(allocation::delta_since);
        let (counters, result) = progress.finish();
        (
            MeasurementState {
                duration,
                operations_hint: Some(counters.completed),
                counters,
                micro: None,
                allocation: allocation_delta.map(allocation_measurement),
            },
            result,
        )
    }

    fn time_result_fixed_operations<F, R, E>(
        operations_per_sample: u64,
        f: &mut F,
    ) -> (MeasurementState, Result<R, E>)
    where
        F: FnMut() -> Result<R, E>,
    {
        let allocation_start = allocation::snapshot();
        let start = Instant::now();
        let mut progress = FallibleProgress::default();
        for _ in 0..operations_per_sample {
            if !progress.observe(std::hint::black_box(f())) {
                break;
            }
        }
        let duration = start.elapsed();
        let allocation_delta = allocation_start.map(allocation::delta_since);
        let (counters, result) = progress.finish();
        (
            MeasurementState {
                duration,
                operations_hint: Some(counters.completed),
                counters,
                micro: None,
                allocation: allocation_delta.map(allocation_measurement),
            },
            result,
        )
    }

    fn time_result_micro_with_setup<S, F, I, R, E>(
        target: Duration,
        setup: &mut S,
        f: &mut F,
    ) -> (MeasurementState, Result<R, E>)
    where
        S: FnMut() -> I,
        F: FnMut(I) -> Result<R, E>,
    {
        let iterations = match calibrate_result_setup_iterations(target, setup, f) {
            Ok(iterations) => iterations,
            Err(error) => return failed_micro_result(error),
        };
        let mut gross_elapsed = Duration::ZERO;
        let mut allocation_total = None;
        let mut progress = FallibleProgress::default();
        for _ in 0..iterations {
            let (elapsed, allocation_delta, output) = time_operation_with_setup(setup, f);
            gross_elapsed = gross_elapsed.saturating_add(elapsed);
            accumulate_allocation(&mut allocation_total, allocation_delta);
            if !progress.observe(std::hint::black_box(output)) {
                break;
            }
        }
        let overhead = time_empty_iterations(progress.attempted);
        let net_elapsed = gross_elapsed.saturating_sub(overhead);
        let attempted = progress.attempted;
        let (counters, result) = progress.finish();
        (
            MeasurementState {
                duration: net_elapsed,
                operations_hint: Some(counters.completed),
                counters,
                micro: Some(MicroMeasurement {
                    iterations: attempted,
                    gross_elapsed,
                    overhead,
                    net_elapsed,
                }),
                allocation: allocation_total,
            },
            result,
        )
    }

    fn time_result_fixed_duration_with_setup<S, F, I, R, E>(
        sample_duration: Duration,
        setup: &mut S,
        f: &mut F,
    ) -> (MeasurementState, Result<R, E>)
    where
        S: FnMut() -> I,
        F: FnMut(I) -> Result<R, E>,
    {
        let mut measured_elapsed = Duration::ZERO;
        let mut allocation_total = None;
        let mut progress = FallibleProgress::default();
        loop {
            let (elapsed, allocation_delta, output) = time_operation_with_setup(setup, f);
            measured_elapsed = measured_elapsed.saturating_add(elapsed);
            accumulate_allocation(&mut allocation_total, allocation_delta);
            if !progress.observe(std::hint::black_box(output))
                || measured_elapsed >= sample_duration
            {
                break;
            }
        }
        let (counters, result) = progress.finish();
        (
            MeasurementState {
                duration: measured_elapsed,
                operations_hint: Some(counters.completed),
                counters,
                micro: None,
                allocation: allocation_total,
            },
            result,
        )
    }

    fn time_result_fixed_operations_with_setup<S, F, I, R, E>(
        operations_per_sample: u64,
        setup: &mut S,
        f: &mut F,
    ) -> (MeasurementState, Result<R, E>)
    where
        S: FnMut() -> I,
        F: FnMut(I) -> Result<R, E>,
    {
        let mut measured_elapsed = Duration::ZERO;
        let mut allocation_total = None;
        let mut progress = FallibleProgress::default();
        for _ in 0..operations_per_sample {
            let (elapsed, allocation_delta, output) = time_operation_with_setup(setup, f);
            measured_elapsed = measured_elapsed.saturating_add(elapsed);
            accumulate_allocation(&mut allocation_total, allocation_delta);
            if !progress.observe(std::hint::black_box(output)) {
                break;
            }
        }
        let (counters, result) = progress.finish();
        (
            MeasurementState {
                duration: measured_elapsed,
                operations_hint: Some(counters.completed),
                counters,
                micro: None,
                allocation: allocation_total,
            },
            result,
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

    async fn time_result_async_workload<F, Fut, R, E>(
        &self,
        f: &mut F,
    ) -> (MeasurementState, Result<R, E>)
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<R, E>>,
    {
        match &self.mode {
            BenchmarkMode::Micro {
                target_sample_duration,
            } => {
                self.time_result_async_duration(*target_sample_duration, true, f)
                    .await
            }
            BenchmarkMode::FixedDuration { sample_duration } => {
                self.time_result_async_duration(*sample_duration, false, f)
                    .await
            }
            BenchmarkMode::FixedOperations {
                operations_per_sample,
            } => {
                self.time_result_async_operations(*operations_per_sample, f)
                    .await
            }
        }
    }

    async fn time_result_async_workload_with_setup<S, F, Fut, I, R, E>(
        &self,
        setup: &mut S,
        f: &mut F,
    ) -> (MeasurementState, Result<R, E>)
    where
        S: FnMut() -> I,
        F: FnMut(I) -> Fut,
        Fut: Future<Output = Result<R, E>>,
    {
        match &self.mode {
            BenchmarkMode::Micro {
                target_sample_duration,
            } => {
                Self::time_result_async_duration_with_setup(*target_sample_duration, true, setup, f)
                    .await
            }
            BenchmarkMode::FixedDuration { sample_duration } => {
                Self::time_result_async_duration_with_setup(*sample_duration, false, setup, f).await
            }
            BenchmarkMode::FixedOperations {
                operations_per_sample,
            } => {
                Self::time_result_async_operations_with_setup(*operations_per_sample, setup, f)
                    .await
            }
        }
    }

    async fn time_result_async_duration<F, Fut, R, E>(
        &self,
        sample_duration: Duration,
        micro: bool,
        f: &mut F,
    ) -> (MeasurementState, Result<R, E>)
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<R, E>>,
    {
        let allocation_start = allocation::snapshot();
        let start = Instant::now();
        let mut progress = FallibleProgress::default();
        loop {
            if !progress.observe(std::hint::black_box(f().await))
                || start.elapsed() >= sample_duration
            {
                break;
            }
        }
        let duration = start.elapsed();
        let allocation_delta = allocation_start.map(allocation::delta_since);
        let iterations = progress.attempted;
        let (counters, result) = progress.finish();
        (
            MeasurementState {
                duration,
                counters,
                operations_hint: Some(counters.completed),
                micro: micro.then_some(MicroMeasurement {
                    iterations,
                    gross_elapsed: duration,
                    overhead: Duration::ZERO,
                    net_elapsed: duration,
                }),
                allocation: allocation_delta.map(allocation_measurement),
            },
            result,
        )
    }

    async fn time_result_async_operations<F, Fut, R, E>(
        &self,
        operations_per_sample: u64,
        f: &mut F,
    ) -> (MeasurementState, Result<R, E>)
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<R, E>>,
    {
        let allocation_start = allocation::snapshot();
        let start = Instant::now();
        let mut progress = FallibleProgress::default();
        for _ in 0..operations_per_sample {
            if !progress.observe(std::hint::black_box(f().await)) {
                break;
            }
        }
        let duration = start.elapsed();
        let allocation_delta = allocation_start.map(allocation::delta_since);
        let (counters, result) = progress.finish();
        (
            MeasurementState {
                duration,
                counters,
                operations_hint: Some(counters.completed),
                micro: None,
                allocation: allocation_delta.map(allocation_measurement),
            },
            result,
        )
    }

    async fn time_result_async_duration_with_setup<S, F, Fut, I, R, E>(
        sample_duration: Duration,
        micro: bool,
        setup: &mut S,
        f: &mut F,
    ) -> (MeasurementState, Result<R, E>)
    where
        S: FnMut() -> I,
        F: FnMut(I) -> Fut,
        Fut: Future<Output = Result<R, E>>,
    {
        let mut measured_elapsed = Duration::ZERO;
        let mut allocation_total = None;
        let mut progress = FallibleProgress::default();
        loop {
            let (elapsed, allocation_delta, output) =
                time_async_operation_with_setup(setup, f).await;
            measured_elapsed = measured_elapsed.saturating_add(elapsed);
            accumulate_allocation(&mut allocation_total, allocation_delta);
            if !progress.observe(std::hint::black_box(output))
                || measured_elapsed >= sample_duration
            {
                break;
            }
        }
        let iterations = progress.attempted;
        let (counters, result) = progress.finish();
        (
            MeasurementState {
                duration: measured_elapsed,
                counters,
                operations_hint: Some(counters.completed),
                micro: micro.then_some(MicroMeasurement {
                    iterations,
                    gross_elapsed: measured_elapsed,
                    overhead: Duration::ZERO,
                    net_elapsed: measured_elapsed,
                }),
                allocation: allocation_total,
            },
            result,
        )
    }

    async fn time_result_async_operations_with_setup<S, F, Fut, I, R, E>(
        operations_per_sample: u64,
        setup: &mut S,
        f: &mut F,
    ) -> (MeasurementState, Result<R, E>)
    where
        S: FnMut() -> I,
        F: FnMut(I) -> Fut,
        Fut: Future<Output = Result<R, E>>,
    {
        let mut measured_elapsed = Duration::ZERO;
        let mut allocation_total = None;
        let mut progress = FallibleProgress::default();
        for _ in 0..operations_per_sample {
            let (elapsed, allocation_delta, output) =
                time_async_operation_with_setup(setup, f).await;
            measured_elapsed = measured_elapsed.saturating_add(elapsed);
            accumulate_allocation(&mut allocation_total, allocation_delta);
            if !progress.observe(std::hint::black_box(output)) {
                break;
            }
        }
        let (counters, result) = progress.finish();
        (
            MeasurementState {
                duration: measured_elapsed,
                counters,
                operations_hint: Some(counters.completed),
                micro: None,
                allocation: allocation_total,
            },
            result,
        )
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
            // Explicitly observed errors are stronger evidence than counters
            // staged before the operation ran. Manual counters may refine an
            // otherwise successful inferred measurement, but never erase a
            // Result or OperationOutcome failure.
            if state.counters.passed() {
                state.counters = self.pending_counters;
                state.operations_hint = Some(state.counters.completed);
            }
            self.pending_counters = CorrectnessCounters::default();
            self.pending_has_counters = false;
        }
        let latency_ns = std::mem::take(&mut self.pending_latency_ns);
        let observations = std::mem::take(&mut self.pending_observations);
        self.measurements.push(MeasurementRecord {
            name,
            intent,
            mode: self.mode.clone(),
            duration: state.duration,
            latency_ns,
            observations,
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
    operations_per_sample: Option<u64>,
    intent: MeasurementIntent,
    parameters: BTreeMap<String, String>,
    metadata: BTreeMap<String, String>,
}

impl BenchmarkBuilder<'_> {
    /// Override measured sample count for this measurement.
    ///
    /// # Panics
    ///
    /// Panics when `value` is zero.
    #[must_use]
    pub const fn samples(mut self, value: usize) -> Self {
        assert!(value != 0, "measured samples must be greater than zero");
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

    /// Override the number of measured operations in each Tier 2 sample.
    ///
    /// This batches fast subsystem operations into a stable timing sample
    /// while preserving per-operation results.
    ///
    /// # Panics
    ///
    /// Panics when `value` is zero or the row is not using the Tier 2
    /// fixed-operations mode.
    #[must_use]
    pub fn operations_per_sample(mut self, value: u64) -> Self {
        assert!(
            value != 0,
            "operations per sample must be greater than zero"
        );
        assert!(
            matches!(&self.ctx.mode, BenchmarkMode::FixedOperations { .. }),
            "operations_per_sample is available only for Tier 2 fixed-operations rows"
        );
        self.operations_per_sample = Some(value);
        self
    }

    /// Set the measurement intent.
    #[must_use]
    pub const fn intent(mut self, intent: MeasurementIntent) -> Self {
        self.intent = intent;
        self
    }

    /// Add a parameter scoped to this measurement only.
    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn parameter(mut self, key: impl Into<String>, value: impl ToString) -> Self {
        self.parameters.insert(key.into(), value.to_string());
        self
    }

    /// Add metadata scoped to this measurement only.
    ///
    /// # Panics
    ///
    /// Panics when `key` is `trust_class`; use [`Self::role`] instead.
    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn metadata(mut self, key: impl Into<String>, value: impl ToString) -> Self {
        let key = key.into();
        assert_authorable_metadata_key(&key);
        self.metadata.insert(key, value.to_string());
        self
    }

    /// Declare whether this row is a release gate, diagnostic, or experiment.
    ///
    /// # Panics
    ///
    /// Panics when `role` is [`TrustClass::Invalid`], which is reserved for
    /// trust failures derived by the framework.
    #[must_use]
    pub fn role(mut self, role: TrustClass) -> Self {
        assert!(
            role != TrustClass::Invalid,
            "invalid is a derived trust class, not an authorable benchmark role"
        );
        self.metadata
            .insert("trust_class".to_string(), role.to_string());
        self
    }

    /// Declare the logical operation represented by this measurement.
    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn logical_unit(mut self, unit: LogicalUnit) -> Self {
        self.parameters
            .insert("logical_unit".to_string(), unit.to_string());
        self
    }

    /// Measure infallible work using the builder overrides.
    ///
    /// Repeated modes return only the final closure value. Use
    /// [`Self::measure_result`] for fallible work.
    pub fn measure<F, R>(self, f: F) -> R
    where
        F: FnMut() -> R,
    {
        let Self {
            ctx,
            name,
            overrides,
            operations_per_sample,
            intent,
            parameters,
            metadata,
        } = self;
        let original_mode = apply_operations_override(ctx, operations_per_sample);
        let result = ctx.measure_with_intent(name, intent, overrides, f);
        restore_mode(ctx, original_mode);
        attach_measurement_fields(ctx, parameters, metadata);
        result
    }

    /// Measure fallible work using the builder overrides.
    ///
    /// The sample stops at the first error and records only the calls actually
    /// attempted. This includes row-local [`Self::operations_per_sample`]
    /// overrides.
    ///
    /// # Errors
    ///
    /// Returns the first error produced by the measured operation.
    pub fn measure_result<F, R, E>(self, f: F) -> Result<R, E>
    where
        F: FnMut() -> Result<R, E>,
    {
        let Self {
            ctx,
            name,
            overrides,
            operations_per_sample,
            intent,
            parameters,
            metadata,
        } = self;
        let original_mode = apply_operations_override(ctx, operations_per_sample);
        let result = ctx.measure_result_with_overrides(name, intent, overrides, f);
        restore_mode(ctx, original_mode);
        attach_measurement_fields(ctx, parameters, metadata);
        result
    }

    /// Measure infallible batched work using the builder overrides.
    ///
    /// Closure return values are discarded. Use [`Self::measure_result`] when
    /// each invocation can fail or [`Self::measure_outcome`] for observed
    /// partial outcomes.
    pub fn measure_batch<F, R>(self, logical_operations_per_iteration: u64, f: F) -> u64
    where
        F: FnMut() -> R,
    {
        let Self {
            ctx,
            name,
            overrides,
            operations_per_sample,
            parameters,
            metadata,
            ..
        } = self;
        let original_mode = apply_operations_override(ctx, operations_per_sample);
        let completed = ctx.measure_batch_with_overrides(
            name,
            logical_operations_per_iteration,
            MeasurementIntent::Batch,
            overrides,
            f,
        );
        restore_mode(ctx, original_mode);
        attach_measurement_fields(ctx, parameters, metadata);
        completed
    }

    /// Measure observed logical-operation outcomes using builder-scoped facts.
    #[allow(clippy::needless_pass_by_value)]
    pub fn measure_outcome<F>(self, logical_unit: LogicalUnit, f: F) -> OperationOutcome
    where
        F: FnMut() -> OperationOutcome,
    {
        let Self {
            ctx,
            name,
            overrides,
            operations_per_sample,
            parameters,
            metadata,
            ..
        } = self;
        let original_mode = apply_operations_override(ctx, operations_per_sample);
        let outcome = ctx.measure_outcome_with_overrides(name, &logical_unit, overrides, f);
        restore_mode(ctx, original_mode);
        attach_measurement_fields(ctx, parameters, metadata);
        outcome
    }

    /// Measure observed outcomes with fresh input using builder-scoped facts.
    #[allow(clippy::needless_pass_by_value)]
    pub fn measure_outcome_with_setup<S, F, I>(
        self,
        logical_unit: LogicalUnit,
        setup: S,
        f: F,
    ) -> OperationOutcome
    where
        S: FnMut() -> I,
        F: FnMut(I) -> OperationOutcome,
    {
        let Self {
            ctx,
            name,
            overrides,
            operations_per_sample,
            parameters,
            metadata,
            ..
        } = self;
        let original_mode = apply_operations_override(ctx, operations_per_sample);
        let outcome =
            ctx.measure_outcome_with_setup_and_overrides(name, &logical_unit, overrides, setup, f);
        restore_mode(ctx, original_mode);
        attach_measurement_fields(ctx, parameters, metadata);
        outcome
    }

    /// Measure infallible work with fresh input for every operation.
    ///
    /// Repeated modes return only the final closure value. Use
    /// [`Self::measure_result_with_setup`] for fallible work.
    pub fn measure_with_setup<S, F, I, R>(self, setup: S, f: F) -> R
    where
        S: FnMut() -> I,
        F: FnMut(I) -> R,
    {
        let Self {
            ctx,
            name,
            overrides,
            operations_per_sample,
            intent,
            parameters,
            metadata,
            ..
        } = self;
        let original_mode = apply_operations_override(ctx, operations_per_sample);
        let result = ctx.measure_with_setup_and_overrides(name, intent, overrides, setup, f);
        restore_mode(ctx, original_mode);
        attach_measurement_fields(ctx, parameters, metadata);
        result
    }

    /// Measure fallible work with fresh input and stop at the first error.
    ///
    /// # Errors
    ///
    /// Returns the first error produced by the measured operation.
    pub fn measure_result_with_setup<S, F, I, R, E>(self, setup: S, f: F) -> Result<R, E>
    where
        S: FnMut() -> I,
        F: FnMut(I) -> Result<R, E>,
    {
        let Self {
            ctx,
            name,
            overrides,
            operations_per_sample,
            intent,
            parameters,
            metadata,
        } = self;
        let original_mode = apply_operations_override(ctx, operations_per_sample);
        let result = ctx.measure_result_with_setup_and_overrides(name, intent, overrides, setup, f);
        restore_mode(ctx, original_mode);
        attach_measurement_fields(ctx, parameters, metadata);
        result
    }

    /// Measure infallible async work using the builder overrides.
    ///
    /// Repeated modes return only the final future output. Use
    /// [`Self::measure_result_async`] for fallible work.
    pub async fn measure_async<F, Fut, R>(self, f: F) -> R
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = R>,
    {
        let Self {
            ctx,
            name,
            overrides,
            operations_per_sample,
            parameters,
            metadata,
            ..
        } = self;
        let original_mode = apply_operations_override(ctx, operations_per_sample);
        let result = ctx
            .measure_async_with_overrides(name, MeasurementIntent::Async, overrides, f)
            .await;
        restore_mode(ctx, original_mode);
        attach_measurement_fields(ctx, parameters, metadata);
        result
    }

    /// Measure fallible async work and stop at the first error.
    ///
    /// # Errors
    ///
    /// Returns the first error produced by the measured future.
    pub async fn measure_result_async<F, Fut, R, E>(self, f: F) -> Result<R, E>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<R, E>>,
    {
        let Self {
            ctx,
            name,
            overrides,
            operations_per_sample,
            parameters,
            metadata,
            ..
        } = self;
        let original_mode = apply_operations_override(ctx, operations_per_sample);
        let result = ctx
            .measure_result_async_with_overrides(name, MeasurementIntent::Async, overrides, f)
            .await;
        restore_mode(ctx, original_mode);
        attach_measurement_fields(ctx, parameters, metadata);
        result
    }

    /// Measure fallible async work with fresh synchronously constructed input.
    ///
    /// # Errors
    ///
    /// Returns the first error produced by the measured future.
    pub async fn measure_result_async_with_setup<S, F, Fut, I, R, E>(
        self,
        setup: S,
        f: F,
    ) -> Result<R, E>
    where
        S: FnMut() -> I,
        F: FnMut(I) -> Fut,
        Fut: Future<Output = Result<R, E>>,
    {
        let Self {
            ctx,
            name,
            overrides,
            operations_per_sample,
            parameters,
            metadata,
            ..
        } = self;
        let original_mode = apply_operations_override(ctx, operations_per_sample);
        let result = ctx
            .measure_result_async_with_setup_and_overrides(
                name,
                MeasurementIntent::Async,
                overrides,
                setup,
                f,
            )
            .await;
        restore_mode(ctx, original_mode);
        attach_measurement_fields(ctx, parameters, metadata);
        result
    }
}

fn assert_authorable_metadata_key(key: &str) {
    assert!(
        key != "trust_class",
        "trust_class is reserved; use the typed benchmark role API"
    );
}

fn apply_operations_override(
    ctx: &mut StressContext,
    operations_per_sample: Option<u64>,
) -> Option<BenchmarkMode> {
    operations_per_sample.map(|operations_per_sample| {
        std::mem::replace(
            &mut ctx.mode,
            BenchmarkMode::FixedOperations {
                operations_per_sample,
            },
        )
    })
}

fn restore_mode(ctx: &mut StressContext, original_mode: Option<BenchmarkMode>) {
    if let Some(original_mode) = original_mode {
        ctx.mode = original_mode;
    }
}

fn attach_measurement_fields(
    ctx: &mut StressContext,
    parameters: BTreeMap<String, String>,
    metadata: BTreeMap<String, String>,
) {
    let record = ctx
        .measurements
        .last_mut()
        .expect("benchmark builder records one measurement");
    record.parameters.extend(parameters);
    record.metadata.extend(metadata);
}

fn allocation_measurement(delta: allocation::AllocationDelta) -> AllocationMeasurement {
    AllocationMeasurement {
        allocs: delta.allocs,
        bytes: delta.bytes,
    }
}

fn correctness_from_outcome(outcome: OperationOutcome) -> CorrectnessCounters {
    CorrectnessCounters {
        attempted: outcome.attempted,
        completed: outcome.completed,
        failures: outcome.failures,
        timeouts: outcome.timeouts,
        duplicates: outcome.duplicates,
        dropped: outcome.dropped,
        validation_errors: outcome.validation_errors,
    }
}

fn accumulate_allocation(
    total: &mut Option<AllocationMeasurement>,
    delta: Option<allocation::AllocationDelta>,
) {
    let Some(delta) = delta else {
        return;
    };
    let total = total.get_or_insert_with(AllocationMeasurement::default);
    total.allocs = total.allocs.saturating_add(delta.allocs);
    total.bytes = total.bytes.saturating_add(delta.bytes);
}

fn time_operation_with_setup<S, F, I, R>(
    setup: &mut S,
    f: &mut F,
) -> (Duration, Option<allocation::AllocationDelta>, R)
where
    S: FnMut() -> I,
    F: FnMut(I) -> R,
{
    let input = std::hint::black_box(setup());
    let allocation_start = allocation::snapshot();
    let start = Instant::now();
    let output = std::hint::black_box(f(input));
    let elapsed = start.elapsed();
    let allocation_delta = allocation_start.map(allocation::delta_since);
    (elapsed, allocation_delta, output)
}

async fn time_async_operation_with_setup<S, F, Fut, I, R>(
    setup: &mut S,
    f: &mut F,
) -> (Duration, Option<allocation::AllocationDelta>, R)
where
    S: FnMut() -> I,
    F: FnMut(I) -> Fut,
    Fut: Future<Output = R>,
{
    let input = std::hint::black_box(setup());
    let allocation_start = allocation::snapshot();
    let start = Instant::now();
    let output = std::hint::black_box(f(input).await);
    let elapsed = start.elapsed();
    let allocation_delta = allocation_start.map(allocation::delta_since);
    (elapsed, allocation_delta, output)
}

fn calibrate_setup_iterations<S, F, I, R>(target: Duration, setup: &mut S, f: &mut F) -> u64
where
    S: FnMut() -> I,
    F: FnMut(I) -> R,
{
    let mut iterations = 1_u64;
    loop {
        let mut elapsed = Duration::ZERO;
        for _ in 0..iterations {
            let (iteration_elapsed, _, output) = time_operation_with_setup(setup, f);
            elapsed = elapsed.saturating_add(iteration_elapsed);
            std::hint::black_box(output);
        }
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

fn calibrate_result_setup_iterations<S, F, I, R, E>(
    target: Duration,
    setup: &mut S,
    f: &mut F,
) -> Result<u64, E>
where
    S: FnMut() -> I,
    F: FnMut(I) -> Result<R, E>,
{
    let mut iterations = 1_u64;
    loop {
        let mut elapsed = Duration::ZERO;
        for _ in 0..iterations {
            let (iteration_elapsed, _, output) = time_operation_with_setup(setup, f);
            elapsed = elapsed.saturating_add(iteration_elapsed);
            std::hint::black_box(output?);
        }
        if elapsed >= target || iterations >= 1 << 32 {
            return Ok(iterations);
        }

        iterations = scaled_iterations(iterations, elapsed, target);
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

        iterations = scaled_iterations(iterations, elapsed, target);
    }
}

fn calibrate_result_iterations<F, R, E>(target: Duration, f: &mut F) -> Result<u64, E>
where
    F: FnMut() -> Result<R, E>,
{
    let mut iterations = 1_u64;
    loop {
        let start = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(std::hint::black_box(f())?);
        }
        let elapsed = start.elapsed();
        if elapsed >= target || iterations >= 1 << 32 {
            return Ok(iterations);
        }

        iterations = scaled_iterations(iterations, elapsed, target);
    }
}

fn scaled_iterations(iterations: u64, elapsed: Duration, target: Duration) -> u64 {
    let elapsed_ns = elapsed.as_nanos().max(1);
    let target_ns = target.as_nanos().max(1);
    let scale = (target_ns / elapsed_ns).clamp(2, 16);
    iterations
        .saturating_mul(u64::try_from(scale).unwrap_or(16))
        .max(1)
}

fn failed_micro_result<R, E>(error: E) -> (MeasurementState, Result<R, E>) {
    (
        MeasurementState {
            duration: Duration::ZERO,
            counters: CorrectnessCounters {
                attempted: 1,
                failures: 1,
                ..CorrectnessCounters::default()
            },
            operations_hint: Some(0),
            micro: Some(MicroMeasurement {
                iterations: 1,
                gross_elapsed: Duration::ZERO,
                overhead: Duration::ZERO,
                net_elapsed: Duration::ZERO,
            }),
            allocation: None,
        },
        Err(error),
    )
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

fn time_empty_iterations(iterations: u64) -> Duration {
    let mut elapsed = Duration::ZERO;
    for index in 0..iterations {
        let start = Instant::now();
        std::hint::black_box(index);
        elapsed = elapsed.saturating_add(start.elapsed());
    }
    elapsed
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
    use std::cell::Cell;

    fn fixed_ops_ctx(operations_per_sample: u64) -> StressContext {
        StressContext::new(
            2,
            BenchmarkMode::FixedOperations {
                operations_per_sample,
            },
        )
    }

    fn repeated_modes() -> [BenchmarkMode; 3] {
        [
            BenchmarkMode::Micro {
                target_sample_duration: Duration::from_secs(1),
            },
            BenchmarkMode::FixedDuration {
                sample_duration: Duration::from_secs(1),
            },
            BenchmarkMode::FixedOperations {
                operations_per_sample: 5,
            },
        ]
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
    fn measure_outcome_uses_observed_counts_and_a_typed_logical_unit() {
        let mut ctx = fixed_ops_ctx(3);

        let outcome = ctx.measure_outcome("write", LogicalUnit::new("record"), || {
            OperationOutcome::new(4, 3).failures(1)
        });

        let record = ctx.take_measurements().remove(0);
        assert_eq!(outcome.attempted, 12);
        assert_eq!(outcome.completed, 9);
        assert_eq!(outcome.failures, 3);
        assert_eq!(record.counters.attempted, 12);
        assert_eq!(record.counters.completed, 9);
        assert_eq!(record.counters.failures, 3);
        assert_eq!(record.operations_hint, Some(9));
        assert_eq!(
            record.parameters.get("logical_unit").map(String::as_str),
            Some("record")
        );
    }

    #[test]
    fn measure_outcome_does_not_count_micro_calibration_as_completed_work() {
        let mut ctx = StressContext::new(
            1,
            BenchmarkMode::Micro {
                target_sample_duration: Duration::from_micros(50),
            },
        );
        let mut calls = 0_u64;

        let outcome = ctx.measure_outcome("lookup", LogicalUnit::new("lookup"), || {
            calls = calls.saturating_add(1);
            OperationOutcome::success(1)
        });

        let record = ctx.take_measurements().remove(0);
        let measured_iterations = record.micro.expect("micro measurement").iterations;
        assert_eq!(outcome.completed, measured_iterations);
        assert!(
            calls > measured_iterations,
            "calibration should have run separately"
        );
    }

    #[test]
    fn measure_with_setup_rebuilds_consumed_input_for_every_measured_operation() {
        let mut ctx = fixed_ops_ctx(4);
        let mut setups = 0_u64;

        let result = ctx.measure_with_setup(
            "consume",
            || {
                setups = setups.saturating_add(1);
                vec![1_u8, 2, 3]
            },
            |mut input| input.pop(),
        );

        let record = ctx.take_measurements().remove(0);
        assert_eq!(result, Some(3));
        assert_eq!(setups, 4);
        assert_eq!(record.counters.completed, 4);
    }

    #[test]
    fn measure_result_stops_at_first_error_and_records_observed_counts() {
        let mut ctx = fixed_ops_ctx(5);
        let calls = Cell::new(0_u64);

        let result = ctx.measure_result("fallible", || {
            let call = calls.get();
            calls.set(call + 1);
            if call == 2 {
                Err("operation failed")
            } else {
                Ok(call)
            }
        });

        let record = ctx.take_measurements().remove(0);
        assert_eq!(result, Err("operation failed"));
        assert_eq!(calls.get(), 3);
        assert_eq!(record.counters.attempted, 3);
        assert_eq!(record.counters.completed, 2);
        assert_eq!(record.counters.failures, 1);
        assert_eq!(record.operations_hint, Some(2));
    }

    #[test]
    fn measure_result_never_runs_past_an_error_in_any_mode() {
        for mode in repeated_modes() {
            let mut ctx = StressContext::new(2, mode);
            let calls = Cell::new(0_u64);

            let result = ctx.measure_result("fallible", || {
                let call = calls.get();
                calls.set(call + 1);
                if call == 1 {
                    Err("operation failed")
                } else {
                    Ok(call)
                }
            });

            let record = ctx.take_measurements().remove(0);
            assert_eq!(result, Err("operation failed"));
            assert_eq!(calls.get(), 2, "work ran again after its first error");
            assert_eq!(record.counters.failures, 1);
            assert!(!record.counters.passed());
        }
    }

    #[test]
    fn pending_manual_counters_cannot_erase_an_observed_result_error() {
        let mut ctx = fixed_ops_ctx(3);
        let _ = ctx.correctness().attempted(1).completed(1);

        let result = ctx.measure_result("fallible", || Err::<(), _>("operation failed"));

        let record = ctx.take_measurements().remove(0);
        assert_eq!(result, Err("operation failed"));
        assert_eq!(record.counters.attempted, 1);
        assert_eq!(record.counters.completed, 0);
        assert_eq!(record.counters.failures, 1);
        assert_eq!(record.operations_hint, Some(0));
    }

    #[test]
    fn pending_operations_cannot_erase_an_observed_result_error() {
        let mut ctx = fixed_ops_ctx(3);
        ctx.operations(1);

        let result = ctx.measure_result("fallible", || Err::<(), _>("operation failed"));

        let record = ctx.take_measurements().remove(0);
        assert_eq!(result, Err("operation failed"));
        assert_eq!(record.counters.attempted, 1);
        assert_eq!(record.counters.completed, 0);
        assert_eq!(record.counters.failures, 1);
    }

    #[test]
    fn pending_manual_counters_cannot_erase_an_observed_outcome_error() {
        let mut ctx = fixed_ops_ctx(1);
        let _ = ctx.correctness().attempted(1).completed(1);

        ctx.measure_outcome("write", LogicalUnit::new("record"), || {
            OperationOutcome::new(1, 0).failures(1)
        });

        let record = ctx.take_measurements().remove(0);
        assert_eq!(record.counters.attempted, 1);
        assert_eq!(record.counters.completed, 0);
        assert_eq!(record.counters.failures, 1);
    }

    #[test]
    fn measure_result_with_setup_stops_and_excludes_later_setups() {
        let mut ctx = fixed_ops_ctx(5);
        let setups = Cell::new(0_u64);
        let calls = Cell::new(0_u64);

        let result = ctx.measure_result_with_setup(
            "fallible setup",
            || {
                setups.set(setups.get() + 1);
                vec![1_u8, 2, 3]
            },
            |mut input| {
                let call = calls.get();
                calls.set(call + 1);
                if call == 1 {
                    Err("consume failed")
                } else {
                    Ok(input.pop())
                }
            },
        );

        let record = ctx.take_measurements().remove(0);
        assert_eq!(result, Err("consume failed"));
        assert_eq!(setups.get(), 2);
        assert_eq!(calls.get(), 2);
        assert_eq!(record.counters.attempted, 2);
        assert_eq!(record.counters.completed, 1);
        assert_eq!(record.counters.failures, 1);
    }

    #[test]
    fn measure_result_async_stops_at_first_error() {
        let mut ctx = fixed_ops_ctx(4);
        let calls = Cell::new(0_u64);

        let result = crate::__private::block_on(ctx.measure_result_async("fallible async", || {
            let call = calls.get();
            calls.set(call + 1);
            async move {
                if call == 1 {
                    Err("async operation failed")
                } else {
                    Ok(call)
                }
            }
        }));

        let record = ctx.take_measurements().remove(0);
        assert_eq!(result, Err("async operation failed"));
        assert_eq!(calls.get(), 2);
        assert_eq!(record.counters.attempted, 2);
        assert_eq!(record.counters.completed, 1);
        assert_eq!(record.counters.failures, 1);
        assert_eq!(record.intent, MeasurementIntent::Async);
    }

    #[test]
    fn measure_result_async_never_runs_past_an_error_in_any_mode() {
        for mode in repeated_modes() {
            let mut ctx = StressContext::new(2, mode);
            let calls = Cell::new(0_u64);

            let result = crate::__private::block_on(ctx.measure_result_async("fallible", || {
                let call = calls.get();
                calls.set(call + 1);
                async move {
                    if call == 1 {
                        Err("operation failed")
                    } else {
                        Ok(call)
                    }
                }
            }));

            let record = ctx.take_measurements().remove(0);
            assert_eq!(result, Err("operation failed"));
            assert_eq!(calls.get(), 2, "work ran again after its first error");
            assert_eq!(record.counters.attempted, 2);
            assert_eq!(record.counters.completed, 1);
            assert_eq!(record.counters.failures, 1);
        }
    }

    #[test]
    fn measure_result_async_with_setup_stops_at_first_error() {
        let mut ctx = fixed_ops_ctx(4);
        let setups = Cell::new(0_u64);

        let result = crate::__private::block_on(ctx.measure_result_async_with_setup(
            "fallible async setup",
            || {
                let setup = setups.get();
                setups.set(setup + 1);
                setup
            },
            |input| async move {
                if input == 2 {
                    Err("async setup operation failed")
                } else {
                    Ok(input)
                }
            },
        ));

        let record = ctx.take_measurements().remove(0);
        assert_eq!(result, Err("async setup operation failed"));
        assert_eq!(setups.get(), 3);
        assert_eq!(record.counters.attempted, 3);
        assert_eq!(record.counters.completed, 2);
        assert_eq!(record.counters.failures, 1);
        assert_eq!(record.intent, MeasurementIntent::Async);
    }

    #[test]
    fn measure_outcome_with_setup_aggregates_observed_counts() {
        let mut ctx = fixed_ops_ctx(3);
        let setups = Cell::new(0_u64);

        let outcome = ctx.measure_outcome_with_setup(
            "setup outcome",
            LogicalUnit::new("record"),
            || {
                setups.set(setups.get() + 1);
                vec![1_u8, 2]
            },
            |input| {
                std::hint::black_box(input);
                OperationOutcome::new(2, 1).failures(1)
            },
        );

        let record = ctx.take_measurements().remove(0);
        assert_eq!(setups.get(), 3);
        assert_eq!(outcome.attempted, 6);
        assert_eq!(outcome.completed, 3);
        assert_eq!(outcome.failures, 3);
        assert_eq!(record.counters.attempted, 6);
        assert_eq!(record.counters.completed, 3);
        assert_eq!(record.counters.failures, 3);
        assert_eq!(record.parameters["logical_unit"], "record");
    }

    #[test]
    #[should_panic(expected = "logical unit cannot be empty")]
    fn logical_unit_rejects_empty_values() {
        let _ = LogicalUnit::new("  ");
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
    fn external_outcome_preserves_failures_and_logical_unit() {
        let mut ctx = fixed_ops_ctx(1);

        ctx.record_external_outcome(
            "remote",
            Duration::from_millis(10),
            LogicalUnit::new("request"),
            OperationOutcome::new(10, 8).failures(2),
        );

        let record = ctx.take_measurements().remove(0);
        assert_eq!(record.counters.attempted, 10);
        assert_eq!(record.counters.completed, 8);
        assert_eq!(record.counters.failures, 2);
        assert_eq!(record.operations_hint, Some(8));
        assert_eq!(record.parameters["logical_unit"], "request");
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
    #[should_panic(expected = "measured samples must be greater than zero")]
    fn builder_rejects_zero_measured_samples() {
        let mut ctx = fixed_ops_ctx(1);
        let _ = ctx.benchmark("lookup").samples(0);
    }

    #[test]
    fn builder_can_batch_fast_tier2_operations_per_sample() {
        let mut ctx = fixed_ops_ctx(1);

        ctx.benchmark("lookup")
            .operations_per_sample(32)
            .measure(|| std::hint::black_box(1_u64));

        let record = ctx.take_measurements().remove(0);
        assert_eq!(record.counters.completed, 32);
        assert_eq!(
            record.mode,
            BenchmarkMode::FixedOperations {
                operations_per_sample: 32
            }
        );
    }

    #[test]
    fn builder_measure_result_stops_inside_operations_override() {
        let mut ctx = fixed_ops_ctx(1);
        let calls = Cell::new(0_u64);

        let result = ctx
            .benchmark("fallible lookup")
            .operations_per_sample(5)
            .measure_result(|| {
                let call = calls.get();
                calls.set(call + 1);
                if call == 2 {
                    Err("lookup failed")
                } else {
                    Ok(call)
                }
            });

        let record = ctx.take_measurements().remove(0);
        assert_eq!(result, Err("lookup failed"));
        assert_eq!(calls.get(), 3);
        assert_eq!(record.counters.attempted, 3);
        assert_eq!(record.counters.completed, 2);
        assert_eq!(record.counters.failures, 1);
        assert_eq!(
            record.mode,
            BenchmarkMode::FixedOperations {
                operations_per_sample: 5
            }
        );
    }

    #[test]
    fn builder_measure_result_with_setup_stops_inside_operations_override() {
        let mut ctx = fixed_ops_ctx(1);
        let setups = Cell::new(0_u64);

        let result = ctx
            .benchmark("fallible consume")
            .operations_per_sample(5)
            .measure_result_with_setup(
                || {
                    let setup = setups.get();
                    setups.set(setup + 1);
                    setup
                },
                |input| {
                    if input == 2 {
                        Err("consume failed")
                    } else {
                        Ok(input)
                    }
                },
            );

        let record = ctx.take_measurements().remove(0);
        assert_eq!(result, Err("consume failed"));
        assert_eq!(setups.get(), 3);
        assert_eq!(record.counters.attempted, 3);
        assert_eq!(record.counters.completed, 2);
        assert_eq!(record.counters.failures, 1);
    }

    #[test]
    #[should_panic(expected = "operations_per_sample is available only for Tier 2")]
    fn builder_rejects_operation_count_override_for_other_tiers() {
        let mut ctx = StressContext::new(
            3,
            BenchmarkMode::FixedDuration {
                sample_duration: Duration::from_millis(1),
            },
        );

        let _ = ctx.benchmark("invalid").operations_per_sample(10);
    }

    #[test]
    fn builder_parameters_and_metadata_are_scoped_to_one_measurement() {
        let mut ctx = fixed_ops_ctx(1);

        ctx.benchmark("write")
            .parameter("payload_bytes", 4096)
            .metadata("owner", "storage")
            .role(TrustClass::Diagnostic)
            .logical_unit(LogicalUnit::new("record"))
            .measure(|| std::hint::black_box(1_u64));
        ctx.measure("read", || std::hint::black_box(2_u64));

        let records = ctx.take_measurements();
        assert_eq!(records[0].parameters["payload_bytes"], "4096");
        assert_eq!(records[0].parameters["logical_unit"], "record");
        assert_eq!(records[0].metadata["owner"], "storage");
        assert_eq!(records[0].metadata["trust_class"], "diagnostic");
        assert!(!records[1].parameters.contains_key("payload_bytes"));
        assert!(!records[1].parameters.contains_key("logical_unit"));
        assert!(!records[1].metadata.contains_key("owner"));
        assert!(!records[1].metadata.contains_key("trust_class"));
    }

    #[test]
    fn scalar_observations_attach_to_latest_measurement_with_typed_contract() {
        let mut ctx = fixed_ops_ctx(1);
        ctx.measure("write", || std::hint::black_box(1_u64));
        ctx.record_observation(
            "commits_per_fsync",
            8.0,
            ObservationUnit::Ratio,
            ObservationDirection::HigherIsBetter,
        );

        let record = ctx.take_measurements().remove(0);
        assert_eq!(record.observations.len(), 1);
        assert_eq!(record.observations[0].name, "commits_per_fsync");
        assert_eq!(record.observations[0].unit, ObservationUnit::Ratio);
        assert_eq!(
            record.observations[0].direction,
            ObservationDirection::HigherIsBetter
        );
    }

    #[test]
    #[should_panic(expected = "invalid is a derived trust class")]
    fn builder_rejects_invalid_as_an_authored_role() {
        let mut ctx = fixed_ops_ctx(1);
        let _ = ctx.benchmark("broken").role(TrustClass::Invalid);
    }

    #[test]
    #[should_panic(expected = "trust_class is reserved")]
    fn context_metadata_rejects_untyped_role_overrides() {
        let mut ctx = fixed_ops_ctx(1);
        let _ = ctx.metadata("trust_class", "gate");
    }

    #[test]
    #[should_panic(expected = "trust_class is reserved")]
    fn builder_metadata_rejects_untyped_role_overrides() {
        let mut ctx = fixed_ops_ctx(1);
        let _ = ctx.benchmark("broken").metadata("trust_class", "gatte");
    }

    #[test]
    fn builder_outcome_keeps_sample_overrides() {
        let mut ctx = fixed_ops_ctx(1);

        ctx.benchmark("write")
            .samples(7)
            .warmup(2)
            .measure_outcome(LogicalUnit::new("record"), || OperationOutcome::success(1));

        let record = ctx.take_measurements().remove(0);
        assert_eq!(record.overrides.samples, Some(7));
        assert_eq!(record.overrides.warmup_samples, Some(2));
        assert_eq!(record.parameters["logical_unit"], "record");
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
