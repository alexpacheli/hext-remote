//! Latency statistics shown in the stats overlay.
//!
//! Every pipeline stage (capture, encode, network, decode, render) and the
//! round-trip time feed a [`Rolling`] window; the overlay renders its
//! [`Summary`].

use std::collections::VecDeque;
use std::time::Duration;

/// Stages of the video pipeline, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Capture,
    Encode,
    Network,
    Decode,
    Render,
}

impl Stage {
    pub const ALL: [Stage; 5] = [
        Stage::Capture,
        Stage::Encode,
        Stage::Network,
        Stage::Decode,
        Stage::Render,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Stage::Capture => "captura",
            Stage::Encode => "encode",
            Stage::Network => "rede",
            Stage::Decode => "decode",
            Stage::Render => "render",
        }
    }
}

/// Fixed-size window over the most recent samples.
#[derive(Debug, Clone)]
pub struct Rolling {
    samples: VecDeque<Duration>,
    capacity: usize,
}

impl Rolling {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "capacity must be positive");
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, sample: Duration) {
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// `None` until at least one sample was pushed.
    pub fn summary(&self) -> Option<Summary> {
        if self.samples.is_empty() {
            return None;
        }
        let mut sorted: Vec<Duration> = self.samples.iter().copied().collect();
        sorted.sort_unstable();
        let total: Duration = sorted.iter().sum();
        Some(Summary {
            count: sorted.len(),
            min: sorted[0],
            avg: total / sorted.len() as u32,
            p95: percentile(&sorted, 95),
            max: sorted[sorted.len() - 1],
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub count: usize,
    pub min: Duration,
    pub avg: Duration,
    pub p95: Duration,
    pub max: Duration,
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "min {:.2} ms | média {:.2} ms | p95 {:.2} ms | máx {:.2} ms ({} amostras)",
            ms(self.min),
            ms(self.avg),
            ms(self.p95),
            ms(self.max),
            self.count
        )
    }
}

/// Nearest-rank percentile over an ascending, non-empty slice.
fn percentile(sorted: &[Duration], pct: usize) -> Duration {
    let rank = (pct * sorted.len()).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Gaps between frames while the picture moves: the stutter an average frame
/// rate hides (57 frames a second can still be one 33 ms gap every quarter
/// of a second). A gap of [`Gaps::STILL`] or more is a still screen, not a
/// stutter, and does not count.
#[derive(Debug, Default)]
pub struct Gaps {
    last: Option<std::time::Instant>,
    longest: Duration,
    late: u32,
}

impl Gaps {
    /// Longer than this, nothing was moving.
    pub const STILL: Duration = Duration::from_millis(250);
    /// Longer than this, a frame of a 60 Hz screen was missed.
    pub const LATE: Duration = Duration::from_millis(25);

    /// A frame at `now`.
    pub fn frame(&mut self, now: std::time::Instant) {
        if let Some(last) = self.last.replace(now) {
            let gap = now.saturating_duration_since(last);
            if gap < Self::STILL {
                self.longest = self.longest.max(gap);
                self.late += u32::from(gap > Self::LATE);
            }
        }
    }

    /// The longest gap and how many were over [`Gaps::LATE`] since the last
    /// call; counting starts over (from the last frame).
    pub fn take(&mut self) -> (Duration, u32) {
        let taken = (self.longest, self.late);
        self.longest = Duration::ZERO;
        self.late = 0;
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn millis(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    #[test]
    fn only_gaps_while_moving_count() {
        let mut gaps = Gaps::default();
        let t0 = std::time::Instant::now();
        for at in [0, 16, 33, 66, 83, 2000, 2016] {
            gaps.frame(t0 + millis(at));
        }
        // 33 → 66 is a missed frame; 83 → 2000 is a still screen.
        assert_eq!(gaps.take(), (millis(33), 1));
        assert_eq!(gaps.take(), (Duration::ZERO, 0));
        gaps.frame(t0 + millis(2050));
        assert_eq!(gaps.take(), (millis(34), 1));
    }

    #[test]
    fn empty_has_no_summary() {
        assert_eq!(Rolling::new(4).summary(), None);
    }

    #[test]
    fn summary_of_samples() {
        let mut r = Rolling::new(100);
        for v in 1..=100 {
            r.push(millis(v));
        }
        let s = r.summary().unwrap();
        assert_eq!(s.count, 100);
        assert_eq!(s.min, millis(1));
        assert_eq!(s.max, millis(100));
        assert_eq!(s.p95, millis(95));
        assert_eq!(s.avg, Duration::from_micros(50_500));
    }

    #[test]
    fn window_drops_oldest() {
        let mut r = Rolling::new(3);
        for v in [100, 1, 2, 3] {
            r.push(millis(v));
        }
        assert_eq!(r.len(), 3);
        assert_eq!(r.summary().unwrap().max, millis(3));
    }

    #[test]
    fn single_sample() {
        let mut r = Rolling::new(8);
        r.push(millis(7));
        let s = r.summary().unwrap();
        assert_eq!(
            (s.min, s.avg, s.p95, s.max),
            (millis(7), millis(7), millis(7), millis(7))
        );
    }
}
