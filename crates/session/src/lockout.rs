//! Limits on guessing the unattended password: after a few wrong tries, the
//! host waits longer and longer, for each technician and for all together
//! (many keys, each with few tries, must not add up to many guesses). Kept in
//! memory: a restart of the service is rare, and takes longer than a guess.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hext_crypto::PublicKey;

/// Wrong passwords allowed before the first wait.
const TECHNICIAN_TRIES: u32 = 5;
const TOTAL_TRIES: u32 = 20;
/// The waits, in order; the last one repeats.
const WAITS: [Duration; 3] = [
    Duration::from_secs(60),
    Duration::from_secs(5 * 60),
    Duration::from_secs(15 * 60),
];
/// Failures this old no longer count: a typo last week is not a guess.
const FORGET_AFTER: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Default, Clone, Copy)]
struct Count {
    failures: u32,
    last: Option<Instant>,
    until: Option<Instant>,
}

impl Count {
    fn forget_if_old(&mut self, now: Instant) {
        if self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) >= FORGET_AFTER)
        {
            *self = Self::default();
        }
    }

    fn wait(&self, now: Instant) -> Option<Duration> {
        self.until
            .map(|until| until.saturating_duration_since(now))
            .filter(|left| !left.is_zero())
    }

    fn failed(&mut self, tries: u32, now: Instant) {
        self.forget_if_old(now);
        self.failures += 1;
        self.last = Some(now);
        if self.failures >= tries {
            let step = (self.failures - tries) as usize;
            self.until = Some(now + WAITS[step.min(WAITS.len() - 1)]);
        }
    }
}

#[derive(Debug, Default)]
struct State {
    technicians: HashMap<PublicKey, Count>,
    total: Count,
}

/// The failures so far; cheap to clone (shared).
#[derive(Debug, Clone, Default)]
pub struct Lockout(Arc<Mutex<State>>);

impl Lockout {
    /// How long `viewer` must still wait to try a password, if at all.
    pub fn check(&self, viewer: &PublicKey) -> Option<Duration> {
        self.check_at(viewer, Instant::now())
    }

    /// A wrong password from `viewer`.
    pub fn failed(&self, viewer: &PublicKey) {
        self.failed_at(viewer, Instant::now());
    }

    /// A right one: whoever this is, they were not guessing.
    pub fn succeeded(&self, viewer: &PublicKey) {
        let mut state = self.0.lock().expect("lock");
        state.technicians.remove(viewer);
        state.total = Count::default();
    }

    fn check_at(&self, viewer: &PublicKey, now: Instant) -> Option<Duration> {
        let mut state = self.0.lock().expect("lock");
        state.total.forget_if_old(now);
        let own = state.technicians.get_mut(viewer).and_then(|count| {
            count.forget_if_old(now);
            count.wait(now)
        });
        own.max(state.total.wait(now))
    }

    fn failed_at(&self, viewer: &PublicKey, now: Instant) {
        let mut state = self.0.lock().expect("lock");
        // Nor do the keys of old failures pile up here.
        state
            .technicians
            .retain(|_, count| count.last.is_some_and(|last| now - last < FORGET_AFTER));
        state
            .technicians
            .entry(*viewer)
            .or_default()
            .failed(TECHNICIAN_TRIES, now);
        state.total.failed(TOTAL_TRIES, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hext_crypto::DeviceKey;

    fn key() -> PublicKey {
        DeviceKey::generate().unwrap().public_key()
    }

    #[test]
    fn waits_grow_after_five_failures() {
        let lockout = Lockout::default();
        let viewer = key();
        let mut now = Instant::now();
        for _ in 0..TECHNICIAN_TRIES - 1 {
            assert_eq!(lockout.check_at(&viewer, now), None);
            lockout.failed_at(&viewer, now);
        }
        assert_eq!(lockout.check_at(&viewer, now), None);
        for wait in [60, 300, 900, 900] {
            lockout.failed_at(&viewer, now);
            let wait = Duration::from_secs(wait);
            assert_eq!(lockout.check_at(&viewer, now), Some(wait));
            now += wait;
            assert_eq!(lockout.check_at(&viewer, now), None, "the wait is over");
        }
    }

    #[test]
    fn each_technician_has_their_own_count() {
        let lockout = Lockout::default();
        let (guesser, technician) = (key(), key());
        let now = Instant::now();
        for _ in 0..TECHNICIAN_TRIES {
            lockout.failed_at(&guesser, now);
        }
        assert!(lockout.check_at(&guesser, now).is_some());
        assert_eq!(lockout.check_at(&technician, now), None);
    }

    #[test]
    fn many_keys_add_up_to_a_wait_for_all() {
        let lockout = Lockout::default();
        let now = Instant::now();
        for _ in 0..TOTAL_TRIES {
            lockout.failed_at(&key(), now);
        }
        assert_eq!(
            lockout.check_at(&key(), now),
            Some(WAITS[0]),
            "anyone waits"
        );
    }

    #[test]
    fn the_right_password_clears_the_count() {
        let lockout = Lockout::default();
        let viewer = key();
        let now = Instant::now();
        for _ in 0..TECHNICIAN_TRIES - 1 {
            lockout.failed_at(&viewer, now);
        }
        lockout.succeeded(&viewer);
        lockout.failed_at(&viewer, now);
        assert_eq!(lockout.check_at(&viewer, now), None);
    }

    #[test]
    fn old_failures_are_forgotten() {
        let lockout = Lockout::default();
        let viewer = key();
        let now = Instant::now();
        for _ in 0..TECHNICIAN_TRIES - 1 {
            lockout.failed_at(&viewer, now);
        }
        let later = now + FORGET_AFTER;
        lockout.failed_at(&viewer, later);
        assert_eq!(lockout.check_at(&viewer, later), None);
    }
}
