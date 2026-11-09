//! The installed host. On Windows (F4, milestone 1): a service, online with
//! the computer's own ID from boot, and an agent in the user's session (see
//! [`crate::agent`]). On macOS (milestone 9): a LaunchAgent that opens the
//! app at login, with no window.

use std::sync::Arc;

use anyhow::Result;
use hext_session::ResumeTokens;
use hext_session::host::Restart;

use crate::ServiceAction;

#[cfg(any(windows, target_os = "macos"))]
mod access;
#[cfg(windows)]
mod banner;
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
mod kept;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use banner::Banner;

/// Above this size a log starts over (the previous one kept as `.old`).
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

/// Whether this computer has the service (on macOS, the LaunchAgent)
/// installed.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub fn installed() -> bool {
    #[cfg(windows)]
    {
        windows::installed()
    }
    #[cfg(target_os = "macos")]
    {
        macos::installed()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        false
    }
}

/// For the app's own host: what restarts this computer, with the token
/// kept before the last restart taken back into `resumes`. Only installed on
/// macOS: on Windows, the service restarts it, and the portable app cannot
/// come back by itself.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub fn own_restart(resumes: &ResumeTokens) -> Option<Arc<dyn Restart>> {
    #[cfg(target_os = "macos")]
    {
        macos::own_restart(resumes)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = resumes;
        None
    }
}

/// Notes this computer's ID for `service status`, where the app's own host
/// is the installed one (macOS).
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub fn remember_id(id: &str) {
    #[cfg(target_os = "macos")]
    if macos::installed() {
        macos::remember_id(id);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = id;
}

/// What `service name` was asked.
pub enum NameChange {
    Show,
    Set(String),
    /// Back to the system's name.
    Reset,
}

/// Writes `change` in `dir`; whether anything changed.
fn apply_name(dir: &std::path::Path, change: NameChange) -> Result<bool> {
    match change {
        NameChange::Show => Ok(false),
        NameChange::Set(name) => crate::name::write(dir, Some(&name)).map(|()| true),
        NameChange::Reset => crate::name::write(dir, None).map(|()| true),
    }
}

/// The name of the app opened by hand (and of the viewer): the user's.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn user_name(change: NameChange) -> Result<()> {
    let dir = hext_crypto::config_dir()?;
    let changed = apply_name(&dir, change)?;
    println!("Nome deste computador: {}", crate::name::described(&dir));
    if changed {
        println!("Vale a partir da próxima vez que o Hext Remote abrir.");
    }
    Ok(())
}

pub fn command(action: ServiceAction) -> Result<()> {
    let action = match action {
        ServiceAction::Name { name, reset } => {
            let change = match name {
                Some(name) => NameChange::Set(crate::name::clean(&name)?),
                None if reset => NameChange::Reset,
                None => NameChange::Show,
            };
            #[cfg(windows)]
            return windows::name(change);
            #[cfg(target_os = "macos")]
            return macos::name(change);
            #[cfg(not(any(windows, target_os = "macos")))]
            return user_name(change);
        }
        ServiceAction::Install { name: Some(name) } => ServiceAction::Install {
            name: Some(crate::name::clean(&name)?),
        },
        action => action,
    };
    #[cfg(windows)]
    {
        match action {
            ServiceAction::Install { name } => windows::install(name),
            ServiceAction::Uninstall { purge } => windows::uninstall(purge),
            ServiceAction::Status => windows::status(),
            ServiceAction::Password { off, pipe } => windows::password(off, pipe.as_deref()),
            ServiceAction::Technicians { revoke, revoke_all } => {
                windows::technicians(revoke.as_deref(), revoke_all)
            }
            ServiceAction::Account { chain, off } => windows::account(chain.as_deref(), off),
            ServiceAction::Run => windows::run(),
            ServiceAction::Name { .. } => unreachable!("answered above"),
        }
    }
    #[cfg(target_os = "macos")]
    {
        match action {
            ServiceAction::Install { name } => macos::install(name),
            ServiceAction::Uninstall { purge } => macos::uninstall(purge),
            ServiceAction::Status => macos::status(),
            ServiceAction::Password { off, .. } => macos::password(off),
            ServiceAction::Technicians { revoke, revoke_all } => {
                macos::technicians(revoke.as_deref(), revoke_all)
            }
            ServiceAction::Account { chain, off } => macos::account(chain.as_deref(), off),
            ServiceAction::Run => anyhow::bail!("no Mac, quem inicia o Hext Remote é o login"),
            ServiceAction::Name { .. } => unreachable!("answered above"),
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = action;
        anyhow::bail!("o serviço por enquanto só existe no Windows e no macOS")
    }
}

/// What the host of the app opened at login takes to let people in without
/// anyone accepting (macOS, F6 marco 3): the password, the technicians and
/// the account.
pub struct LoginAccess {
    pub unattended: hext_session::host::UnattendedAccess,
    pub owners: Arc<dyn hext_session::host::Owners>,
    pub attempts: tokio::sync::mpsc::UnboundedReceiver<hext_session::host::Attempt>,
    pub ask: Box<dyn crate::signaling::Ask>,
}

/// The unattended access of the installed app on macOS (`None` elsewhere,
/// or not installed). Keeps `hub` up to date with it.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub fn login_access(
    key: &Arc<hext_crypto::DeviceKey>,
    hub: &crate::ui_link::UiHub,
) -> Option<LoginAccess> {
    #[cfg(target_os = "macos")]
    {
        macos::login_access(key, hub)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (key, hub);
        None
    }
}

/// Sets the password typed in the app's screen (`Some`), or turns it off
/// (`None`). On Windows, it takes an administrator: Windows asks.
// (Linux has no app window: only the CLI.)
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub async fn set_password_from_app(password: Option<String>) -> Result<()> {
    let password = password.map(zeroize::Zeroizing::new);
    #[cfg(windows)]
    {
        match password {
            Some(password) => windows::password_from_app(&password).await,
            None => windows::password_off_from_app().await,
        }
    }
    #[cfg(target_os = "macos")]
    {
        tokio::task::spawn_blocking(move || {
            macos::set_password_from_app(password.as_deref().map(|p| p.as_str()))
        })
        .await?
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = password;
        anyhow::bail!("só no Windows e no Mac")
    }
}

/// Binds (`Some`) or unbinds the account from the app (macOS: no
/// administrator, the access is the user's).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn account_from_app(chain: Option<&str>) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos::account(chain, chain.is_none())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = chain;
        anyhow::bail!("só no Mac")
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
