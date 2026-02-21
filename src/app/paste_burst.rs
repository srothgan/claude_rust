// claude_rust - A native Rust terminal interface for Claude Code
// Copyright (C) 2025  Simon Peter Rothgang
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Timing-based paste burst detection for terminals that don't support
//! bracketed paste (notably Windows Terminal with crossterm).
//!
//! Approach inspired by Codex CLI (openai/codex PR #9348):
//! measure the interval between consecutive key events. If characters arrive
//! faster than a threshold, they are part of a paste burst. Humans type at
//! ~200ms between keystrokes; 30ms is a safe upper bound for paste detection
//! on Windows (where terminals add ~10-15ms latency per pasted character).

use std::time::{Duration, Instant};

/// Maximum interval between consecutive key events to be considered part of
/// the same paste burst. Characters arriving faster than this are pasted, not typed.
#[cfg(not(windows))]
const BURST_INTERVAL: Duration = Duration::from_millis(8);

#[cfg(windows)]
const BURST_INTERVAL: Duration = Duration::from_millis(30);

/// Minimum number of key events in a burst to classify it as a paste.
/// A burst of 2-3 keys could be a fast typist or key repeat; require more.
const MIN_BURST_LEN: usize = 4;

/// Tracks rapid key events to detect paste bursts.
pub struct PasteBurstDetector {
    /// Timestamp of the last key event.
    last_key_time: Option<Instant>,
    /// Number of consecutive key events within the burst interval.
    burst_len: usize,
    /// Input line count when the current burst started.
    lines_before_burst: usize,
}

impl PasteBurstDetector {
    pub fn new() -> Self {
        Self { last_key_time: None, burst_len: 0, lines_before_burst: 1 }
    }

    /// Call this on every key event. Returns `true` if we're currently in a
    /// paste burst (rapid key input detected).
    pub fn on_key_event(&mut self, current_line_count: usize) -> bool {
        let now = Instant::now();
        if let Some(last) = self.last_key_time {
            if now.duration_since(last) <= BURST_INTERVAL {
                self.burst_len += 1;
            } else {
                // Gap too large - start a new burst
                self.burst_len = 1;
                self.lines_before_burst = current_line_count;
            }
        } else {
            // First key event
            self.burst_len = 1;
            self.lines_before_burst = current_line_count;
        }
        self.last_key_time = Some(now);
        self.is_paste()
    }

    /// Whether the current burst qualifies as a paste.
    #[must_use]
    pub fn is_paste(&self) -> bool {
        self.burst_len >= MIN_BURST_LEN
    }

    /// Whether key events are still arriving inside the burst interval.
    /// While this is true, the current burst is still "active".
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.last_key_time.is_some_and(|last| last.elapsed() <= BURST_INTERVAL)
    }

    /// Whether a detected paste burst has gone idle long enough to be treated as complete.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.is_paste() && !self.is_active()
    }

    /// Number of lines added since the burst started.
    #[must_use]
    pub fn lines_added(&self, current_line_count: usize) -> usize {
        current_line_count.saturating_sub(self.lines_before_burst)
    }

    /// Reset the burst state. Call after processing a completed burst
    /// (e.g. after converting to placeholder or after the drain cycle ends
    /// without a burst).
    pub fn reset(&mut self) {
        self.last_key_time = None;
        self.burst_len = 0;
    }
}

impl Default for PasteBurstDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_burst_on_single_key() {
        let mut d = PasteBurstDetector::new();
        assert!(!d.on_key_event(1));
    }

    #[test]
    fn no_burst_on_slow_typing() {
        let mut d = PasteBurstDetector::new();
        d.on_key_event(1);
        // Simulate slow typing by advancing time
        d.last_key_time = Instant::now().checked_sub(Duration::from_millis(200));
        assert!(!d.on_key_event(1));
    }

    #[test]
    fn burst_after_min_rapid_keys() {
        let mut d = PasteBurstDetector::new();
        for _ in 0..MIN_BURST_LEN {
            d.on_key_event(1);
        }
        assert!(d.is_paste());
    }

    #[test]
    fn reset_clears_burst() {
        let mut d = PasteBurstDetector::new();
        for _ in 0..MIN_BURST_LEN {
            d.on_key_event(1);
        }
        assert!(d.is_paste());
        d.reset();
        assert!(!d.is_paste());
    }

    #[test]
    fn lines_added_tracks_growth() {
        let mut d = PasteBurstDetector::new();
        d.on_key_event(1); // burst starts at 1 line
        d.on_key_event(3); // now 3 lines
        assert_eq!(d.lines_added(5), 4); // 5 - 1 = 4 lines added
    }

    #[test]
    fn active_while_recent_key() {
        let mut d = PasteBurstDetector::new();
        d.on_key_event(1);
        assert!(d.is_active());
    }

    #[test]
    fn settled_after_idle_gap() {
        let mut d = PasteBurstDetector::new();
        for _ in 0..MIN_BURST_LEN {
            d.on_key_event(1);
        }
        let idle_gap = BURST_INTERVAL + Duration::from_millis(1);
        d.last_key_time = Instant::now().checked_sub(idle_gap);
        assert!(d.is_settled());
    }
}
