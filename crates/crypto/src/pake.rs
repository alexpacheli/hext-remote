//! The unattended password: SPAKE2 over the connection already open, so the
//! password never travels and neither a relay nor the server can learn it or
//! stand in for a side. Both sides' keys go into the exchange: it holds only
//! between this viewer and this host.
//!
//! The password is stretched with Argon2id into a [`Verifier`] (what the host
//! stores) and only that goes into SPAKE2. Both sides then confirm with a key
//! made by the exchange; a wrong password shows up as a wrong confirmation.
//! Being the input to the exchange, the secret in the file works like the
//! password itself: it stays sealed, and readable by the system only.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Params, Version};
use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use zeroize::Zeroizing;

use crate::{CryptoError, PublicKey, Result, constant_time_eq, protect};

pub const SALT_LEN: usize = 16;
pub const CONFIRMATION_LEN: usize = 32;
const SECRET_LEN: usize = 32;

/// Argon2id cost. Both sides must use the same, so they stay fixed here; the
/// host's salt travels, these do not.
const ARGON2_MEMORY_KIB: u32 = 64 * 1024;
const ARGON2_PASSES: u32 = 3;
const ARGON2_LANES: u32 = 1;

/// File header, and the size after it once unsealed.
const MAGIC: &[u8; 8] = b"HEXTPWD1";
const STORED_LEN: usize = SALT_LEN + SECRET_LEN;
/// More than any sealed file takes.
const MAX_FILE_LEN: u64 = 4096;

/// The stretched password.
pub type Secret = Zeroizing<[u8; SECRET_LEN]>;

/// A password stretched with `salt`. Slow on purpose: call it off the async
/// threads.
pub fn stretch(password: &str, salt: &[u8; SALT_LEN]) -> Result<Secret> {
    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_PASSES,
        ARGON2_LANES,
        Some(SECRET_LEN),
    )
    .map_err(|_| CryptoError::Derive)?;
    let mut secret = Zeroizing::new([0; SECRET_LEN]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, secret.as_mut_slice())
        .map_err(|_| CryptoError::Derive)?;
    Ok(secret)
}

/// What the host keeps of the password.
pub struct Verifier {
    salt: [u8; SALT_LEN],
    secret: Secret,
}

impl Verifier {
    /// For a new password, with a new salt.
    pub fn new(password: &str) -> Result<Self> {
        let mut salt = [0; SALT_LEN];
        SystemRandom::new()
            .fill(&mut salt)
            .map_err(|_| CryptoError::Random)?;
        Ok(Self {
            salt,
            secret: stretch(password, &salt)?,
        })
    }

    pub fn salt(&self) -> &[u8; SALT_LEN] {
        &self.salt
    }

    /// The verifier in `path`, or `None` if there is none (unattended access
    /// is off).
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(CryptoError::Io {
                    path: path.to_owned(),
                    source,
                });
            }
        };
        let bad = || CryptoError::PasswordFile(path.to_owned());
        if bytes.len() as u64 > MAX_FILE_LEN {
            return Err(bad());
        }
        let sealed = bytes.strip_prefix(MAGIC).ok_or_else(bad)?;
        let stored = protect::unseal(sealed).map_err(|_| bad())?;
        if stored.len() != STORED_LEN {
            return Err(bad());
        }
        let (salt, secret) = stored.split_at(SALT_LEN);
        Ok(Some(Self {
            salt: salt.try_into().map_err(|_| bad())?,
            secret: Zeroizing::new(secret.try_into().map_err(|_| bad())?),
        }))
    }

    /// Stores this, sealed for the machine, replacing what is in `path`. The
    /// folder must be closed to all but the system and administrators.
    pub fn save(&self, path: &Path) -> Result<()> {
        let io = |source| CryptoError::Io {
            path: path.to_owned(),
            source,
        };
        let mut stored = Zeroizing::new(Vec::with_capacity(STORED_LEN));
        stored.extend_from_slice(&self.salt);
        stored.extend_from_slice(self.secret.as_slice());
        let sealed = protect::seal(&stored, true).map_err(io)?;
        let mut file = MAGIC.to_vec();
        file.extend_from_slice(&sealed);
        // All of it or none: a half-written file must not turn the access off.
        let new: PathBuf = path.with_extension("new");
        fs::write(&new, file).map_err(io)?;
        fs::rename(&new, path).map_err(io)
    }

    /// Turns unattended access off.
    pub fn remove(path: &Path) -> Result<()> {
        match fs::remove_file(path) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(CryptoError::Io {
                path: path.to_owned(),
                source: e,
            }),
            _ => Ok(()),
        }
    }
}

type Exchange = Spake2<Ed25519Group>;

fn identity(key: &PublicKey) -> Identity {
    Identity::new(key.as_bytes())
}

/// The confirmation of a key by one side: saying it proves that side reached
/// the same key.
fn confirmation(key: &[u8], side: &[u8]) -> [u8; CONFIRMATION_LEN] {
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), side);
    let mut out = [0; CONFIRMATION_LEN];
    out.copy_from_slice(tag.as_ref());
    out
}

const VIEWER_SIDE: &[u8] = b"hext-unattended-v1 viewer";
const HOST_SIDE: &[u8] = b"hext-unattended-v1 host";

/// The host's part of an exchange.
pub struct HostExchange(Exchange);

impl HostExchange {
    /// Starts it; the message goes to the viewer.
    pub fn start(verifier: &Verifier, viewer: &PublicKey, host: &PublicKey) -> (Self, Vec<u8>) {
        let (exchange, message) = Exchange::start_b(
            &Password::new(verifier.secret.as_slice()),
            &identity(viewer),
            &identity(host),
        );
        (Self(exchange), message)
    }

    /// The viewer's answer: the host's confirmation if it knew the password,
    /// `None` if not (or if the message is not one).
    pub fn finish(self, message: &[u8], proof: &[u8]) -> Option<[u8; CONFIRMATION_LEN]> {
        let key = Zeroizing::new(self.0.finish(message).ok()?);
        constant_time_eq(&confirmation(&key, VIEWER_SIDE), proof)
            .then(|| confirmation(&key, HOST_SIDE))
    }
}

/// The viewer's part of an exchange.
pub struct ViewerExchange(Exchange);

/// What the viewer has once it took the host's message.
pub struct ViewerKey(Zeroizing<Vec<u8>>);

impl ViewerExchange {
    /// Starts it; the message goes to the host.
    pub fn start(secret: &Secret, viewer: &PublicKey, host: &PublicKey) -> (Self, Vec<u8>) {
        let (exchange, message) = Exchange::start_a(
            &Password::new(secret.as_slice()),
            &identity(viewer),
            &identity(host),
        );
        (Self(exchange), message)
    }

    /// Takes the host's message; `None` if it is not one.
    pub fn finish(self, message: &[u8]) -> Option<ViewerKey> {
        Some(ViewerKey(Zeroizing::new(self.0.finish(message).ok()?)))
    }
}

impl ViewerKey {
    /// What proves to the host that this side knows the password.
    pub fn proof(&self) -> [u8; CONFIRMATION_LEN] {
        confirmation(&self.0, VIEWER_SIDE)
    }

    /// Whether the host knew it, too.
    pub fn host_confirmed(&self, from_host: &[u8]) -> bool {
        constant_time_eq(&confirmation(&self.0, HOST_SIDE), from_host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DeviceKey;

    fn key() -> PublicKey {
        DeviceKey::generate().unwrap().public_key()
    }

    /// Runs an exchange; whether each side was satisfied with the other.
    fn run(
        verifier: &Verifier,
        typed: &str,
        seen_by_host: (PublicKey, PublicKey),
        seen_by_viewer: (PublicKey, PublicKey),
    ) -> (bool, bool) {
        let (viewer, host) = seen_by_host;
        let (host_side, to_viewer) = HostExchange::start(verifier, &viewer, &host);
        let secret = stretch(typed, verifier.salt()).unwrap();
        let (viewer, host) = seen_by_viewer;
        let (viewer_side, to_host) = ViewerExchange::start(&secret, &viewer, &host);
        let key = viewer_side.finish(&to_viewer).unwrap();
        match host_side.finish(&to_host, &key.proof()) {
            Some(confirmation) => (true, key.host_confirmed(&confirmation)),
            None => (false, false),
        }
    }

    #[test]
    fn the_right_password_passes_both_ways() {
        let verifier = Verifier::new("cavalo correto").unwrap();
        let keys = (key(), key());
        assert_eq!(run(&verifier, "cavalo correto", keys, keys), (true, true));
    }

    #[test]
    fn a_wrong_password_fails() {
        let verifier = Verifier::new("cavalo correto").unwrap();
        let keys = (key(), key());
        assert_eq!(run(&verifier, "cavalo errado", keys, keys), (false, false));
    }

    #[test]
    fn it_holds_only_between_those_two_keys() {
        let verifier = Verifier::new("cavalo correto").unwrap();
        let (viewer, host) = (key(), key());
        // Someone in the middle: each side sees another key.
        let other_host = run(&verifier, "cavalo correto", (viewer, host), (viewer, key()));
        assert_eq!(other_host, (false, false));
        let other_viewer = run(&verifier, "cavalo correto", (viewer, host), (key(), host));
        assert_eq!(other_viewer, (false, false));
    }

    #[test]
    fn a_host_without_the_password_is_found_out() {
        let verifier = Verifier::new("cavalo correto").unwrap();
        let (viewer, host) = (key(), key());
        let impostor = Verifier::new("outra coisa").unwrap();
        let (_, to_viewer) = HostExchange::start(&impostor, &viewer, &host);
        let secret = stretch("cavalo correto", verifier.salt()).unwrap();
        let (viewer_side, _) = ViewerExchange::start(&secret, &viewer, &host);
        let key = viewer_side.finish(&to_viewer).unwrap();
        assert!(!key.host_confirmed(&[0; CONFIRMATION_LEN]));
    }

    #[test]
    fn garbage_is_a_failure_not_a_panic() {
        let verifier = Verifier::new("cavalo correto").unwrap();
        let (viewer, host) = (key(), key());
        let (side, _) = HostExchange::start(&verifier, &viewer, &host);
        assert!(side.finish(&[1, 2, 3], &[0; CONFIRMATION_LEN]).is_none());
    }

    #[test]
    fn salts_differ() {
        let (a, b) = (Verifier::new("x").unwrap(), Verifier::new("x").unwrap());
        assert_ne!(a.salt(), b.salt());
        assert_ne!(a.secret.as_slice(), b.secret.as_slice());
    }

    #[cfg(windows)]
    #[test]
    fn it_is_kept_sealed_and_can_go() {
        let dir = std::env::temp_dir().join(format!("hext-pake-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("unattended.key");
        let none = Verifier::load(&path).unwrap().is_none();

        let verifier = Verifier::new("cavalo correto").unwrap();
        verifier.save(&path).unwrap();
        let on_disk = fs::read(&path).unwrap();
        let in_the_clear = on_disk
            .windows(SECRET_LEN)
            .any(|w| w == verifier.secret.as_slice());
        let loaded = Verifier::load(&path).unwrap().unwrap();
        fs::write(&path, b"HEXTPWD1 nothing sealed here").unwrap();
        let corrupt = Verifier::load(&path);
        Verifier::remove(&path).unwrap();
        Verifier::remove(&path).unwrap();
        let _ = fs::remove_dir_all(&dir);

        assert!(none);
        assert!(!in_the_clear);
        assert_eq!(loaded.salt(), verifier.salt());
        assert_eq!(loaded.secret.as_slice(), verifier.secret.as_slice());
        assert!(corrupt.is_err());
    }
}
