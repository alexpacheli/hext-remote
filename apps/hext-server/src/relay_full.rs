//! The relay without the per-device limit, for the accounts on a list kept
//! by hand on the server. Their devices and computers are told to use it
//! (at login), and whoever they open a session with may use it for that
//! session.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hext_crypto::PublicKey;
use hext_signal::RelayHint;
use tracing::warn;

/// How long the other side of a session may start using the relay: enough
/// to reach it (once in, it stays until it leaves).
const GUEST_WINDOW: Duration = Duration::from_secs(120);

/// The relay without the limit, and who may use it.
#[derive(Debug, Clone)]
pub struct RelayFull {
    /// Its URL, as the apps reach it.
    pub url: String,
    /// Its QUIC address discovery port (the other relay has the default).
    pub quic_port: u16,
    /// The accounts' e-mails, one per line (`#` starts a comment). Read on
    /// each check: an edit counts at once, with nothing to restart.
    pub accounts: PathBuf,
}

impl RelayFull {
    /// What the apps are told.
    pub fn hint(&self) -> RelayHint {
        RelayHint {
            url: self.url.clone(),
            quic_port: self.quic_port,
        }
    }

    /// Whether one of `emails` is on the list.
    pub fn lists(&self, emails: &[String]) -> bool {
        if emails.is_empty() {
            return false;
        }
        match std::fs::read_to_string(&self.accounts) {
            Ok(text) => listed(&text, emails),
            Err(e) => {
                warn!("relay sem limite: {}: {e}", self.accounts.display());
                false
            }
        }
    }
}

/// Whether the list `text` has one of `emails` (case does not matter).
fn listed(text: &str, emails: &[String]) -> bool {
    text.lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|line| !line.is_empty())
        .any(|line| emails.iter().any(|e| e.eq_ignore_ascii_case(line)))
}

/// Devices let in for a session with a listed account, until when.
#[derive(Default)]
pub(crate) struct Guests(Mutex<HashMap<PublicKey, Instant>>);

impl Guests {
    pub(crate) fn let_in(&self, keys: &[PublicKey]) {
        let now = Instant::now();
        let mut guests = self.0.lock().expect("lock");
        guests.retain(|_, until| *until > now);
        for key in keys {
            guests.insert(*key, now + GUEST_WINDOW);
        }
    }

    pub(crate) fn admits(&self, key: &PublicKey) -> bool {
        self.0
            .lock()
            .expect("lock")
            .get(key)
            .is_some_and(|until| *until > Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_takes_comments_blank_lines_and_any_case() {
        let text = "# contas liberadas\n\nAlex@Exemplo.com.br  # eu\noutra@exemplo.com\n";
        assert!(listed(text, &["alex@exemplo.com.br".into()]));
        assert!(listed(text, &["x@y.z".into(), "OUTRA@exemplo.com".into()]));
        assert!(!listed(text, &["ninguem@exemplo.com".into()]));
        assert!(!listed(text, &["# contas liberadas".into()]));
        assert!(!listed(text, &[]));
    }

    #[test]
    fn a_guest_is_let_in_for_a_while() {
        let guests = Guests::default();
        let key = hext_crypto::DeviceKey::generate().unwrap().public_key();
        assert!(!guests.admits(&key));
        guests.let_in(&[key]);
        assert!(guests.admits(&key));
    }
}
