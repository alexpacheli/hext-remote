//! Technicians this computer trusts without the unattended password (F4
//! marco 12): one got in with the password and asked to be remembered, so
//! its device key is written down here. No password is kept, only keys. The
//! list is the owner's to see and revoke, and it goes when the password
//! changes or is turned off.
// (Only the Windows service keeps such a list; elsewhere only the
// interface shows one.)
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::{Path, PathBuf};

use hext_crypto::PublicKey;
use serde::{Deserialize, Serialize};
use tracing::warn;

/// Most technicians kept: the oldest goes for a new one.
const MAX_TECHNICIANS: usize = 50;

/// A technician this computer trusts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Technician {
    /// Its device key (hex): what the trust is in.
    pub key: String,
    /// Its ID and name when it was bound, as the user saw them.
    pub id: String,
    pub name: String,
    /// Seconds since 1970.
    pub since: u64,
}

/// The list, kept in a file. (Read on each use: the command line changes it
/// too.)
#[derive(Debug, Clone)]
pub struct Technicians {
    file: PathBuf,
}

impl Technicians {
    pub fn new(file: PathBuf) -> Self {
        Self { file }
    }

    /// Where it is kept.
    pub fn path(&self) -> &Path {
        &self.file
    }

    /// The technicians, the newest first. A file that cannot be read counts
    /// as none: nobody is trusted by mistake.
    pub fn list(&self) -> Vec<Technician> {
        match std::fs::read(&self.file) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                warn!("{}: {e}", self.file.display());
                Vec::new()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                warn!("{}: {e}", self.file.display());
                Vec::new()
            }
        }
    }

    /// Whether the device with `key` is trusted.
    pub fn bound(&self, key: &PublicKey) -> bool {
        let key = key.to_string();
        self.list().iter().any(|t| t.key == key)
    }

    /// Trusts `key` from now on (again, if it already was).
    pub fn bind(&self, key: &PublicKey, id: &str, name: &str) -> std::io::Result<()> {
        let key = key.to_string();
        let mut list = self.list();
        list.retain(|t| t.key != key);
        list.insert(
            0,
            Technician {
                key,
                id: id.to_owned(),
                name: name.to_owned(),
                since: unix_now(),
            },
        );
        list.truncate(MAX_TECHNICIANS);
        self.write(&list)
    }

    /// Stops trusting the technicians that `which` names: by key, or by ID
    /// (spaces do not count). Those that went.
    pub fn revoke(&self, which: &str) -> std::io::Result<Vec<Technician>> {
        let digits = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        let which = digits(which);
        if which.is_empty() {
            return Ok(Vec::new());
        }
        let (gone, kept): (Vec<_>, Vec<_>) = self
            .list()
            .into_iter()
            .partition(|t| t.key.eq_ignore_ascii_case(&which) || digits(&t.id) == which);
        if !gone.is_empty() {
            self.write(&kept)?;
        }
        Ok(gone)
    }

    /// Trusts nobody anymore.
    pub fn clear(&self) -> std::io::Result<()> {
        match std::fs::remove_file(&self.file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Whole or not at all: written aside, then put in place.
    fn write(&self, list: &[Technician]) -> std::io::Result<()> {
        if list.is_empty() {
            return self.clear();
        }
        let bytes = serde_json::to_vec_pretty(list).map_err(std::io::Error::other)?;
        let aside = self.file.with_extension("new");
        std::fs::write(&aside, bytes)?;
        std::fs::rename(&aside, &self.file)
    }
}

/// `since` as a day (UTC), for the command line.
pub fn date(since: u64) -> String {
    // Days since 1970 to a civil date (Howard Hinnant's algorithm).
    let days = (since / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{day:02}/{month:02}/{year}")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> PublicKey {
        PublicKey::from_bytes([n; 32])
    }

    /// A store of its own for test `name`, empty.
    fn store(name: &str) -> Technicians {
        let dir =
            std::env::temp_dir().join(format!("hext-technicians-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Technicians::new(dir.join("technicians.json"))
    }

    #[test]
    fn a_bound_key_is_trusted_and_others_are_not() {
        let store = store("1");
        assert!(!store.bound(&key(1)));
        store.bind(&key(1), "482 913 557", "Técnico").unwrap();
        assert!(store.bound(&key(1)));
        assert!(!store.bound(&key(2)));
        let list = store.list();
        assert_eq!(list.len(), 1);
        assert_eq!(
            (list[0].id.as_str(), list[0].name.as_str()),
            ("482 913 557", "Técnico")
        );
    }

    #[test]
    fn binding_again_moves_it_first_without_doubling() {
        let store = store("2");
        store.bind(&key(1), "1", "Um").unwrap();
        store.bind(&key(2), "2", "Dois").unwrap();
        store.bind(&key(1), "1", "Um de novo").unwrap();
        let names: Vec<_> = store.list().into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["Um de novo", "Dois"]);
    }

    #[test]
    fn revoking_by_id_or_key_takes_it_out() {
        let store = store("3");
        store.bind(&key(1), "482 913 557", "Um").unwrap();
        store.bind(&key(2), "111 222 333", "Dois").unwrap();
        let gone = store.revoke("482913557").unwrap();
        assert_eq!(gone.len(), 1);
        assert!(!store.bound(&key(1)));
        assert!(store.bound(&key(2)));
        store.revoke(&key(2).to_string()).unwrap();
        assert!(store.list().is_empty());
        assert!(!store.path().exists(), "nothing left: no file");
        assert!(store.revoke("").unwrap().is_empty());
        assert!(store.revoke("999").unwrap().is_empty());
    }

    #[test]
    fn clearing_trusts_nobody() {
        let store = store("4");
        store.clear().unwrap();
        store.bind(&key(1), "1", "Um").unwrap();
        store.clear().unwrap();
        assert!(!store.bound(&key(1)));
    }

    #[test]
    fn a_garbled_file_trusts_nobody() {
        let store = store("5");
        std::fs::write(store.path(), b"{not json").unwrap();
        assert!(!store.bound(&key(1)));
        assert!(store.list().is_empty());
    }

    #[test]
    fn dates_read_as_days() {
        assert_eq!(date(0), "01/01/1970");
        assert_eq!(date(1_794_009_600), "07/11/2026");
        assert_eq!(date(951_782_400), "29/02/2000");
    }

    #[test]
    fn the_list_has_a_limit() {
        let store = store("6");
        for n in 0..=MAX_TECHNICIANS as u8 {
            store.bind(&key(n), &n.to_string(), "T").unwrap();
        }
        let list = store.list();
        assert_eq!(list.len(), MAX_TECHNICIANS);
        assert!(!store.bound(&key(0)), "the oldest went");
    }
}
