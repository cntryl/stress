//! Rank-based stationarity checks behind `insufficient_warmup` and
//! `measurement_drift`.
//!
//! These functions only read sample values in recorded order (samples
//! without a value for the primary metric are skipped). They never
//! change measurements; callers turn findings into Info diagnostics.

/// Minimum measured samples before either check is evaluated.
pub(crate) const MIN_MEASURED_SAMPLES: usize = 10;
/// Minimum warmup samples before either check is evaluated.
pub(crate) const MIN_WARMUP_SAMPLES: usize = 2;

/// Evidence that the warmup tail sits at a different level than the start of
/// the measured samples.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WarmupFinding {
    pub tail_median: f64,
    pub tail_ci: (f64, f64),
    pub head_median: f64,
    pub head_ci: (f64, f64),
    pub shift_percent: f64,
    pub suggested_warmup_samples: usize,
}

/// Evidence of a monotonic trend across the measured samples.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DriftFinding {
    pub slope: f64,
    pub slope_ci: (f64, f64),
    pub drift_percent: f64,
}

/// Whether the sample counts are large enough to evaluate either check.
pub(crate) fn eligible(warmup: &[f64], measured: &[f64]) -> bool {
    measured.len() >= MIN_MEASURED_SAMPLES && warmup.len() >= MIN_WARMUP_SAMPLES
}

/// Upper bound on measured samples for the O(n^2) Theil-Sen slope set.
const MAX_DRIFT_SAMPLES: usize = 2_000;
/// Normal quantile for the two-sided 99.9% Sen slope interval. The strict
/// level keeps stationary false positives well under 1%.
const SEN_Z: f64 = 3.29;
/// Minimum total drift, as a fraction of the median, worth reporting.
const MIN_DRIFT_FRACTION: f64 = 0.10;
/// A warmup level shift must exceed this many robust standard deviations of
/// the measured samples, so ranks alone never fire on small samples.
const WARMUP_SHIFT_SIGMAS: f64 = 4.0;
/// A warmup level shift must also exceed this fraction of the head median.
const MIN_WARMUP_SHIFT_FRACTION: f64 = 0.01;
/// Band, in robust standard deviations, that counts a sample as settled.
const SETTLED_SIGMAS: f64 = 3.0;
/// Warmup tail length examined.
const WARMUP_TAIL: usize = 5;
/// Minimum measured head length examined.
const MIN_HEAD: usize = 5;
/// 1.4826 * MAD estimates the standard deviation of normal data.
const MAD_TO_SIGMA: f64 = 1.4826;

fn all_finite(values: &[f64]) -> bool {
    values.iter().all(|value| value.is_finite())
}

fn sorted(values: &[f64]) -> Vec<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted
}

fn median(values: &[f64]) -> f64 {
    crate::artifact::percentile_sorted(&sorted(values), 0.5)
}

/// Median and its distribution-free 95% order-statistic interval.
fn median_with_ci(values: &[f64]) -> (f64, (f64, f64)) {
    let sorted = sorted(values);
    let ci = crate::artifact::quantile_confidence_interval_95(&sorted, 0.5);
    (
        crate::artifact::percentile_sorted(&sorted, 0.5),
        (ci.lower, ci.upper),
    )
}

/// Robust standard deviation estimate (scaled median absolute deviation).
fn robust_sigma(values: &[f64]) -> f64 {
    let center = median(values);
    let deviations = values
        .iter()
        .map(|value| (value - center).abs())
        .collect::<Vec<_>>();
    MAD_TO_SIGMA * median(&deviations)
}

/// `insufficient_warmup`: the median of the last (up to 5) warmup samples and
/// the median of the first `max(5, ceil(n/3))` measured samples have
/// non-overlapping distribution-free 95% confidence intervals, and the shift
/// between them exceeds both 4 robust standard deviations of the measured
/// samples and 1% of the head median.
pub(crate) fn insufficient_warmup(warmup: &[f64], measured: &[f64]) -> Option<WarmupFinding> {
    if !eligible(warmup, measured) || !all_finite(warmup) || !all_finite(measured) {
        return None;
    }
    let tail = &warmup[warmup.len().saturating_sub(WARMUP_TAIL)..];
    let head_len = MIN_HEAD.max(measured.len().div_ceil(3)).min(measured.len());
    let head = &measured[..head_len];
    let (tail_median, tail_ci) = median_with_ci(tail);
    let (head_median, head_ci) = median_with_ci(head);
    let separated = tail_ci.0 > head_ci.1 || tail_ci.1 < head_ci.0;
    let shift = (tail_median - head_median).abs();
    let sigma = robust_sigma(measured);
    if !separated
        || shift <= WARMUP_SHIFT_SIGMAS * sigma
        || shift <= MIN_WARMUP_SHIFT_FRACTION * head_median.abs()
    {
        return None;
    }
    // Where the level settles: leading measured samples outside a band
    // around the median of the second half are still warming up.
    let settled = median(&measured[measured.len() / 2..]);
    let band = SETTLED_SIGMAS * sigma;
    let unsettled = measured
        .iter()
        .take_while(|value| (**value - settled).abs() > band)
        .count();
    Some(WarmupFinding {
        tail_median,
        tail_ci,
        head_median,
        head_ci,
        shift_percent: if head_median == 0.0 {
            f64::INFINITY
        } else {
            (tail_median - head_median) / head_median.abs() * 100.0
        },
        // Capped so a run that also drifts does not suggest warming up for
        // most of the run.
        suggested_warmup_samples: (warmup.len() * 2)
            .max(warmup.len() + unsettled.min(measured.len() / 2)),
    })
}

/// `measurement_drift`: the Theil-Sen slope over measured samples in
/// execution order has a Sen 99.9% rank confidence interval (Kendall tau
/// variance, no tie correction) that excludes zero, and the total drift
/// `slope * (n - 1)` exceeds 10% of the measured median.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub(crate) fn measurement_drift(warmup: &[f64], measured: &[f64]) -> Option<DriftFinding> {
    if !eligible(warmup, measured) || !all_finite(measured) || measured.len() > MAX_DRIFT_SAMPLES {
        return None;
    }
    let n = measured.len();
    let mut slopes = Vec::with_capacity(n * (n - 1) / 2);
    for (i, left) in measured.iter().enumerate() {
        for (offset, right) in measured[i + 1..].iter().enumerate() {
            slopes.push((right - left) / (offset + 1) as f64);
        }
    }
    slopes.sort_by(f64::total_cmp);
    let pairs = slopes.len() as f64;
    let n_f = n as f64;
    let spread = SEN_Z * (n_f * (n_f - 1.0) * (2.0 * n_f + 5.0) / 18.0).sqrt();
    // 1-based ranks M1 = (N - C) / 2 and M2 = (N + C) / 2 + 1.
    let lower_rank = ((pairs - spread) / 2.0).round().max(1.0) as usize;
    let upper_rank = (f64::midpoint(pairs, spread).round() as usize + 1).min(slopes.len());
    let slope_ci = (slopes[lower_rank - 1], slopes[upper_rank - 1]);
    let slope = crate::artifact::percentile_sorted(&slopes, 0.5);
    let center = median(measured);
    if center == 0.0 {
        return None;
    }
    let drift_fraction = slope * (n_f - 1.0) / center.abs();
    let excludes_zero = slope_ci.0 > 0.0 || slope_ci.1 < 0.0;
    (excludes_zero && drift_fraction.abs() > MIN_DRIFT_FRACTION).then_some(DriftFinding {
        slope,
        slope_ci,
        drift_percent: drift_fraction * 100.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tiny deterministic xorshift64* generator; no external dependencies.
    struct XorShift(u64);

    impl XorShift {
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        #[allow(clippy::cast_precision_loss)]
        fn uniform(&mut self) -> f64 {
            // 53 random bits mapped into (0, 1).
            ((self.next_u64() >> 11) as f64 + 0.5) / (1_u64 << 53) as f64
        }

        /// Standard normal via Box-Muller.
        fn gaussian(&mut self) -> f64 {
            let u1 = self.uniform();
            let u2 = self.uniform();
            (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
        }
    }

    fn stationary(rng: &mut XorShift, len: usize, rsd: f64) -> Vec<f64> {
        (0..len)
            .map(|_| 1_000.0 * (1.0 + rsd * rng.gaussian()).max(0.01))
            .collect()
    }

    #[test]
    fn checks_are_not_evaluated_below_minimum_counts() {
        let ramp = (0..30)
            .map(|index| 100.0 + f64::from(index) * 10.0)
            .collect::<Vec<_>>();
        assert!(measurement_drift(&[100.0], &ramp).is_none());
        assert!(measurement_drift(&[100.0, 100.0], &ramp[..9]).is_none());
        let flat = vec![100.0; 30];
        assert!(insufficient_warmup(&[500.0], &flat).is_none());
        assert!(insufficient_warmup(&[500.0, 500.0], &flat[..9]).is_none());
    }

    #[test]
    fn stationary_noise_rarely_fires_either_check() {
        const TRIALS: usize = 4_000;
        let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
        for measured_len in [5_usize, 10, 11, 30] {
            for warmup_len in [2_usize, 5] {
                for rsd in [0.01, 0.10, 0.30] {
                    let mut warmup_hits = 0;
                    let mut drift_hits = 0;
                    for _ in 0..TRIALS {
                        let warmup = stationary(&mut rng, warmup_len, rsd);
                        let measured = stationary(&mut rng, measured_len, rsd);
                        warmup_hits +=
                            usize::from(insufficient_warmup(&warmup, &measured).is_some());
                        drift_hits += usize::from(measurement_drift(&warmup, &measured).is_some());
                    }
                    // < 1% false positives.
                    assert!(
                        warmup_hits * 100 < TRIALS,
                        "insufficient_warmup fired {warmup_hits}/{TRIALS} at N={measured_len} W={warmup_len} rsd={rsd}"
                    );
                    assert!(
                        drift_hits * 100 < TRIALS,
                        "measurement_drift fired {drift_hits}/{TRIALS} at N={measured_len} W={warmup_len} rsd={rsd}"
                    );
                }
            }
        }
    }

    #[test]
    fn step_down_after_short_warmup_is_detected_with_a_warmup_estimate() {
        let mut rng = XorShift(42);
        // Two cold warmup samples, then three still-settling measured samples.
        let warmup = vec![1_600.0, 1_500.0];
        let mut measured = vec![1_450.0, 1_400.0, 1_350.0];
        measured.extend(stationary(&mut rng, 17, 0.01));
        let finding = insufficient_warmup(&warmup, &measured).expect("step down detected");
        assert!(finding.tail_ci.0 > finding.head_ci.1, "{finding:?}");
        assert!(finding.shift_percent > 10.0, "{finding:?}");
        assert!(finding.suggested_warmup_samples >= 5, "{finding:?}");
        assert!(finding.suggested_warmup_samples <= 10, "{finding:?}");
    }

    #[test]
    fn warmup_suggestion_is_capped_when_the_run_also_drifts() {
        let warmup = vec![3_000.0, 2_900.0];
        let measured = (0..20)
            .map(|index| 2_000.0 - 50.0 * f64::from(index))
            .collect::<Vec<_>>();
        if let Some(finding) = insufficient_warmup(&warmup, &measured) {
            assert!(finding.suggested_warmup_samples <= warmup.len() + measured.len() / 2);
        }
    }

    #[test]
    fn linear_ramp_is_detected_as_drift() {
        let mut rng = XorShift(7);
        let measured = (0..20)
            .map(|index| 1_000.0 * (1.0 + 0.02 * f64::from(index)) * (1.0 + 0.01 * rng.gaussian()))
            .collect::<Vec<_>>();
        let finding = measurement_drift(&[1_000.0, 1_000.0], &measured).expect("ramp detected");
        assert!(finding.slope > 0.0);
        assert!(finding.slope_ci.0 > 0.0, "{finding:?}");
        assert!(finding.drift_percent > 10.0, "{finding:?}");

        let falling = measured.iter().rev().copied().collect::<Vec<_>>();
        let finding = measurement_drift(&[1_000.0, 1_000.0], &falling).expect("falling ramp");
        assert!(finding.slope_ci.1 < 0.0, "{finding:?}");
        assert!(finding.drift_percent < -10.0, "{finding:?}");
    }

    #[test]
    fn small_significant_trends_below_ten_percent_are_ignored() {
        let measured = (0..30)
            .map(|index| 1_000.0 + f64::from(index))
            .collect::<Vec<_>>();
        assert!(measurement_drift(&[1_000.0, 1_000.0], &measured).is_none());
    }

    #[test]
    fn non_finite_values_skip_the_checks() {
        let mut measured = vec![100.0; 12];
        measured[3] = f64::NAN;
        assert!(measurement_drift(&[100.0, 100.0], &measured).is_none());
        assert!(insufficient_warmup(&[f64::INFINITY, 100.0], &[100.0; 12]).is_none());
    }
}
