//! Notices an encoder that takes a new rate and keeps going at the old one.
//!
//! Intel Quick Sync does: only a new session changes its rate, and new
//! sessions cost more than they gave back (see `docs/medicao-video.md`). Such
//! an encoder stays at its full rate.

use std::time::{Duration, Instant};

/// How much output one verdict is based on.
const WINDOW: Duration = Duration::from_secs(1);
/// Windows over the target in a row: what an encoder sent before it obeyed
/// fills one by itself.
const OVER_WINDOWS: u32 = 2;
/// How far over the target counts: a step down from 8 to 5.6 Mbps, with the
/// encoder still at 8, must show.
const OVER: f64 = 1.25;

pub(crate) struct RateWatch {
    over: u32,
    window_start: Instant,
    bytes: u64,
}

impl RateWatch {
    pub fn new(now: Instant) -> Self {
        Self {
            over: 0,
            window_start: now,
            bytes: 0,
        }
    }

    /// The encoder produced `bytes` while aiming at `target_bps`. Returns
    /// whether it has been seen ignoring that target. A screen at rest sends
    /// little whatever the encoder would obey, so only going over says
    /// anything.
    pub fn output(&mut self, bytes: usize, target_bps: u32, now: Instant) -> bool {
        self.bytes += bytes as u64;
        let elapsed = now.duration_since(self.window_start);
        if elapsed < WINDOW {
            return false;
        }
        let bps = self.bytes as f64 * 8.0 / elapsed.as_secs_f64();
        self.over = if bps > f64::from(target_bps) * OVER {
            self.over + 1
        } else {
            0
        };
        self.window_start = now;
        self.bytes = 0;
        self.over >= OVER_WINDOWS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);
    /// One second of output at `mbps`.
    const fn at(mbps: usize) -> usize {
        mbps * 125_000
    }

    /// `seconds` of output at `mbps`: whether any of them gave the verdict.
    fn run(
        watch: &mut RateWatch,
        mbps: usize,
        target: u32,
        seconds: u32,
        now: &mut Instant,
    ) -> bool {
        let mut ignored = false;
        for _ in 0..seconds {
            *now += SECOND;
            ignored |= watch.output(at(mbps), target, *now);
        }
        ignored
    }

    #[test]
    fn an_encoder_that_keeps_its_old_rate_is_noticed() {
        let mut now = Instant::now();
        let mut watch = RateWatch::new(now);
        assert!(run(&mut watch, 8, 5_600_000, 3, &mut now));
    }

    #[test]
    fn an_encoder_that_obeys_is_left_alone() {
        let mut now = Instant::now();
        let mut watch = RateWatch::new(now);
        assert!(!run(&mut watch, 8, 2_000_000, 1, &mut now));
        assert!(!run(&mut watch, 2, 2_000_000, 9, &mut now));
    }

    #[test]
    fn a_screen_at_rest_says_nothing_about_the_encoder() {
        let mut now = Instant::now();
        let mut watch = RateWatch::new(now);
        assert!(!run(&mut watch, 0, 2_000_000, 10, &mut now));
    }

    #[test]
    fn a_single_burst_says_nothing_either() {
        let mut now = Instant::now();
        let mut watch = RateWatch::new(now);
        for _ in 0..5 {
            assert!(!run(&mut watch, 8, 2_000_000, 1, &mut now));
            assert!(!run(&mut watch, 2, 2_000_000, 1, &mut now));
        }
    }
}
