//! Tokens that bring a viewer back without anyone accepting again: after a
//! network loss, and after a restart it asked for.
//!
//! Each session through the server gets a token, known only to that viewer
//! and good for one return. While the session runs, the token waits; a session
//! that ends properly takes it along. Only a hash is kept here.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hext_crypto::PublicKey;
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use tokio::sync::Notify;

use crate::Grant;

/// How long a viewer has to come back after losing the connection.
pub const NETWORK_GRACE: Duration = Duration::from_secs(120);
/// How long a viewer has to come back after a restart.
pub const RESTART_LIFETIME: Duration = Duration::from_secs(600);

pub type ResumeToken = [u8; 32];
pub type TokenHash = [u8; 32];

/// A token's way past a restart: nothing here lets anyone in by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptResume {
    pub hash: TokenHash,
    pub viewer: PublicKey,
    pub grant: Grant,
}

#[derive(Debug)]
struct Entry {
    kept: KeptResume,
    /// `None`: the session is still on.
    expires: Option<Instant>,
    /// Ends the session on, when its viewer is back on a new connection.
    displaced: Arc<Notify>,
}

/// The tokens that may still be used; cheap to clone (shared).
#[derive(Debug, Clone, Default)]
pub struct ResumeTokens(Arc<Mutex<Vec<Entry>>>);

fn hash_of(token: &ResumeToken) -> TokenHash {
    digest(&SHA256, token)
        .as_ref()
        .try_into()
        .expect("SHA-256 has 32 bytes")
}

impl ResumeTokens {
    /// A token for the session of `viewer` starting now. The returned
    /// [`Notify`] tells that session its viewer came back on a new connection.
    pub fn open(&self, viewer: PublicKey, grant: Grant) -> (ResumeToken, TokenHash, Arc<Notify>) {
        let mut token = [0u8; 32];
        SystemRandom::new()
            .fill(&mut token)
            .expect("system random numbers");
        let hash = hash_of(&token);
        let displaced = Arc::new(Notify::new());
        let mut entries = self.0.lock().expect("lock");
        purge(&mut entries, Instant::now());
        entries.push(Entry {
            kept: KeptResume {
                hash,
                viewer,
                grant,
            },
            expires: None,
            displaced: displaced.clone(),
        });
        (token, hash, displaced)
    }

    /// What `token` grants, if it is live and `viewer`'s; it is then used up.
    pub fn take(&self, token: &ResumeToken, viewer: &PublicKey) -> Option<Grant> {
        self.take_at(token, viewer, Instant::now())
    }

    fn take_at(&self, token: &ResumeToken, viewer: &PublicKey, now: Instant) -> Option<Grant> {
        let hash = hash_of(token);
        let mut entries = self.0.lock().expect("lock");
        purge(&mut entries, now);
        // A viewer's guess reveals nothing: it compares hashes.
        let found = entries
            .iter()
            .position(|e| e.kept.hash == hash && e.kept.viewer == *viewer)?;
        Some(entries.remove(found).kept.grant)
    }

    /// Whether `viewer` may be coming back. A session of its that the host
    /// still believes on is told to end: the connection is gone, or the
    /// viewer would not be asking.
    pub fn displace(&self, viewer: &PublicKey) -> bool {
        let mut entries = self.0.lock().expect("lock");
        purge(&mut entries, Instant::now());
        let mut any = false;
        for entry in entries.iter().filter(|e| e.kept.viewer == *viewer) {
            any = true;
            if entry.expires.is_none() {
                entry.displaced.notify_one();
            }
        }
        any
    }

    /// The session lost its connection: the viewer has [`NETWORK_GRACE`].
    pub fn lost(&self, hash: &TokenHash) {
        let now = Instant::now();
        let mut entries = self.0.lock().expect("lock");
        for entry in entries.iter_mut().filter(|e| e.kept.hash == *hash) {
            entry.expires = Some(now + NETWORK_GRACE);
        }
    }

    /// The session ended properly: no coming back.
    pub fn forget(&self, hash: &TokenHash) {
        self.0
            .lock()
            .expect("lock")
            .retain(|e| e.kept.hash != *hash);
    }

    /// What to keep of a session's token for after a restart.
    pub fn kept(&self, hash: &TokenHash) -> Option<KeptResume> {
        self.0
            .lock()
            .expect("lock")
            .iter()
            .find(|e| e.kept.hash == *hash)
            .map(|e| e.kept.clone())
    }

    /// Brings back a token kept before a restart, good for `left` more.
    pub fn restore(&self, kept: KeptResume, left: Duration) {
        self.0.lock().expect("lock").push(Entry {
            kept,
            expires: Some(Instant::now() + left),
            displaced: Arc::default(),
        });
    }
}

fn purge(entries: &mut Vec<Entry>, now: Instant) {
    entries.retain(|e| e.expires.is_none_or(|until| until > now));
}

#[cfg(test)]
mod tests {
    use super::*;
    use hext_crypto::DeviceKey;
    use hext_proto::Permissions;

    fn key() -> PublicKey {
        DeviceKey::generate().unwrap().public_key()
    }

    fn grant() -> Grant {
        Grant {
            permissions: Permissions::CONTROL,
            name: "Técnico".into(),
            id: "482 913 557".into(),
            needs_password: false,
            remember: false,
            bound: false,
        }
    }

    #[test]
    fn a_token_brings_its_viewer_back_once() {
        let (tokens, viewer) = (ResumeTokens::default(), key());
        let (token, hash, _) = tokens.open(viewer, grant());
        tokens.lost(&hash);
        assert_eq!(tokens.take(&[0; 32], &viewer), None);
        assert_eq!(tokens.take(&token, &viewer), Some(grant()));
        assert_eq!(tokens.take(&token, &viewer), None, "used up");
    }

    #[test]
    fn it_is_no_good_to_another_viewer() {
        let (tokens, viewer) = (ResumeTokens::default(), key());
        let (token, _, _) = tokens.open(viewer, grant());
        assert_eq!(tokens.take(&token, &key()), None);
        assert!(tokens.take(&token, &viewer).is_some());
    }

    #[test]
    fn it_expires() {
        let (tokens, viewer) = (ResumeTokens::default(), key());
        let (token, hash, _) = tokens.open(viewer, grant());
        tokens.lost(&hash);
        let late = Instant::now() + NETWORK_GRACE + Duration::from_secs(1);
        assert_eq!(tokens.take_at(&token, &viewer, late), None);
    }

    #[test]
    fn a_session_that_ended_properly_leaves_none() {
        let (tokens, viewer) = (ResumeTokens::default(), key());
        let (token, hash, _) = tokens.open(viewer, grant());
        tokens.forget(&hash);
        assert!(!tokens.displace(&viewer));
        assert_eq!(tokens.take(&token, &viewer), None);
    }

    #[tokio::test]
    async fn a_returning_viewer_ends_the_session_left_behind() {
        let (tokens, viewer) = (ResumeTokens::default(), key());
        let (_, _, displaced) = tokens.open(viewer, grant());
        assert!(tokens.displace(&viewer));
        displaced.notified().await;
        assert!(!tokens.displace(&key()), "only its own");
    }

    #[test]
    fn it_survives_a_restart_as_kept() {
        let (before, viewer) = (ResumeTokens::default(), key());
        let (token, hash, _) = before.open(viewer, grant());
        let kept = before.kept(&hash).unwrap();
        let after = ResumeTokens::default();
        after.restore(kept, RESTART_LIFETIME);
        assert!(after.displace(&viewer));
        assert_eq!(after.take(&token, &viewer), Some(grant()));
    }
}
