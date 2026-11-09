//! Rate limits: at most `max` events per `window` for each key; the key
//! of an address for them; and caps on what is open at once.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The address as the per-address limits count it: an IPv4 address as it
/// is (also written as IPv6), an IPv6 one by its /64, which one subscriber
/// usually has whole.
pub fn ip_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let prefix = u128::from(v6) & !((1u128 << 64) - 1);
                IpAddr::V6(Ipv6Addr::from(prefix))
            }
        },
    }
}

/// How many of each key are open at once, up to `max`.
pub struct Open<K> {
    max: usize,
    open: Mutex<HashMap<K, usize>>,
}

impl<K: Eq + Hash + Clone> Open<K> {
    pub fn new(max: usize) -> Self {
        Self {
            max,
            open: Mutex::default(),
        }
    }

    /// One more for `key`, if under the cap; it closes when the guard drops.
    pub fn open(&self, key: K) -> Option<OpenGuard<'_, K>> {
        let mut open = self.open.lock().expect("lock");
        let count = open.entry(key.clone()).or_default();
        if *count >= self.max {
            return None;
        }
        *count += 1;
        Some(OpenGuard { open: self, key })
    }
}

/// One open of [`Open`].
pub struct OpenGuard<'a, K: Eq + Hash + Clone> {
    open: &'a Open<K>,
    key: K,
}

impl<K: Eq + Hash + Clone> Drop for OpenGuard<'_, K> {
    fn drop(&mut self) {
        let mut open = self.open.open.lock().expect("lock");
        if let Some(count) = open.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                open.remove(&self.key);
            }
        }
    }
}

pub struct RateLimiter<K> {
    max: usize,
    window: Duration,
    hits: Mutex<HashMap<K, VecDeque<Instant>>>,
}

impl<K: Eq + Hash + Clone> RateLimiter<K> {
    pub fn new(max: usize, window: Duration) -> Self {
        Self {
            max,
            window,
            hits: Mutex::default(),
        }
    }

    /// Records an event for `key` if it is within the limit; false if not.
    pub fn allow(&self, key: &K, now: Instant) -> bool {
        let mut hits = self.hits.lock().expect("lock");
        let times = hits.entry(key.clone()).or_default();
        self.forget_old(times, now);
        if times.len() >= self.max {
            return false;
        }
        times.push_back(now);
        true
    }

    /// [`Self::allow`], and also no sooner after the last event than `wait`
    /// says for the number of events in the window (a growing wait).
    pub fn allow_spaced(&self, key: &K, now: Instant, wait: impl Fn(usize) -> Duration) -> bool {
        let mut hits = self.hits.lock().expect("lock");
        let times = hits.entry(key.clone()).or_default();
        self.forget_old(times, now);
        if times.len() >= self.max {
            return false;
        }
        if let Some(&last) = times.back()
            && now.saturating_duration_since(last) < wait(times.len())
        {
            return false;
        }
        times.push_back(now);
        true
    }

    /// Takes back the latest event of `key` (it turned out not to count).
    pub fn refund(&self, key: &K) {
        let mut hits = self.hits.lock().expect("lock");
        if let Some(times) = hits.get_mut(key) {
            times.pop_back();
        }
    }

    /// Drops keys with no recent events (call now and then: memory).
    pub fn prune(&self, now: Instant) {
        let mut hits = self.hits.lock().expect("lock");
        hits.retain(|_, times| {
            self.forget_old(times, now);
            !times.is_empty()
        });
    }

    fn forget_old(&self, times: &mut VecDeque<Instant>, now: Instant) {
        while times
            .front()
            .is_some_and(|&t| now.saturating_duration_since(t) >= self.window)
        {
            times.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_each_key_within_the_window() {
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        let t = Instant::now();
        assert!(limiter.allow(&"a", t));
        assert!(limiter.allow(&"a", t));
        assert!(!limiter.allow(&"a", t));
        assert!(limiter.allow(&"b", t), "keys are separate");
        // Refused attempts do not count.
        assert!(limiter.allow(&"a", t + Duration::from_secs(60)));
    }

    #[test]
    fn spaced_events_wait_longer_each_time_and_refunds_give_back() {
        let limiter = RateLimiter::new(5, Duration::from_secs(3600));
        let wait = |n: usize| Duration::from_secs(if n < 2 { 0 } else { 10 });
        let t = Instant::now();
        assert!(limiter.allow_spaced(&"a", t, wait));
        assert!(limiter.allow_spaced(&"a", t, wait));
        assert!(!limiter.allow_spaced(&"a", t, wait), "now it waits");
        assert!(limiter.allow_spaced(&"a", t + Duration::from_secs(10), wait));
        limiter.refund(&"a");
        limiter.refund(&"a");
        assert!(limiter.allow_spaced(&"a", t + Duration::from_secs(10), wait));
    }

    #[test]
    fn addresses_count_by_their_subscriber() {
        let key = |s: &str| ip_key(s.parse().unwrap()).to_string();
        assert_eq!(key("203.0.113.7"), "203.0.113.7");
        assert_eq!(key("::ffff:203.0.113.7"), "203.0.113.7");
        assert_eq!(key("2001:db8:1:2:aaaa:bbbb:cccc:dddd"), "2001:db8:1:2::");
        assert_eq!(key("2001:db8:1:2::1"), key("2001:db8:1:2:ffff::9"));
        assert_ne!(key("2001:db8:1:2::1"), key("2001:db8:1:3::1"));
    }

    #[test]
    fn open_counts_until_the_guard_drops() {
        let open = Open::new(2);
        let a = open.open("a").unwrap();
        let _b = open.open("a").unwrap();
        assert!(open.open("a").is_none());
        assert!(open.open("b").is_some());
        drop(a);
        assert!(open.open("a").is_some());
    }

    #[test]
    fn prune_forgets_idle_keys() {
        let limiter = RateLimiter::new(1, Duration::from_secs(1));
        let t = Instant::now();
        limiter.allow(&1, t);
        limiter.prune(t + Duration::from_secs(2));
        assert!(limiter.hits.lock().unwrap().is_empty());
    }
}
