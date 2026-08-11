//! Structured benchmark-function errors.

use std::fmt;

/// Error returned by a stress benchmark function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StressError {
    message: String,
}

impl StressError {
    /// Create a stress error with an actionable message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Return the error message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for StressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StressError {}

impl From<String> for StressError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<&str> for StressError {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

/// Result returned by fallible stress benchmarks.
pub type StressResult = Result<(), StressError>;

/// Conversion used by `#[stress]` and the programmatic runner so benchmark
/// functions may idiomatically return either `()` or `Result<(), E>`.
#[doc(hidden)]
pub trait IntoStressResult {
    /// Convert the benchmark return value into a structured stress result.
    fn into_stress_result(self) -> StressResult;
}

impl IntoStressResult for () {
    fn into_stress_result(self) -> StressResult {
        Ok(())
    }
}

impl<E> IntoStressResult for Result<(), E>
where
    E: fmt::Display,
{
    fn into_stress_result(self) -> StressResult {
        self.map_err(|error| StressError::new(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_and_result_values_convert_to_stress_results() {
        assert_eq!(().into_stress_result(), Ok(()));
        assert_eq!(Ok::<(), &str>(()).into_stress_result(), Ok(()));
        assert_eq!(
            Err::<(), _>("transport failed").into_stress_result(),
            Err(StressError::new("transport failed"))
        );
    }
}
