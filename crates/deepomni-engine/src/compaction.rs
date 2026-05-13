//! Compaction handler — triggers summary generation when token budget exceeded.

/// Tracks consecutive compaction failures for circuit breaker.
#[derive(Clone, Default)]
pub struct CompactionTracker {
    pub consecutive_failures: u32,
    pub total_compactions: u64,
    pub last_token_reduction: Option<(u64, u64)>,
}

impl CompactionTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if auto-compaction is allowed (circuit breaker).
    pub fn can_auto_compact(&self, max_consecutive_failures: u32) -> bool {
        self.consecutive_failures < max_consecutive_failures
    }

    /// Record a successful compaction.
    pub fn record_success(&mut self, before: u64, after: u64) {
        self.total_compactions += 1;
        self.consecutive_failures = 0;
        self.last_token_reduction = Some((before, after));
    }

    /// Record a failed compaction.
    pub fn record_failure(&mut self) {
        self.consecutive_failures += 1;
    }

    /// Whether compaction should be triggered for the given token ratio.
    pub fn needs_compaction(total_tokens: u64, context_window: u64, threshold: f64) -> bool {
        context_window > 0 && (total_tokens as f64 / context_window as f64) >= threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker() {
        let mut tracker = CompactionTracker::new();
        assert!(tracker.can_auto_compact(3));
        tracker.record_failure();
        tracker.record_failure();
        tracker.record_failure();
        assert!(!tracker.can_auto_compact(3));
    }

    #[test]
    fn test_success_resets_failures() {
        let mut tracker = CompactionTracker::new();
        tracker.record_failure();
        tracker.record_failure();
        tracker.record_success(1000, 500);
        assert!(tracker.can_auto_compact(3));
        assert_eq!(tracker.total_compactions, 1);
    }

    #[test]
    fn test_threshold_detection() {
        assert!(CompactionTracker::needs_compaction(900, 1000, 0.8));
        assert!(!CompactionTracker::needs_compaction(700, 1000, 0.8));
    }
}
