//! Rolling live-activity histogram for the harness status bar sparkline.
//!
//! Tracks recent agent events (tools, token batches) over a short wall-clock
//! window so the sparkline answers "is this turn still moving?" rather than
//! historical session archaeology.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Number of bars in the status-bar sparkline.
pub const LIVE_ACTIVITY_BUCKETS: usize = 24;

/// Wall-clock window covered by the sparkline (oldest → newest).
pub const LIVE_ACTIVITY_WINDOW: Duration = Duration::from_secs(90);

#[derive(Debug, Clone)]
pub struct LiveActivity {
    /// Event timestamps within the window (newest at back).
    events: VecDeque<Instant>,
    /// Sample counter so streaming tokens do not flood the series.
    token_sample: u32,
}

impl Default for LiveActivity {
    fn default() -> Self {
        Self {
            events: VecDeque::with_capacity(256),
            token_sample: 0,
        }
    }
}

impl LiveActivity {
    pub fn push_tool(&mut self) {
        self.record(2);
    }

    pub fn push_tool_result(&mut self) {
        self.record(1);
    }

    /// Count a stream chunk occasionally so generation shows as activity.
    pub fn push_token_sample(&mut self) {
        self.token_sample = self.token_sample.wrapping_add(1);
        if self.token_sample % 16 == 1 {
            self.record(1);
        }
    }

    pub fn push_turn_boundary(&mut self) {
        self.record(1);
    }

    fn record(&mut self, weight: u32) {
        let now = Instant::now();
        for _ in 0..weight.max(1) {
            self.events.push_back(now);
        }
        self.prune(now);
        // Cap memory if something floods us.
        while self.events.len() > 2000 {
            self.events.pop_front();
        }
    }

    fn prune(&mut self, now: Instant) {
        let cutoff = now.checked_sub(LIVE_ACTIVITY_WINDOW).unwrap_or(now);
        while self.events.front().is_some_and(|t| *t < cutoff) {
            self.events.pop_front();
        }
    }

    /// Bucket counts oldest → newest (length [`LIVE_ACTIVITY_BUCKETS`]).
    pub fn histogram(&mut self) -> Vec<u64> {
        let now = Instant::now();
        self.prune(now);
        let n = LIVE_ACTIVITY_BUCKETS;
        let mut hist = vec![0u64; n];
        if self.events.is_empty() {
            return hist;
        }
        let window = LIVE_ACTIVITY_WINDOW.as_secs_f64().max(0.001);
        let start = now.checked_sub(LIVE_ACTIVITY_WINDOW).unwrap_or(now);
        for t in &self.events {
            let age = t.saturating_duration_since(start).as_secs_f64();
            let ratio = (age / window).clamp(0.0, 0.999_999);
            let idx = (ratio * n as f64) as usize;
            hist[idx.min(n - 1)] = hist[idx.min(n - 1)].saturating_add(1);
        }
        hist
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_histogram_is_zeros() {
        let mut a = LiveActivity::default();
        let h = a.histogram();
        assert_eq!(h.len(), LIVE_ACTIVITY_BUCKETS);
        assert!(h.iter().all(|&v| v == 0));
    }

    #[test]
    fn tool_events_fill_recent_buckets() {
        let mut a = LiveActivity::default();
        a.push_tool();
        a.push_tool_result();
        let h = a.histogram();
        assert!(h.iter().sum::<u64>() >= 3);
        // Newest buckets are toward the end.
        assert!(h[h.len() - 1] > 0 || h[h.len() - 2] > 0);
    }
}
