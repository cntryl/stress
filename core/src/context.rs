//! Benchmark context for timing control.

use std::time::{Duration, Instant};

/// Context passed to benchmark closures for timing control.
///
/// The closure must call exactly one timing method to record a duration.
pub struct StressContext {
    pub(crate) duration: Option<Duration>,
    pub(crate) bytes: Option<u64>,
    pub(crate) elements: Option<u64>,
    pub(crate) tags: Vec<(String, String)>,
}

impl StressContext {
    pub(crate) fn new() -> Self {
        Self {
            duration: None,
            bytes: None,
            elements: None,
            tags: Vec::new(),
        }
    }

    /// Record throughput in bytes processed.
    ///
    /// This enables bytes/sec reporting in results.
    pub fn set_bytes(&mut self, bytes: u64) {
        self.bytes = Some(bytes);
    }

    /// Record throughput in elements/operations processed.
    ///
    /// This enables ops/sec reporting in results.
    pub fn set_elements(&mut self, elements: u64) {
        self.elements = Some(elements);
    }

    /// Add a custom tag to this benchmark result.
    ///
    /// Tags are included in JSON output for filtering and grouping.
    pub fn tag(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.tags.push((key.into(), value.into()));
    }

    fn set_duration(&mut self, duration: Duration) {
        if self.duration.is_some() {
            panic!("Timing was recorded more than once for this benchmark.");
        }
        self.duration = Some(duration);
    }

    /// Time a single-shot operation. Call exactly once per benchmark.
    ///
    /// Everything before this is setup (not timed).
    /// Everything after this is teardown (not timed).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use cntryl_stress::StressContext;
    /// # fn example(ctx: &mut StressContext) {
    /// let data = prepare_expensive_data();  // Not timed
    ///
    /// let result = ctx.measure(|| {
    ///     process_data(&data)  // Timed
    /// });
    ///
    /// validate_result(&result);  // Not timed
    /// # }
    /// # fn prepare_expensive_data() -> Vec<u8> { vec![] }
    /// # fn process_data(_: &[u8]) -> bool { true }
    /// # fn validate_result(_: &bool) {}
    /// ```
    pub fn measure<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let start = Instant::now();
        let result = f();
        self.set_duration(start.elapsed());
        result
    }

    /// Time a repeated operation until the requested wall-clock budget is met.
    ///
    /// The closure is executed as many times as possible until the target
    /// duration has elapsed. The return value is the number of completed
    /// iterations, which is useful for reporting total throughput.
    #[must_use = "use the iteration count to report throughput totals"]
    pub fn measure_for<F>(&mut self, duration: Duration, mut f: F) -> usize
    where
        F: FnMut(),
    {
        let start = Instant::now();
        let mut iterations = 0usize;

        loop {
            f();
            iterations += 1;

            if start.elapsed() >= duration {
                break;
            }
        }

        self.set_duration(start.elapsed());
        iterations
    }

    /// Time an operation on a borrowed reference (avoids moves).
    ///
    /// Useful when you need to use the target after measurement.
    pub fn measure_ref<F, T, R>(&mut self, target: &T, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        let start = Instant::now();
        let result = f(target);
        self.set_duration(start.elapsed());
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
        result
    }

    /// Manually record a duration (for cases where you time externally).
    ///
    /// Use this when the timing happens inside the system under test.
    pub fn record_duration(&mut self, duration: Duration) {
        self.set_duration(duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_measure_duration_when_called() {
        let mut ctx = StressContext::new();
        ctx.measure(|| std::thread::sleep(Duration::from_millis(10)));

        let d = ctx.duration.unwrap();
        assert!(d >= Duration::from_millis(10));
        assert!(d < Duration::from_millis(100));
    }

    #[test]
    fn should_measure_duration_for_budget() {
        let mut ctx = StressContext::new();
        let iterations = ctx.measure_for(Duration::from_millis(5), || {
            std::hint::black_box(1usize);
        });

        assert!(iterations > 0);
        assert!(ctx.duration.unwrap() >= Duration::from_millis(5));
    }

    #[test]
    fn should_track_bytes_when_set() {
        let mut ctx = StressContext::new();
        ctx.set_bytes(1024);
        assert_eq!(ctx.bytes, Some(1024));
    }

    #[test]
    fn should_track_elements_when_set() {
        let mut ctx = StressContext::new();
        ctx.set_elements(100);
        assert_eq!(ctx.elements, Some(100));
    }

    #[test]
    fn should_collect_tags_when_added() {
        let mut ctx = StressContext::new();
        ctx.tag("env", "prod");
        ctx.tag("version", "1.0");
        assert_eq!(ctx.tags.len(), 2);
    }
}
