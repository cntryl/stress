//! Process peak resident set size (RSS).
//!
//! The value is process-wide and monotonic: it is the high-water mark of the
//! whole benchmark process since it started, not a per-benchmark delta.

/// Peak resident set size of the current process in bytes, when the platform
/// exposes it.
///
/// Unix reads `getrusage(RUSAGE_SELF).ru_maxrss`, which macOS/iOS report in
/// bytes and other Unix platforms report in KiB. Other platforms return `None`.
#[cfg(unix)]
#[must_use]
pub(crate) fn peak_rss_bytes() -> Option<u64> {
    // SAFETY: `rusage` is plain old data, so an all-zero value is valid, and
    // `getrusage` only writes into the provided pointer.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `usage` is a valid, writable `rusage` for the call's duration.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &raw mut usage) } != 0 {
        return None;
    }
    let max_rss = u64::try_from(usage.ru_maxrss).ok()?;
    if max_rss == 0 {
        return None;
    }
    Some(max_rss.saturating_mul(RU_MAXRSS_UNIT_BYTES))
}

/// Peak resident set size is not collected on this platform.
#[cfg(not(unix))]
#[must_use]
pub(crate) fn peak_rss_bytes() -> Option<u64> {
    None
}

#[cfg(all(unix, any(target_os = "macos", target_os = "ios")))]
const RU_MAXRSS_UNIT_BYTES: u64 = 1;

#[cfg(all(unix, not(any(target_os = "macos", target_os = "ios"))))]
const RU_MAXRSS_UNIT_BYTES: u64 = 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn peak_rss_grows_after_touching_a_large_buffer() {
        // A modest buffer and a half-size bound: under memory pressure (for
        // example parallel test binaries) the OS may compress or reclaim
        // touched pages before the high-water mark reaches the full size.
        const BUFFER_BYTES: usize = 128 * 1024 * 1024;
        let before = peak_rss_bytes().expect("unix exposes peak RSS");
        assert!(
            before > 1024 * 1024,
            "peak RSS {before} is implausibly small"
        );

        let mut buffer = vec![0_u8; BUFFER_BYTES];
        for page in buffer.chunks_mut(4096) {
            page[0] = 1;
        }
        std::hint::black_box(&buffer);

        let after = peak_rss_bytes().expect("unix exposes peak RSS");
        // An absolute bound stays robust even if an earlier test already
        // raised the high-water mark and freed the memory.
        assert!(after >= before);
        assert!(
            after >= (BUFFER_BYTES / 2) as u64,
            "peak RSS {after} is below half of a touched {BUFFER_BYTES}-byte buffer"
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn peak_rss_is_unknown_off_unix() {
        assert_eq!(peak_rss_bytes(), None);
    }
}
