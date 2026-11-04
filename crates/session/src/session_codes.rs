//! One-time codes: when the host's user accepts a viewer (through the
//! server), the viewer gets a code that opens one session, instead of the
//! access code.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hext_crypto::PublicKey;
use hext_proto::Permissions;
use ring::rand::{SecureRandom, SystemRandom};

use crate::codes_match;

/// How long the viewer has to connect after being accepted.
pub const SESSION_CODE_LIFETIME: Duration = Duration::from_secs(120);

/// What the host's user accepted: who, and what they may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub permissions: Permissions,
    /// The name and ID the user saw when asked.
    pub name: String,
    pub id: String,
    /// Nobody accepted: the viewer must also know the unattended password.
    pub needs_password: bool,
}

/// A code, the viewer it was issued to, when, and what it grants.
type Issued = (String, PublicKey, Instant, Grant);

/// Codes waiting to be used; cheap to clone (shared).
#[derive(Debug, Clone, Default)]
pub struct SessionCodes(Arc<Mutex<Vec<Issued>>>);

impl SessionCodes {
    /// A new code (20 random digits) for the viewer with key `viewer`,
    /// valid once within [`SESSION_CODE_LIFETIME`], granting `grant`.
    pub fn issue(&self, viewer: PublicKey, grant: Grant) -> String {
        let mut bytes = [0u8; 16];
        SystemRandom::new()
            .fill(&mut bytes)
            .expect("system random numbers");
        let code = format!("{:020}", u128::from_le_bytes(bytes) % 10u128.pow(20));
        let mut codes = self.0.lock().expect("lock");
        codes.retain(|(_, _, issued, _)| issued.elapsed() < SESSION_CODE_LIFETIME);
        codes.push((code.clone(), viewer, Instant::now(), grant));
        code
    }

    /// What `given` grants, if it is a live code issued to `viewer`; it is
    /// then used up.
    pub fn redeem(&self, given: &str, viewer: &PublicKey) -> Option<Grant> {
        self.redeem_at(given, viewer, Instant::now())
    }

    fn redeem_at(&self, given: &str, viewer: &PublicKey, now: Instant) -> Option<Grant> {
        let mut codes = self.0.lock().expect("lock");
        codes.retain(|(_, _, issued, _)| {
            now.saturating_duration_since(*issued) < SESSION_CODE_LIFETIME
        });
        // Compare against every code (constant time each), then remove.
        let found = codes
            .iter()
            .enumerate()
            .fold(None, |found, (i, (code, owner, _, _))| {
                let matched = codes_match(code, given) & (owner == viewer);
                found.or(matched.then_some(i))
            });
        found.map(|i| codes.remove(i).3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hext_crypto::DeviceKey;

    fn key() -> PublicKey {
        DeviceKey::generate().unwrap().public_key()
    }

    fn grant() -> Grant {
        Grant {
            permissions: Permissions::CONTROL,
            name: "Técnico".into(),
            id: "482 913 557".into(),
            needs_password: false,
        }
    }

    #[test]
    fn a_code_opens_one_session() {
        let codes = SessionCodes::default();
        let viewer = key();
        let code = codes.issue(viewer, grant());
        assert_eq!(code.len(), 20);
        assert_eq!(codes.redeem("12345", &viewer), None);
        assert_eq!(codes.redeem("", &viewer), None);
        assert_eq!(codes.redeem(&code, &viewer), Some(grant()));
        assert_eq!(codes.redeem(&code, &viewer), None, "used up");
    }

    #[test]
    fn only_the_viewer_it_was_issued_to() {
        let codes = SessionCodes::default();
        let viewer = key();
        let code = codes.issue(viewer, grant());
        assert_eq!(
            codes.redeem(&code, &key()),
            None,
            "someone else with the code"
        );
        assert!(codes.redeem(&code, &viewer).is_some());
    }

    #[test]
    fn codes_are_independent() {
        let codes = SessionCodes::default();
        let viewer = key();
        let view_only = Grant {
            permissions: Permissions::VIEW_ONLY,
            ..grant()
        };
        let (a, b) = (
            codes.issue(viewer, grant()),
            codes.issue(viewer, view_only.clone()),
        );
        assert_ne!(a, b);
        assert_eq!(codes.redeem(&b, &viewer), Some(view_only));
        assert_eq!(codes.redeem(&a, &viewer), Some(grant()));
    }

    #[test]
    fn old_codes_expire() {
        let codes = SessionCodes::default();
        let viewer = key();
        let code = codes.issue(viewer, grant());
        let late = Instant::now() + SESSION_CODE_LIFETIME;
        assert_eq!(codes.redeem_at(&code, &viewer, late), None);
    }
}
