//! Fixed-bucket log-linear histogram for recorded latencies.

/// Values below this are counted exactly, one bucket each.
const LINEAR_LIMIT: u64 = 128;

/// Sub-buckets per power of two above [`LINEAR_LIMIT`]: 64 gives a bucket
/// width of at most 1/64 of its lower bound (about two significant digits).
const SUB_BUCKETS: u64 = 64;
/// 128 exact buckets, then 64 sub-buckets for each exponent 7..=63.
const BUCKETS: usize = 128 + 57 * 64;

/// A mergeable histogram of non-negative integer values with fixed
/// log-linear buckets. Quantiles use the nearest-rank definition and report
/// the bucket midpoint, clamped to the exact maximum, so the relative error is
/// at most 1/128 of the true value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LatencyHistogram {
    counts: Vec<u64>,
    total: u64,
    max: u64,
}

impl LatencyHistogram {
    pub(crate) fn new() -> Self {
        Self {
            counts: vec![0; BUCKETS],
            total: 0,
            max: 0,
        }
    }

    pub(crate) fn record(&mut self, value: u64) {
        self.counts[bucket_index(value)] += 1;
        self.total += 1;
        self.max = self.max.max(value);
    }

    /// Adds every value of `other`; merging is exact.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn merge(&mut self, other: &Self) {
        for (count, added) in self.counts.iter_mut().zip(&other.counts) {
            *count += added;
        }
        self.total += other.total;
        self.max = self.max.max(other.max);
    }

    pub(crate) const fn count(&self) -> u64 {
        self.total
    }

    pub(crate) const fn max(&self) -> u64 {
        self.max
    }

    /// Nearest-rank quantile estimate; `None` when empty.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    pub(crate) fn quantile(&self, quantile: f64) -> Option<f64> {
        if self.total == 0 {
            return None;
        }
        let rank =
            ((quantile.clamp(0.0, 1.0) * self.total as f64).ceil() as u64).clamp(1, self.total);
        let mut seen = 0;
        for (index, count) in self.counts.iter().enumerate() {
            seen += count;
            if seen >= rank {
                let (lower, width) = bucket_bounds(index);
                let midpoint = lower as f64 + (width - 1) as f64 / 2.0;
                return Some(midpoint.min(self.max as f64));
            }
        }
        Some(self.max as f64)
    }
}

fn bucket_index(value: u64) -> usize {
    if value < LINEAR_LIMIT {
        return usize::try_from(value).expect("small value fits usize");
    }
    let exponent = u64::from(value.ilog2());
    let sub = value >> (exponent - 6);
    usize::try_from(LINEAR_LIMIT + (exponent - 7) * SUB_BUCKETS + (sub - SUB_BUCKETS))
        .expect("bucket index fits usize")
}

/// Lower bound and width of a bucket.
fn bucket_bounds(index: usize) -> (u64, u64) {
    let index = index as u64;
    if index < LINEAR_LIMIT {
        return (index, 1);
    }
    let offset = index - LINEAR_LIMIT;
    let shift = offset / SUB_BUCKETS + 1;
    let sub = offset % SUB_BUCKETS + SUB_BUCKETS;
    (sub << shift, 1 << shift)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    fn exact(sorted: &[u64], quantile: f64) -> f64 {
        let rank = ((quantile * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
        sorted[rank - 1] as f64
    }

    fn assert_close(values: &mut [u64], quantiles: &[f64]) {
        let mut histogram = LatencyHistogram::new();
        for value in values.iter() {
            histogram.record(*value);
        }
        values.sort_unstable();
        for quantile in quantiles {
            let estimate = histogram.quantile(*quantile).expect("non-empty");
            let truth = exact(values, *quantile);
            let error = (estimate - truth).abs() / truth.max(1.0);
            assert!(
                error <= 1.0 / 64.0,
                "q={quantile} estimate={estimate} exact={truth} error={error}"
            );
        }
        assert_eq!(histogram.max(), *values.last().unwrap());
    }

    /// Deterministic xorshift for reproducible distributions.
    fn uniform(state: &mut u64) -> f64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        #[allow(clippy::cast_precision_loss)]
        let unit = (*state >> 11) as f64 / (1_u64 << 53) as f64;
        unit
    }

    const QUANTILES: [f64; 5] = [0.5, 0.9, 0.99, 0.999, 1.0];

    #[test]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn uniform_values_stay_within_bucket_precision() {
        let mut state = 0x9E37_79B9_7F4A_7C15;
        let mut values = (0..20_000)
            .map(|_| 1_000 + (uniform(&mut state) * 1_000_000.0) as u64)
            .collect::<Vec<_>>();
        assert_close(&mut values, &QUANTILES);
    }

    #[test]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn exponential_tail_stays_within_bucket_precision() {
        let mut state = 42;
        let mut values = (0..50_000)
            .map(|_| (-(1.0 - uniform(&mut state)).ln() * 250_000.0) as u64 + 1)
            .collect::<Vec<_>>();
        assert_close(&mut values, &QUANTILES);
    }

    #[test]
    fn small_values_are_exact_and_huge_values_do_not_panic() {
        let mut values = (0..LINEAR_LIMIT).collect::<Vec<_>>();
        assert_close(&mut values, &QUANTILES);
        let mut histogram = LatencyHistogram::new();
        histogram.record(u64::MAX);
        histogram.record(0);
        assert_eq!(histogram.count(), 2);
        assert_eq!(histogram.max(), u64::MAX);
        assert!(histogram.quantile(1.0).unwrap() > 1.8e19);
    }

    #[test]
    fn merged_histograms_equal_one_histogram_of_all_values() {
        let mut left = LatencyHistogram::new();
        let mut right = LatencyHistogram::new();
        let mut all = LatencyHistogram::new();
        for value in 1..5_000_u64 {
            let value = value * 37 % 100_003;
            if value % 3 == 0 {
                left.record(value);
            } else {
                right.record(value);
            }
            all.record(value);
        }
        left.merge(&right);
        assert_eq!(left, all);
        assert_eq!(left.count(), 4_999);
    }

    #[test]
    fn empty_histogram_has_no_quantiles() {
        assert!(LatencyHistogram::new().quantile(0.5).is_none());
    }
}
