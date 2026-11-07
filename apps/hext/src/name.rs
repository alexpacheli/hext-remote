//! The computer's name, as the other side sees it: in the session, in the
//! list of computers and in the question before accepting. The hostname
//! unless someone gave it another (`service name`, F4 milestone 10).
//!
//! It lives in a `name.txt`: in the computer's folder for the Windows
//! service, in the user's folder otherwise (the portable app, the viewer,
//! the installed Mac).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

const FILE: &str = "name.txt";

/// The longest name the server keeps.
const MAX: usize = hext_signal::MAX_NAME_LEN;

/// The system's name for this computer.
pub fn hostname() -> String {
    gethostname::gethostname().to_string_lossy().into_owned()
}

/// A name as typed, made fit to show: no control or direction marks, no
/// spaces around it.
pub fn clean(typed: &str) -> Result<String> {
    let kept: String = typed
        .chars()
        .filter(|&c| crate::signaling::shows(c))
        .collect();
    let name = kept.trim();
    if name.is_empty() {
        bail!("o nome está vazio");
    }
    if name.chars().count() > MAX {
        bail!("o nome passa de {MAX} caracteres");
    }
    Ok(name.to_owned())
}

fn file(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// The name given in `dir`, if any.
pub fn read(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(file(dir)).ok()?;
    clean(&text).ok()
}

/// The name in `dir`, or the hostname.
pub fn in_dir(dir: &Path) -> String {
    read(dir).unwrap_or_else(hostname)
}

/// Gives the name in `dir`; `None` goes back to the hostname.
pub fn write(dir: &Path, name: Option<&str>) -> Result<()> {
    let path = file(dir);
    match name {
        Some(name) => {
            std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
            std::fs::write(&path, name).with_context(|| path.display().to_string())
        }
        None => match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).with_context(|| path.display().to_string())
            }
            _ => Ok(()),
        },
    }
}

/// This user's name for the computer (portable app, viewer, installed Mac).
pub fn of_user() -> String {
    match hext_crypto::config_dir() {
        Ok(dir) => in_dir(&dir),
        Err(_) => hostname(),
    }
}

/// How `dir`'s name reads in `service status` and `service name`.
pub fn described(dir: &Path) -> String {
    let system = || format!("{} (o nome do sistema)", hostname());
    match std::fs::read_to_string(file(dir)) {
        Ok(text) => clean(&text).unwrap_or_else(|_| system()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => system(),
        Err(_) => "rode como administrador para ver".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hext-name-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_name_is_shown_as_text_only() {
        assert_eq!(clean("  Recepção\u{202E} 2 \n").unwrap(), "Recepção 2");
        assert!(clean(" \u{200F} ").is_err());
        assert!(clean(&"x".repeat(MAX + 1)).is_err());
        assert_eq!(clean(&"ç".repeat(MAX)).unwrap().chars().count(), MAX);
        // A mark pasted in does not count.
        let marked = format!("\u{200F}{}", "x".repeat(MAX));
        assert_eq!(clean(&marked).unwrap().len(), MAX);
    }

    #[test]
    fn without_a_name_it_is_the_hostname() {
        let dir = temp_dir("back");
        assert_eq!(in_dir(&dir), hostname());
        write(&dir, Some("Caixa 1")).unwrap();
        assert_eq!(in_dir(&dir), "Caixa 1");
        write(&dir, None).unwrap();
        write(&dir, None).unwrap();
        let back = in_dir(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(back, hostname());
    }

    #[test]
    fn a_name_edited_by_hand_is_cleaned_too() {
        let dir = temp_dir("hand");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(file(&dir), "Sala\u{0007} 3\r\n").unwrap();
        let name = read(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(name.as_deref(), Some("Sala 3"));
    }
}
