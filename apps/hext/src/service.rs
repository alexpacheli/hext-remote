//! The installed host (F4, milestone 1): a Windows service, online with the
//! computer's own ID from boot, and an agent in the user's session (see
//! [`crate::agent`]). macOS comes later (milestone 9).

use anyhow::Result;

use crate::ServiceAction;

#[cfg(windows)]
mod banner;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use banner::Banner;

/// Above this size a log starts over (the previous one kept as `.old`).
#[cfg_attr(not(windows), allow(dead_code))]
const MAX_LOG: u64 = 4 << 20;

/// The log file of the service or an agent (`name`), in the computer's
/// folder; `None` where there is none.
pub fn log_file(name: &str) -> Option<std::fs::File> {
    log_in(&hext_crypto::machine_dir().ok()?, name)
}

/// The interface's log file, in the user's folder (it runs as the user).
pub fn user_log_file(name: &str) -> Option<std::fs::File> {
    let dir = hext_crypto::config_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    log_in(&dir, name)
}

fn log_in(dir: &std::path::Path, name: &str) -> Option<std::fs::File> {
    let path = dir.join(format!("{name}.log"));
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG) {
        let _ = std::fs::rename(&path, dir.join(format!("{name}.old.log")));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

/// Whether this computer has the service installed.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub fn installed() -> bool {
    #[cfg(windows)]
    {
        windows::installed()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub fn command(action: ServiceAction) -> Result<()> {
    #[cfg(windows)]
    {
        match action {
            ServiceAction::Install => windows::install(),
            ServiceAction::Uninstall { purge } => windows::uninstall(purge),
            ServiceAction::Status => windows::status(),
            ServiceAction::Password { off } => windows::password(off),
            ServiceAction::Run => windows::run(),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = action;
        anyhow::bail!("o serviço por enquanto só existe no Windows")
    }
}

pub fn agent(pipe: &str) -> Result<()> {
    #[cfg(windows)]
    {
        windows::agent(pipe)
    }
    #[cfg(not(windows))]
    {
        let _ = pipe;
        anyhow::bail!("o agent só existe no Windows")
    }
}
