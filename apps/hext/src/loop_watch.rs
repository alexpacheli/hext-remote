//! How long the window's thread spends in each call of its event loop.
//!
//! The windows of the viewer live on one thread: while it is held in a call
//! (waiting on the GPU, a lock, a pipe...), none of them moves, resizes or
//! repaints. Each call over [`STALL`] is logged with what held it; with
//! `HEXT_LOOP_TRACE=1`, a summary every second says where the time went.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use tracing::{info, warn};

/// A call this long is felt: the window stops following the mouse.
pub const STALL: Duration = Duration::from_millis(100);
/// How often the summary comes, with `HEXT_LOOP_TRACE`.
const SUMMARY_EVERY: Duration = Duration::from_secs(1);

thread_local! {
    static WATCH: RefCell<Watch> = RefCell::new(Watch::new(
        std::env::var_os("HEXT_LOOP_TRACE").is_some_and(|v| v != "0"),
    ));
}

/// Runs `f`, counting its time against `what`.
pub fn timed<R>(what: &'static str, f: impl FnOnce() -> R) -> R {
    let start = Instant::now();
    let result = f();
    let took = start.elapsed();
    if took >= STALL {
        warn!("janela presa por {} ms em {what}", took.as_millis());
    }
    count(what, took);
    result
}

/// Counts a wait for the window's thread measured elsewhere (a message in
/// its queue): what the mouse waited, at least.
pub fn note(what: &'static str, took: Duration) {
    if took >= STALL {
        warn!("{what}: esperou {} ms pela janela", took.as_millis());
    }
    count(what, took);
}

fn count(what: &'static str, took: Duration) {
    let summary = WATCH.with(|watch| watch.borrow_mut().record(what, took, Instant::now()));
    if let Some(summary) = summary {
        info!("loop da janela: {summary}");
    }
}

/// The longest call of each kind since the last summary.
struct Watch {
    trace: bool,
    since: Instant,
    /// What, how many times, the longest, all together.
    calls: Vec<(&'static str, u32, Duration, Duration)>,
}

impl Watch {
    fn new(trace: bool) -> Self {
        Self {
            trace,
            since: Instant::now(),
            calls: Vec::new(),
        }
    }

    /// Counts a call; the summary, when one is due.
    fn record(&mut self, what: &'static str, took: Duration, now: Instant) -> Option<String> {
        if !self.trace {
            return None;
        }
        match self.calls.iter_mut().find(|(w, ..)| *w == what) {
            Some((_, count, longest, total)) => {
                *count += 1;
                *longest = (*longest).max(took);
                *total += took;
            }
            None => self.calls.push((what, 1, took, took)),
        }
        if now.duration_since(self.since) < SUMMARY_EVERY {
            return None;
        }
        self.since = now;
        let mut calls = std::mem::take(&mut self.calls);
        calls.sort_by_key(|c| std::cmp::Reverse(c.2));
        Some(
            calls
                .iter()
                .map(|(what, count, longest, total)| {
                    format!(
                        "{what} {count}x máx {:.1} ms soma {:.1} ms",
                        longest.as_secs_f64() * 1000.0,
                        total.as_secs_f64() * 1000.0
                    )
                })
                .collect::<Vec<_>>()
                .join("; "),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_trace_nothing_is_kept() {
        let mut watch = Watch::new(false);
        let later = Instant::now() + Duration::from_secs(5);
        assert_eq!(watch.record("x", Duration::from_secs(1), later), None);
        assert!(watch.calls.is_empty());
    }

    #[test]
    fn the_summary_puts_the_longest_first() {
        let mut watch = Watch::new(true);
        let t0 = watch.since;
        let ms = Duration::from_millis;
        assert_eq!(watch.record("quadro", ms(2), t0), None);
        assert_eq!(watch.record("título", ms(30), t0), None);
        let summary = watch.record("quadro", ms(4), t0 + SUMMARY_EVERY).unwrap();
        assert_eq!(
            summary,
            "título 1x máx 30.0 ms soma 30.0 ms; quadro 2x máx 4.0 ms soma 6.0 ms"
        );
        // It starts over.
        assert!(watch.calls.is_empty());
    }
}
