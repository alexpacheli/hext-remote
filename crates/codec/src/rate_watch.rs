//! Notices an encoder that takes a new rate and keeps going at the old one.
//!
//! Intel Quick Sync does: only a new session changes its rate. A new session
//! costs a keyframe, so one is opened only when the output shows it is needed.

use std::time::{Duration, Instant};

/// How much output one verdict is based on.
const WINDOW: Duration = Duration::from_secs(1);
/// Windows over the target in a row: what an encoder sent before it obeyed
/// fills one by itself.
const OVER_WINDOWS: u32 = 2;
/// How far over the target counts: a step down from 8 to 5.6 Mbps, with the
/// encoder still at 8, must show.
const OVER: f64 = 1.25;
/// Each new session sends a keyframe and stops the capture for 100 to 300 ms.
const REOPEN_GAP: Duration = Duration::from_secs(5);

pub(crate) struct RateWatch {
    /// The rate of the first session, which is as high as it goes.
    max_bps: u32,
    /// The rate the session was opened with.
    session_bps: u32,
    /// Seen ignoring a new rate: from here on, only new sessions change it.
    ignores: bool,
    over: u32,
    window_start: Instant,
    bytes: u64,
    opened: Instant,
}

impl RateWatch {
    pub fn new(bitrate_bps: u32, now: Instant) -> Self {
        Self {
            max_bps: bitrate_bps,
            session_bps: bitrate_bps,
            ignores: false,
            over: 0,
            window_start: now,
            bytes: 0,
            opened: now,
        }
    }

    /// A session was opened at `bitrate_bps`, for whatever reason.
    pub fn opened(&mut self, bitrate_bps: u32, now: Instant) {
        *self = Self {
            max_bps: self.max_bps,
            ignores: self.ignores,
            ..Self::new(bitrate_bps, now)
        };
    }

    /// The encoder produced `bytes` while aiming at `target_bps`.
    pub fn output(&mut self, bytes: usize, target_bps: u32, now: Instant) {
        self.bytes += bytes as u64;
        let elapsed = now.duration_since(self.window_start);
        if elapsed < WINDOW {
            return;
        }
        let bps = self.bytes as f64 * 8.0 / elapsed.as_secs_f64();
        self.over = if bps > f64::from(target_bps) * OVER {
            self.over + 1
        } else {
            0
        };
        self.window_start = now;
        self.bytes = 0;
        // A screen at rest sends little, whatever the encoder would obey:
        // only going over says anything, and only below the session's rate.
        if self.over >= OVER_WINDOWS && target_bps < self.session_bps {
            self.ignores = true;
        }
    }

    /// Whether a new session is worth its keyframe now.
    pub fn reopen_due(&self, target_bps: u32, now: Instant) -> bool {
        let (target, session) = (u64::from(target_bps), u64::from(self.session_bps));
        let down = target * 4 < session * 3;
        // On the way up the pause comes on a good network, so big steps only.
        let up = target >= session * 2 || (target_bps == self.max_bps && target * 4 > session * 5);
        self.ignores && now.duration_since(self.opened) >= REOPEN_GAP && (down || up)
    }

    pub fn session_bps(&self) -> u32 {
        self.session_bps
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

    /// `seconds` of output at `mbps`, returning the time after them.
    fn run(
        watch: &mut RateWatch,
        mbps: usize,
        target: u32,
        seconds: u32,
        mut now: Instant,
    ) -> Instant {
        for _ in 0..seconds {
            now += SECOND;
            watch.output(at(mbps), target, now);
        }
        now
    }

    #[test]
    fn an_encoder_that_ignores_a_lower_rate_gets_a_new_session() {
        let start = Instant::now();
        let mut watch = RateWatch::new(8_000_000, start);
        let now = run(&mut watch, 8, 2_000_000, 5, start);
        assert!(watch.reopen_due(2_000_000, now));
    }

    #[test]
    fn an_encoder_that_obeys_keeps_its_session() {
        let start = Instant::now();
        let mut watch = RateWatch::new(8_000_000, start);
        let now = run(&mut watch, 8, 2_000_000, 1, start);
        let now = run(&mut watch, 2, 2_000_000, 9, now);
        assert!(!watch.reopen_due(2_000_000, now));
    }

    #[test]
    fn a_screen_at_rest_says_nothing_about_the_encoder() {
        let start = Instant::now();
        let mut watch = RateWatch::new(8_000_000, start);
        let now = run(&mut watch, 0, 2_000_000, 10, start);
        assert!(!watch.reopen_due(2_000_000, now));
    }

    #[test]
    fn new_sessions_keep_their_distance() {
        let start = Instant::now();
        let mut watch = RateWatch::new(8_000_000, start);
        let now = run(&mut watch, 8, 2_000_000, 5, start);
        watch.opened(2_000_000, now);
        assert!(!watch.reopen_due(1_000_000, now + SECOND * 4));
        assert!(watch.reopen_due(1_000_000, now + SECOND * 5));
    }

    /// After a new session, the output can stay over a target that low:
    /// another session at the same rate would only add keyframes.
    #[test]
    fn going_over_the_session_s_own_rate_changes_nothing() {
        let start = Instant::now();
        let mut watch = RateWatch::new(8_000_000, start);
        let now = run(&mut watch, 8, 1_000_000, 5, start);
        watch.opened(1_000_000, now);
        let now = run(&mut watch, 2, 1_000_000, 10, now);
        assert!(!watch.reopen_due(1_000_000, now));
    }

    #[test]
    fn a_rate_going_back_up_needs_a_new_session_too() {
        let start = Instant::now();
        let mut watch = RateWatch::new(8_000_000, start);
        let now = run(&mut watch, 8, 1_000_000, 5, start);
        watch.opened(1_000_000, now);
        let later = now + SECOND * 5;
        assert!(!watch.reopen_due(1_900_000, later));
        assert!(watch.reopen_due(2_000_000, later));
    }

    #[test]
    fn the_full_rate_is_worth_a_smaller_step() {
        let start = Instant::now();
        let mut watch = RateWatch::new(8_000_000, start);
        let now = run(&mut watch, 8, 5_600_000, 5, start);
        assert!(watch.reopen_due(5_600_000, now));
        watch.opened(5_600_000, now);
        let later = now + SECOND * 5;
        assert!(!watch.reopen_due(7_500_000, later));
        assert!(watch.reopen_due(8_000_000, later));
    }
}
