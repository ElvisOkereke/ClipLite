//! Test-mode support for the hardware-in-the-loop harness.
//!
//! `CLIPLITE_TEST_MODE=1` marks a harness run. The replay buffer starts from
//! persisted Settings through the normal launch path
//! (`commands::start_buffer_at_launch`), so test mode has no start path of its
//! own; its log lines carry a `[test-mode]` prefix.

/// True when `CLIPLITE_TEST_MODE` is exactly "1".
pub fn is_enabled(value: Option<&str>) -> bool {
    value == Some("1")
}

/// Test mode is on for this process.
pub fn active() -> bool {
    is_enabled(std::env::var("CLIPLITE_TEST_MODE").ok().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_only_for_exact_one() {
        assert!(is_enabled(Some("1")));
        assert!(!is_enabled(Some("0")));
        assert!(!is_enabled(None));
    }
}
