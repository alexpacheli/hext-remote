//! The computers this one accessed, for reaching them again without typing
//! the ID: a list of this user's, on this computer. A name and an ID, never a
//! password. (An address book that follows the account comes with the
//! accounts, in F5.)

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::warn;

/// How many computers are kept.
const MAX: usize = 30;

/// One computer accessed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Computer {
    pub id: String,
    pub name: String,
    /// In Meus computadores: its key on the account's list, for taking it
    /// out (none: shared by another account, or not known yet). Never on a
    /// recent one, nor in the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// The list, the latest first.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Recent(Vec<Computer>);

impl Recent {
    fn file() -> Option<PathBuf> {
        Some(hext_crypto::config_dir().ok()?.join("computers.json"))
    }

    /// The list on disk; an unreadable one starts over, it is only a
    /// convenience.
    pub fn load() -> Self {
        let text = Self::file().and_then(|file| std::fs::read_to_string(file).ok());
        Self(text.map_or_else(Vec::new, |text| {
            serde_json::from_str(&text).unwrap_or_else(|e| {
                warn!("lista de computadores: {e}");
                Vec::new()
            })
        }))
    }

    pub fn save(&self) {
        let Some(file) = Self::file() else { return };
        let saved = serde_json::to_string_pretty(&self.0)
            .map_err(std::io::Error::other)
            .and_then(|text| hext_crypto::write_user_file(&file, text.as_bytes()));
        if let Err(e) = saved {
            warn!("{}: {e}", file.display());
        }
    }

    /// Computer `id` was just accessed: first on the list, under the name it
    /// goes by now.
    pub fn visited(&mut self, id: &str, name: &str) {
        self.forget(id);
        let computer = Computer {
            id: id.to_owned(),
            name: name.to_owned(),
            host: None,
        };
        self.0.insert(0, computer);
        self.0.truncate(MAX);
    }

    pub fn forget(&mut self, id: &str) {
        self.0.retain(|computer| computer.id != id);
    }

    pub fn into_list(self) -> Vec<Computer> {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(recent: &Recent) -> Vec<&str> {
        recent.0.iter().map(|c| c.id.as_str()).collect()
    }

    #[test]
    fn the_latest_comes_first_and_only_once() {
        let mut recent = Recent::default();
        recent.visited("1", "A");
        recent.visited("2", "B");
        recent.visited("1", "A renomeado");
        assert_eq!(ids(&recent), ["1", "2"]);
        assert_eq!(recent.0[0].name, "A renomeado");
    }

    #[test]
    fn it_can_be_taken_off_and_does_not_grow_forever() {
        let mut recent = Recent::default();
        for i in 0..MAX + 5 {
            recent.visited(&i.to_string(), "PC");
        }
        assert_eq!(recent.0.len(), MAX);
        assert_eq!(recent.0[0].id, (MAX + 4).to_string());
        recent.forget(&(MAX + 4).to_string());
        assert_eq!(recent.0.len(), MAX - 1);
    }
}
