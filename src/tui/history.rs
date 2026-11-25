//! Values the dashboard derives on the client side: the request rate per group (from
//! differences of `requests_total` between polls) and the countdown until a group sleeps.

use std::collections::VecDeque;

use crate::controller::GroupSnapshot;

/// One history bucket covers this much time of the source clock.
pub const SAMPLE_MS: u64 = 1000;
/// Buckets kept per group (about one minute).
pub const HISTORY: usize = 60;

/// Requests per second for one group, built from successive `requests_total` readings.
///
/// The dashboard redraws more often than the source updates, so readings are folded into
/// fixed buckets of [`SAMPLE_MS`]; a reading that arrives early is carried into the next
/// bucket and the sum stays exact.
#[derive(Debug, Clone, Default)]
pub struct RateHistory {
    /// Counter value and bucket boundary (source clock) of the last flushed bucket.
    baseline: Option<(u64, u64)>,
    samples: VecDeque<u64>,
}

impl RateHistory {
    /// Feed the counter value seen at `now_ms`.
    ///
    /// The first reading only sets the baseline, so a group that appears (or a dashboard
    /// that attaches) with a large counter does not draw a spike. A counter that went
    /// backwards means the daemon restarted: everything it counted since is new.
    pub fn observe(&mut self, now_ms: u64, total: u64) {
        let Some((last_total, last_at)) = self.baseline else {
            self.baseline = Some((total, now_ms));
            return;
        };
        if now_ms < last_at {
            // The clock stepped back: start over instead of waiting for it to catch up.
            self.baseline = Some((total, now_ms));
            return;
        }
        let buckets = (now_ms - last_at) / SAMPLE_MS;
        if buckets == 0 {
            return;
        }
        let delta = if total >= last_total { total - last_total } else { total };
        // Quiet buckets in between; everything counted since the last reading lands in the newest.
        let quiet = usize::try_from(buckets).unwrap_or(usize::MAX).min(HISTORY).saturating_sub(1);
        for _ in 0..quiet {
            self.push(0);
        }
        self.push(delta);
        self.baseline = Some((total, last_at.saturating_add(buckets.saturating_mul(SAMPLE_MS))));
    }

    /// Forget the baseline (source unreachable): the next reading must not count the whole
    /// outage as one burst. Collected buckets stay.
    pub fn pause(&mut self) {
        self.baseline = None;
    }

    fn push(&mut self, value: u64) {
        if self.samples.len() == HISTORY {
            self.samples.pop_front();
        }
        self.samples.push_back(value);
    }

    /// Buckets, oldest first.
    pub fn samples(&self) -> Vec<u64> {
        self.samples.iter().copied().collect()
    }

    /// The newest bucket, if any.
    pub fn latest(&self) -> Option<u64> {
        self.samples.back().copied()
    }
}

/// Time left until the idle timeout stops a group.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Countdown {
    /// Nothing to count down: the group is not running.
    Inactive,
    /// On hold: it is stopped and requests do not wake it.
    Held,
    Running {
        remaining_ms: u64,
        timeout_ms: u64,
        /// `remaining / timeout` within `0..=1`; `0` for a zero timeout.
        ratio: f64,
        /// Open connections that keep the group up once the time is over.
        inflight: u64,
    },
}

pub fn countdown(g: &GroupSnapshot, now_ms: u64) -> Countdown {
    if g.held {
        return Countdown::Held;
    }
    let Some(remaining_ms) = g.sleeps_in_ms(now_ms) else {
        return Countdown::Inactive;
    };
    let timeout_ms = u64::try_from(g.settings.idle_timeout.as_millis()).unwrap_or(u64::MAX);
    let ratio = if timeout_ms == 0 { 0.0 } else { (remaining_ms as f64 / timeout_ms as f64).clamp(0.0, 1.0) };
    Countdown::Running { remaining_ms, timeout_ms, ratio, inflight: g.inflight }
}

/// Number of filled cells of a `width` wide bar: any time left shows at least one cell,
/// a full bar only means "the whole timeout is left".
pub fn bar_cells(ratio: f64, width: usize) -> usize {
    if ratio.is_nan() || ratio <= 0.0 || width == 0 {
        return 0;
    }
    ((ratio * width as f64).ceil() as usize).clamp(1, width)
}

/// Sparkline text of exactly `width` characters: the newest `width` buckets, oldest on the
/// left, blank on the right while the history is still filling. A bucket without requests is
/// a dot, anything else one of eight levels. The scale never drops below four requests so a
/// single request does not fill the cell.
pub fn sparkline(samples: &[u64], width: usize) -> String {
    const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let shown = &samples[samples.len().saturating_sub(width)..];
    let scale = shown.iter().copied().max().unwrap_or(0).max(4);
    let mut out = String::with_capacity(width);
    for &v in shown {
        out.push(if v == 0 {
            '·'
        } else {
            let level = usize::try_from(v.saturating_mul(8).div_ceil(scale)).unwrap_or(8);
            LEVELS[level.clamp(1, 8) - 1]
        });
    }
    out.extend(std::iter::repeat_n(' ', width - shown.len()));
    out
}

/// `45s`, `14m 59s`, `2h 05m`, `3d 4h`.
pub fn fmt_compact(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        3600..86_400 => format!("{}h {:02}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d {}h", secs / 86_400, secs % 86_400 / 3600),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::controller::StateKind;
    use crate::test_support;

    fn running(timeout: Duration, last_activity_ago_ms: u64, now: u64) -> GroupSnapshot {
        let mut g = test_support::snapshot("g", StateKind::Running);
        g.settings.idle_timeout = timeout;
        g.last_activity_ms = Some(now - last_activity_ago_ms);
        g
    }

    #[test]
    fn rate_history_counts_differences_and_ignores_the_first_reading() {
        let mut h = RateHistory::default();
        h.observe(10_000, 5_000);
        assert!(h.samples().is_empty(), "attaching to a busy group must not draw a spike");
        h.observe(11_000, 5_003);
        h.observe(12_000, 5_003);
        h.observe(13_000, 5_010);
        assert_eq!(h.samples(), vec![3, 0, 7]);
        assert_eq!(h.latest(), Some(7));
    }

    #[test]
    fn rate_history_folds_early_readings_and_fills_gaps() {
        let mut h = RateHistory::default();
        h.observe(0, 0);
        h.observe(500, 2);
        assert!(h.samples().is_empty());
        h.observe(1_000, 3);
        assert_eq!(h.samples(), vec![3]);
        // Three seconds without a reading: two quiet buckets, then the burst.
        h.observe(4_000, 9);
        assert_eq!(h.samples(), vec![3, 0, 0, 6]);
    }

    #[test]
    fn rate_history_handles_counter_reset_clock_step_and_outage() {
        let mut h = RateHistory::default();
        h.observe(0, 100);
        h.observe(1_000, 110);
        // Daemon restarted: the counter started again and already reached 4.
        h.observe(2_000, 4);
        assert_eq!(h.samples(), vec![10, 4]);
        // Clock stepped back: no panic, no phantom bucket, counting resumes.
        h.observe(500, 6);
        h.observe(1_500, 8);
        assert_eq!(h.samples(), vec![10, 4, 2]);
        // Outage: the burst after it is not booked as one huge bucket.
        h.pause();
        h.observe(60_000, 1_000);
        assert_eq!(h.samples(), vec![10, 4, 2]);
        h.observe(61_000, 1_001);
        assert_eq!(h.samples(), vec![10, 4, 2, 1]);
    }

    #[test]
    fn rate_history_is_bounded() {
        let mut h = RateHistory::default();
        h.observe(0, 0);
        for i in 1..=(HISTORY as u64 * 3) {
            h.observe(i * SAMPLE_MS, i);
        }
        assert_eq!(h.samples().len(), HISTORY);
        // A huge gap does not allocate per elapsed second.
        h.observe(u64::MAX / 2, u64::MAX / 4);
        assert_eq!(h.samples().len(), HISTORY);
    }

    #[test]
    fn sparkline_pads_scales_and_marks_quiet_buckets() {
        assert_eq!(sparkline(&[], 4), "    ");
        assert_eq!(sparkline(&[0, 1, 4, 8], 6), "·▁▄█  ");
        // Only the newest `width` buckets are shown.
        assert_eq!(sparkline(&[8, 8, 0], 2), "█·");
        assert_eq!(sparkline(&[3], 0), "");
        // Small counts stay small.
        assert_eq!(sparkline(&[1], 1), "▂");
    }

    #[test]
    fn countdown_ratio_and_edge_cases() {
        let now = 1_000_000_000;
        let g = running(Duration::from_secs(100), 25_000, now);
        assert_eq!(
            countdown(&g, now),
            Countdown::Running { remaining_ms: 75_000, timeout_ms: 100_000, ratio: 0.75, inflight: 0 }
        );
        // Over: the stop is due, the bar is empty.
        let over = running(Duration::from_secs(10), 20_000, now);
        assert!(matches!(countdown(&over, now), Countdown::Running { remaining_ms: 0, ratio, .. } if ratio == 0.0));
        // Activity in the future (clock skew) never exceeds a full bar.
        let mut skew = running(Duration::from_secs(10), 0, now);
        skew.last_activity_ms = Some(now + 60_000);
        assert!(matches!(countdown(&skew, now), Countdown::Running { ratio, .. } if ratio == 1.0));
        // A zero timeout must not divide by zero.
        let zero = running(Duration::ZERO, 0, now);
        assert!(matches!(countdown(&zero, now), Countdown::Running { timeout_ms: 0, ratio, .. } if ratio == 0.0));
        // Not running: no deadline. Held wins over everything.
        assert_eq!(countdown(&test_support::snapshot("g", StateKind::Stopped), now), Countdown::Inactive);
        assert_eq!(countdown(&test_support::snapshot("g", StateKind::Starting), now), Countdown::Inactive);
        let mut held = running(Duration::from_secs(100), 0, now);
        held.held = true;
        assert_eq!(countdown(&held, now), Countdown::Held);
        // Without any request the timer starts at the last state change.
        let mut quiet = running(Duration::from_secs(100), 0, now);
        quiet.last_activity_ms = None;
        quiet.state_since_ms = now - 50_000;
        assert!(matches!(countdown(&quiet, now), Countdown::Running { remaining_ms: 50_000, .. }));
    }

    #[test]
    fn bar_cells_never_lie_about_empty_or_full() {
        assert_eq!(bar_cells(0.0, 10), 0);
        assert_eq!(bar_cells(f64::NAN, 10), 0);
        assert_eq!(bar_cells(0.001, 10), 1);
        assert_eq!(bar_cells(0.5, 10), 5);
        assert_eq!(bar_cells(1.0, 10), 10);
        assert_eq!(bar_cells(1.0, 0), 0);
    }

    #[test]
    fn compact_durations() {
        assert_eq!(fmt_compact(0), "0s");
        assert_eq!(fmt_compact(59_999), "59s");
        assert_eq!(fmt_compact(899_000), "14m 59s");
        assert_eq!(fmt_compact(7_500_000), "2h 05m");
        assert_eq!(fmt_compact(100 * 3600 * 1000), "4d 4h");
    }
}
