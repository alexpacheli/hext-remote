//! How much video the network takes right now.
//!
//! The encoder used to send its full rate whatever the path could carry, and
//! the host stopped for as long as congestion control held it back. A lower
//! rate costs a little quality, which is what a bad network should cost,
//! instead of a frozen image.

use std::time::Duration;

/// Below this the image is not worth much, and a keyframe alone fills it.
const MIN_BITRATE: u32 = 1_000_000;

/// Reports ignored at the start: every connection opens with a small window,
/// and a first keyframe held back by it says nothing about the path.
const WARMUP: u32 = 2;

/// This much of a second waiting for congestion control is too much.
const HELD: Duration = Duration::from_millis(150);
/// Round trip above the path's best: packets are queueing somewhere.
const RTT_RISE: Duration = Duration::from_millis(100);
/// Packets lost in a second, in percent. Wi-Fi loses a few without any
/// harm (3.8% in a second that kept 52 frames in the field), so only this
/// much counts on its own.
const LOSS_PERCENT: u64 = 10;

/// What a bad second takes off, as a fraction.
const DOWN: (u32, u32) = (7, 10);
/// What a good second adds back, as a fraction.
const UP: (u32, u32) = (11, 10);
/// Good seconds in a row before going up: going down again is what hurts.
const QUIET: u32 = 5;

/// One second of sending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Second {
    /// Time held back by congestion control.
    pub held: Duration,
    /// Packets lost and sent on the path in use.
    pub lost: u64,
    pub sent: u64,
    pub rtt: Duration,
    /// Video actually sent.
    pub bitrate_bps: u32,
    /// The path changed: its round trip says nothing about the last one.
    pub new_path: bool,
}

/// A new target rate, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Change {
    pub bitrate_bps: u32,
    pub why: &'static str,
}

/// Picks the video's target rate, one second at a time.
#[derive(Debug, Clone)]
pub(crate) struct RateControl {
    max: u32,
    min: u32,
    bitrate_bps: u32,
    warmup: u32,
    /// Good seconds in a row.
    quiet: u32,
    /// The best round trip on this path.
    base_rtt: Option<Duration>,
}

impl RateControl {
    pub fn new(max: u32) -> Self {
        Self {
            max,
            min: MIN_BITRATE.min(max),
            bitrate_bps: max,
            warmup: WARMUP,
            quiet: 0,
            base_rtt: None,
        }
    }

    pub fn bitrate_bps(&self) -> u32 {
        self.bitrate_bps
    }

    /// Takes in a second of sending; returns the new target if it moved.
    pub fn second(&mut self, second: &Second) -> Option<Change> {
        if second.new_path {
            self.base_rtt = None;
        }
        let base = self.best_rtt(second.rtt);
        if self.warmup > 0 {
            self.warmup -= 1;
            return None;
        }
        match trouble(second, base) {
            Some(why) => {
                self.quiet = 0;
                // Held back, what went out is what the path carried.
                let carried = (second.held >= HELD).then_some(second.bitrate_bps);
                let target = scaled(self.bitrate_bps, DOWN).min(carried.unwrap_or(u32::MAX));
                self.go_to(target, why)
            }
            None => {
                self.quiet += 1;
                // A still screen proves nothing about a higher rate.
                let using = second.bitrate_bps >= self.bitrate_bps / 2;
                if self.quiet < QUIET || !using {
                    return None;
                }
                self.go_to(scaled(self.bitrate_bps, UP), "rede boa")
            }
        }
    }

    fn best_rtt(&mut self, rtt: Duration) -> Option<Duration> {
        if !rtt.is_zero() {
            self.base_rtt = Some(self.base_rtt.map_or(rtt, |base| base.min(rtt)));
        }
        self.base_rtt
    }

    fn go_to(&mut self, target: u32, why: &'static str) -> Option<Change> {
        let target = target.clamp(self.min, self.max);
        if target == self.bitrate_bps {
            return None;
        }
        self.bitrate_bps = target;
        Some(Change {
            bitrate_bps: target,
            why,
        })
    }
}

/// Why this second was bad, if it was.
fn trouble(second: &Second, base_rtt: Option<Duration>) -> Option<&'static str> {
    if second.held >= HELD {
        return Some("preso no controle de congestionamento");
    }
    if base_rtt.is_some_and(|base| second.rtt > base + RTT_RISE) {
        return Some("RTT subiu");
    }
    if second.sent > 0 && second.lost * 100 >= second.sent * LOSS_PERCENT {
        return Some("perda alta");
    }
    None
}

fn scaled(bitrate_bps: u32, (over, under): (u32, u32)) -> u32 {
    let scaled = u64::from(bitrate_bps) * u64::from(over) / u64::from(under);
    u32::try_from(scaled).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: u32 = 8_000_000;

    fn good() -> Second {
        Second {
            held: Duration::ZERO,
            lost: 0,
            sent: 1000,
            rtt: Duration::from_millis(15),
            bitrate_bps: MAX,
            new_path: false,
        }
    }

    fn held(bitrate_bps: u32) -> Second {
        Second {
            held: Duration::from_millis(500),
            bitrate_bps,
            ..good()
        }
    }

    /// Past the warmup, with the best round trip known.
    fn warm() -> RateControl {
        let mut rate = RateControl::new(MAX);
        for _ in 0..WARMUP {
            assert_eq!(rate.second(&good()), None);
        }
        rate
    }

    #[test]
    fn the_first_seconds_of_a_connection_do_not_count() {
        let mut rate = RateControl::new(MAX);
        for _ in 0..WARMUP {
            assert_eq!(rate.second(&held(1_000_000)), None);
        }
        assert_eq!(rate.bitrate_bps(), MAX);
    }

    #[test]
    fn held_back_goes_down_to_what_the_path_carried() {
        let mut rate = warm();
        let change = rate.second(&held(2_000_000)).unwrap();
        assert_eq!(change.bitrate_bps, 2_000_000);
    }

    #[test]
    fn a_keyframe_held_back_at_a_full_rate_still_takes_a_step_down() {
        let mut rate = warm();
        let change = rate.second(&held(MAX * 2)).unwrap();
        assert_eq!(change.bitrate_bps, 5_600_000);
    }

    #[test]
    fn a_rising_round_trip_goes_down() {
        let mut rate = warm();
        let second = Second {
            rtt: Duration::from_millis(270),
            ..good()
        };
        assert_eq!(rate.second(&second).unwrap().bitrate_bps, 5_600_000);
    }

    #[test]
    fn some_loss_is_not_enough() {
        let mut rate = warm();
        let second = Second {
            lost: 46,
            sent: 1196,
            ..good()
        };
        assert_eq!(rate.second(&second), None);
    }

    #[test]
    fn heavy_loss_goes_down() {
        let mut rate = warm();
        let second = Second {
            lost: 417,
            sent: 1083,
            ..good()
        };
        assert_eq!(rate.second(&second).unwrap().bitrate_bps, 5_600_000);
    }

    #[test]
    fn the_rate_stops_at_the_floor() {
        let mut rate = warm();
        assert_eq!(
            rate.second(&held(170_000)).unwrap().bitrate_bps,
            MIN_BITRATE
        );
        assert_eq!(rate.second(&held(170_000)), None);
    }

    #[test]
    fn it_comes_back_after_quiet_seconds_and_stops_at_the_top() {
        let mut rate = warm();
        rate.second(&held(MAX / 2));
        for _ in 1..QUIET {
            assert_eq!(rate.second(&good()), None);
        }
        assert_eq!(rate.second(&good()).unwrap().bitrate_bps, 4_400_000);
        while rate.bitrate_bps() < MAX {
            rate.second(&good()).unwrap();
        }
        assert_eq!(rate.second(&good()), None);
    }

    #[test]
    fn a_still_screen_proves_nothing() {
        let mut rate = warm();
        rate.second(&held(2_000_000));
        let still = Second {
            bitrate_bps: 100_000,
            ..good()
        };
        for _ in 0..QUIET * 2 {
            assert_eq!(rate.second(&still), None);
        }
        assert_eq!(rate.bitrate_bps(), 2_000_000);
    }

    #[test]
    fn a_new_path_has_its_own_round_trip() {
        let mut rate = warm();
        let slower = Second {
            rtt: Duration::from_millis(200),
            new_path: true,
            ..good()
        };
        assert_eq!(rate.second(&slower), None);
        let same = Second {
            rtt: Duration::from_millis(200),
            ..good()
        };
        assert_eq!(rate.second(&same), None);
    }

    #[test]
    fn a_rate_under_the_floor_is_kept() {
        let mut rate = RateControl::new(500_000);
        for _ in 0..WARMUP {
            rate.second(&good());
        }
        assert_eq!(rate.second(&held(100_000)), None);
        assert_eq!(rate.bitrate_bps(), 500_000);
    }
}
