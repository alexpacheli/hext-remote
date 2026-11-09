//! Limits on guessing the unattended password: after a few wrong tries, the
//! host waits longer and longer, for each technician and for all together
//! (many keys, each with few tries, must not add up to many guesses). The
//! service also keeps them on disk ([`Lockout::kept_in`]): otherwise
//! restarting it (a crash it can be made to have, say) would clear them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hext_crypto::PublicKey;
use tracing::warn;

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
    /// Where they are kept; `None`: in memory only.
    file: Option<PathBuf>,
}

/// Seconds since 1970 (nothing else counts across a restart).
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Count {
    /// As a line of the file: failures, the last one and the end of the
    /// wait, in seconds since 1970 (0: none).
    fn to_line(self, who: &str, now: Instant, unix: u64) -> String {
        let last = self.last.map_or(0, |last| {
            unix.saturating_sub(now.saturating_duration_since(last).as_secs())
        });
        let until = self
            .wait(now)
            .map_or(0, |left| unix + left.as_secs().max(1));
        format!("{who} {} {last} {until}\n", self.failures)
    }

    /// From a line's numbers; `None`: forgotten already.
    fn from_numbers(failures: u32, last: u64, until: u64, now: Instant, unix: u64) -> Option<Self> {
        // (A clock set back counts as now: it must not stretch anything.)
        let age = Duration::from_secs(unix.saturating_sub(last));
        if failures == 0 || age >= FORGET_AFTER {
            return None;
        }
        let longest = WAITS[WAITS.len() - 1];
        let left = Duration::from_secs(until.saturating_sub(unix)).min(longest);
        Some(Self {
            failures,
            // (So soon after boot that it would be before it: now.)
            last: Some(now.checked_sub(age).unwrap_or(now)),
            until: (!left.is_zero()).then(|| now + left),
        })
    }
}

impl State {
    /// Reads `file`; one that is not there or does not read starts from none.
    fn load(file: PathBuf) -> Self {
        let mut state = Self::default();
        let (now, unix) = (Instant::now(), unix_now());
        if let Ok(text) = std::fs::read_to_string(&file) {
            for line in text.lines().take(10_000) {
                let mut parts = line.split_whitespace();
                let (Some(who), Some(failures), Some(last), Some(until)) =
                    (parts.next(), parts.next(), parts.next(), parts.next())
                else {
                    continue;
                };
                let (Ok(failures), Ok(last), Ok(until)) =
                    (failures.parse(), last.parse(), until.parse())
                else {
                    continue;
                };
                let Some(count) = Count::from_numbers(failures, last, until, now, unix) else {
                    continue;
                };
                if who == "total" {
                    state.total = count;
                } else if let Ok(key) = who.parse::<PublicKey>() {
                    state.technicians.insert(key, count);
                }
            }
        }
        state.file = Some(file);
        state
    }

    /// Writes it down, whole or not at all.
    fn save(&self) {
        let Some(file) = &self.file else { return };
        let (now, unix) = (Instant::now(), unix_now());
        let mut text = self.total.to_line("total", now, unix);
        for (key, count) in &self.technicians {
            text.push_str(&count.to_line(&key.to_string(), now, unix));
        }
        let aside = file.with_extension("new");
        if let Err(e) = std::fs::write(&aside, text).and_then(|()| std::fs::rename(&aside, file)) {
            warn!("{}: {e}", file.display());
        }
    }
}

/// The failures so far; cheap to clone (shared).
#[derive(Debug, Clone, Default)]
pub struct Lockout(Arc<Mutex<State>>);

impl Lockout {
    /// Failures kept in `file` (read now, written on each change), so a
    /// restart does not clear them. Only the system should write there.
    pub fn kept_in(file: PathBuf) -> Self {
        Self(Arc::new(Mutex::new(State::load(file))))
    }

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
        let had = state.technicians.remove(viewer).is_some() || state.total.failures > 0;
        state.total = Count::default();
        if had {
            state.save();
        }
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
        state.save();
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
    fn a_restart_does_not_clear_the_waits() {
        let file = std::env::temp_dir().join(format!("hext-lockout-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let (guesser, other) = (key(), key());
        let before = Lockout::kept_in(file.clone());
        for _ in 0..TECHNICIAN_TRIES {
            before.failed(&guesser);
        }
        for _ in 0..TOTAL_TRIES - TECHNICIAN_TRIES - 1 {
            before.failed(&key());
        }
        assert!(before.check(&guesser).is_some());
        assert_eq!(before.check(&other), None);

        let after = Lockout::kept_in(file.clone());
        let wait = after.check(&guesser);
        assert!(
            wait.is_some_and(|wait| wait <= WAITS[0]),
            "still waiting: {wait:?}"
        );
        assert_eq!(after.check(&other), None, "not yet for all");
        after.failed(&other);
        assert!(after.check(&other).is_some(), "the total came back too");

        // A garbled file starts from none.
        std::fs::write(&file, "total x y\nlixo\n").unwrap();
        assert_eq!(Lockout::kept_in(file.clone()).check(&guesser), None);
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn a_kept_wait_is_never_longer_than_the_longest() {
        let (now, unix) = (Instant::now(), unix_now());
        let count = Count::from_numbers(9, unix, unix + 1_000_000, now, unix).unwrap();
        assert_eq!(count.wait(now), Some(WAITS[WAITS.len() - 1]));
        assert!(Count::from_numbers(9, unix - FORGET_AFTER.as_secs(), 0, now, unix).is_none());
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
