//! Benchmark result types.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

/// Result of a single benchmark measurement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchResult {
    /// Full name including suite: "suite/benchmark"
    pub name: String,
    /// Measured duration (median if multiple runs)
    #[serde(with = "duration_serde")]
    pub duration: Duration,
    /// Bytes processed (for throughput calculation)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Elements/operations processed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elements: Option<u64>,
    /// All individual run durations
    #[serde(with = "duration_vec_serde")]
    pub all_runs: Vec<Duration>,
    /// Custom tags
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tags: HashMap<String, String>,
}

impl BenchResult {
    /// Calculate bytes per second throughput.
    pub fn bytes_per_sec(&self) -> Option<f64> {
        self.bytes.map(|b| b as f64 / self.duration.as_secs_f64())
    }

    /// Calculate elements per second throughput.
    pub fn elements_per_sec(&self) -> Option<f64> {
        self.elements
            .map(|e| e as f64 / self.duration.as_secs_f64())
    }

    /// Get minimum duration across all runs.
    pub fn min_duration(&self) -> Duration {
        self.all_runs.iter().copied().min().unwrap_or(self.duration)
    }

    /// Get maximum duration across all runs.
    pub fn max_duration(&self) -> Duration {
        self.all_runs.iter().copied().max().unwrap_or(self.duration)
    }

    /// Get standard deviation of durations.
    pub fn std_dev(&self) -> Option<Duration> {
        if self.all_runs.len() < 2 {
            return None;
        }
        let mean =
            self.all_runs.iter().map(|d| d.as_secs_f64()).sum::<f64>() / self.all_runs.len() as f64;
        let variance = self
            .all_runs
            .iter()
            .map(|d| {
                let diff = d.as_secs_f64() - mean;
                diff * diff
            })
            .sum::<f64>()
            / (self.all_runs.len() - 1) as f64;
        Some(Duration::from_secs_f64(variance.sqrt()))
    }

    /// Compare against a baseline result.
    ///
    /// Returns the ratio: `self.duration / baseline.duration`.
    /// - `< 1.0` means faster (improvement)
    /// - `> 1.0` means slower (regression)
    pub fn compare(&self, baseline: &BenchResult) -> f64 {
        self.duration.as_secs_f64() / baseline.duration.as_secs_f64()
    }

    /// Check if this result is a regression against baseline.
    ///
    /// A result is a regression if it's more than `threshold` percent slower.
    /// Default threshold is 5% (0.05).
    pub fn is_regression(&self, baseline: &BenchResult, threshold: f64) -> bool {
        self.compare(baseline) > 1.0 + threshold
    }
}

/// Results for an entire benchmark suite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteResult {
    /// Suite name
    pub suite: String,
    /// Individual benchmark results
    pub results: Vec<BenchResult>,
    /// Total suite duration
    #[serde(with = "duration_serde")]
    pub total_duration: Duration,
    /// Timestamp when suite started
    pub started_at: String,
    /// Git commit hash (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
    /// Custom metadata
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
}

impl SuiteResult {
    /// Load a suite result from JSON file.
    pub fn load(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Compare this suite against a baseline.
    ///
    /// Returns a map of benchmark name to ratio (self/baseline).
    pub fn compare(&self, baseline: &SuiteResult) -> HashMap<String, f64> {
        let baseline_map: HashMap<_, _> = baseline.results.iter().map(|r| (&r.name, r)).collect();

        self.results
            .iter()
            .filter_map(|r| {
                baseline_map
                    .get(&r.name)
                    .map(|b| (r.name.clone(), r.compare(b)))
            })
            .collect()
    }

    /// Find regressions compared to baseline.
    ///
    /// Returns benchmarks that are more than `threshold` percent slower.
    pub fn find_regressions(
        &self,
        baseline: &SuiteResult,
        threshold: f64,
    ) -> Vec<(&BenchResult, f64)> {
        let baseline_map: HashMap<_, _> = baseline.results.iter().map(|r| (&r.name, r)).collect();

        self.results
            .iter()
            .filter_map(|r| {
                baseline_map.get(&r.name).and_then(|b| {
                    let ratio = r.compare(b);
                    if ratio > 1.0 + threshold {
                        Some((r, ratio))
                    } else {
                        None
                    }
                })
            })
            .collect()
    }
}

mod duration_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        d.as_nanos().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let nanos = u128::deserialize(d)?;
        Ok(Duration::from_nanos(nanos as u64))
    }
}

mod duration_vec_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(v: &[Duration], s: S) -> Result<S::Ok, S::Error> {
        v.iter()
            .map(|d| d.as_nanos())
            .collect::<Vec<_>>()
            .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Duration>, D::Error> {
        let nanos: Vec<u128> = Vec::deserialize(d)?;
        Ok(nanos
            .into_iter()
            .map(|n| Duration::from_nanos(n as u64))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_calculate_throughput_when_bytes_set() {
        let result = BenchResult {
            name: "test".to_string(),
            duration: Duration::from_secs(1),
            bytes: Some(1_000_000),
            elements: None,
            all_runs: vec![Duration::from_secs(1)],
            tags: HashMap::new(),
        };
        assert_eq!(result.bytes_per_sec(), Some(1_000_000.0));
    }

    #[test]
    fn should_detect_regression_when_slower() {
        let baseline = BenchResult {
            name: "test".to_string(),
            duration: Duration::from_millis(100),
            bytes: None,
            elements: None,
            all_runs: vec![],
            tags: HashMap::new(),
        };
        let current = BenchResult {
            name: "test".to_string(),
            duration: Duration::from_millis(120),
            bytes: None,
            elements: None,
            all_runs: vec![],
            tags: HashMap::new(),
        };
        assert!(current.is_regression(&baseline, 0.05)); // 20% slower > 5% threshold
    }

    #[test]
    fn should_not_detect_regression_when_within_threshold() {
        let baseline = BenchResult {
            name: "test".to_string(),
            duration: Duration::from_millis(100),
            bytes: None,
            elements: None,
            all_runs: vec![],
            tags: HashMap::new(),
        };
        let current = BenchResult {
            name: "test".to_string(),
            duration: Duration::from_millis(103),
            bytes: None,
            elements: None,
            all_runs: vec![],
            tags: HashMap::new(),
        };
        assert!(!current.is_regression(&baseline, 0.05)); // 3% slower < 5% threshold
    }
}
