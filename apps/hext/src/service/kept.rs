//! What an installed host keeps on disk for across a restart: the hash of
//! the viewer's way back (`resume.json`) and the audit log. Windows keeps
//! them in the computer's folder; macOS, in the user's (see
//! [`super::macos`]).

use std::path::{Path, PathBuf};
use std::time::Duration;

use hext_proto::Permissions;
use hext_session::{Grant, KeptResume, RESTART_LIFETIME, ResumeTokens};
use tracing::{info, warn};

/// What is kept of a token for after a restart. Only its hash: the file lets
/// nobody in.
#[derive(serde::Serialize, serde::Deserialize)]
struct KeptFile {
    hash: [u8; 32],
    viewer: [u8; 32],
    permissions: Permissions,
    name: String,
    id: String,
    needs_password: bool,
    /// Came in as a trusted technician (older files: no).
    #[serde(default)]
    bound: bool,
    /// Seconds since 1970 (nothing else counts across a restart).
    until: u64,
}

fn kept_file(dir: &Path) -> PathBuf {
    dir.join("resume.json")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Writes `kept` in `dir`, good for the next [`RESTART_LIFETIME`].
pub fn keep(dir: &Path, kept: &KeptResume) -> Result<(), String> {
    let file = KeptFile {
        hash: kept.hash,
        viewer: *kept.viewer.as_bytes(),
        permissions: kept.grant.permissions,
        name: kept.grant.name.clone(),
        id: kept.grant.id.clone(),
        needs_password: kept.grant.needs_password,
        bound: kept.grant.bound,
        until: unix_now() + RESTART_LIFETIME.as_secs(),
    };
    let path = kept_file(dir);
    let text = serde_json::to_vec(&file).map_err(|e| e.to_string())?;
    write_private(&path, &text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Only this user reads it (on Windows, the folder is closed already).
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let _ = std::fs::remove_file(path);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

/// The token kept in `dir` is no longer good.
pub fn revoke(dir: &Path) {
    let _ = std::fs::remove_file(kept_file(dir));
}

/// Takes back the token kept in `dir` before a restart, if it is still good.
pub fn restore(dir: &Path, resumes: &ResumeTokens) {
    let path = kept_file(dir);
    let Ok(text) = std::fs::read(&path) else {
        return;
    };
    // Good for one start, however this goes.
    let _ = std::fs::remove_file(&path);
    let kept: KeptFile = match serde_json::from_slice(&text) {
        Ok(kept) => kept,
        Err(e) => {
            warn!("{}: {e}", path.display());
            return;
        }
    };
    let left = kept.until.saturating_sub(unix_now());
    // (A clock set back must not stretch it.)
    if left == 0 || left > RESTART_LIFETIME.as_secs() {
        return;
    }
    resumes.restore(
        KeptResume {
            hash: kept.hash,
            viewer: hext_crypto::PublicKey::from_bytes(kept.viewer),
            grant: Grant {
                permissions: kept.permissions,
                name: kept.name,
                id: kept.id,
                needs_password: kept.needs_password,
                remember: false,
                bound: kept.bound,
            },
        },
        Duration::from_secs(left),
    );
}

/// A line in the audit log (`log`), for the owner of the computer.
pub fn audit_to(log: Option<std::fs::File>, entry: &str) {
    use std::io::Write;
    info!("auditoria: {entry}");
    let Some(mut log) = log else {
        warn!("auditoria: o arquivo não abriu");
        return;
    };
    if let Err(e) = writeln!(log, "{} {entry}", utc(unix_now())) {
        warn!("auditoria: {e}");
    }
}

/// `seconds` since 1970 as a UTC date and time.
pub fn utc(seconds: u64) -> String {
    let (days, time) = (seconds / 86_400, seconds % 86_400);
    // From days to a civil date: years of 400 years' cycles, March first.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        time / 3600,
        time / 60 % 60,
        time % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hext-kept-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn grant() -> Grant {
        Grant {
            permissions: Permissions::default(),
            name: "Ana".into(),
            id: "123456789".into(),
            needs_password: false,
            remember: false,
            bound: false,
        }
    }

    #[test]
    fn dates_in_the_audit_are_utc() {
        assert_eq!(utc(0), "1970-01-01 00:00:00Z");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00:00Z");
        assert_eq!(utc(1_793_794_661), "2026-11-04 12:17:41Z");
    }

    #[test]
    fn the_token_comes_back_once_after_a_restart() {
        let dir = temp_dir("back");
        let viewer = hext_crypto::DeviceKey::generate().unwrap().public_key();
        let before = ResumeTokens::default();
        let (token, hash, _) = before.open(viewer, grant());
        keep(&dir, &before.kept(&hash).unwrap()).unwrap();

        let after = ResumeTokens::default();
        restore(&dir, &after);
        let again = ResumeTokens::default();
        restore(&dir, &again);

        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            after.take(&token, &viewer).map(|g| g.name),
            Some("Ana".into())
        );
        assert!(again.take(&token, &viewer).is_none(), "good for one start");
    }

    #[test]
    fn a_revoked_token_does_not_come_back() {
        let dir = temp_dir("revoked");
        let viewer = hext_crypto::DeviceKey::generate().unwrap().public_key();
        let before = ResumeTokens::default();
        let (token, hash, _) = before.open(viewer, grant());
        keep(&dir, &before.kept(&hash).unwrap()).unwrap();
        revoke(&dir);

        let after = ResumeTokens::default();
        restore(&dir, &after);

        let _ = std::fs::remove_dir_all(&dir);
        assert!(after.take(&token, &viewer).is_none());
    }
}
