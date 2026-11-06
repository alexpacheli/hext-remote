//! How fast the other side may write. Each message makes work on this
//! computer (a window to update, at least): a person does not notice the
//! limit, and a flood stops here.

use std::time::{Duration, Instant};

/// Messages taken each second.
const PER_SECOND: u32 = 10;

#[derive(Debug)]
pub(crate) struct ChatPace {
    started: Instant,
    taken: u32,
}

impl ChatPace {
    pub(crate) fn new() -> Self {
        Self {
            started: Instant::now(),
            taken: 0,
        }
    }

    /// Whether a message arriving at `now` is taken.
    pub(crate) fn allows(&mut self, now: Instant) -> bool {
        if now.duration_since(self.started) >= Duration::from_secs(1) {
            self.started = now;
            self.taken = 0;
        }
        self.taken += 1;
        self.taken <= PER_SECOND
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flood_stops_and_the_next_second_starts_over() {
        let mut pace = ChatPace::new();
        let now = Instant::now();
        let taken = (0..1000).filter(|_| pace.allows(now)).count();
        assert_eq!(taken, PER_SECOND as usize);
        assert!(pace.allows(now + Duration::from_secs(2)));
    }
}
