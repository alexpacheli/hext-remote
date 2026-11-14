//! The installed app of one user (macOS and Linux): started at login, with
//! the unattended access (F6 marco 3) kept in that user's folder: the
//! password, the technicians and the account binding, set by this user
//! (`service password`, or the app's Segurança tab) and good once they are
//! signed in. What differs between the two systems (how it starts at login,
//! how the computer restarts) is in `super::os`.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, bail};
use hext_crypto::DeviceKey;
use hext_session::host::{BoxFuture, Restart, UnattendedAccess};
use hext_session::{KeptResume, Lockout, ResumeTokens};
use zeroize::Zeroizing;

use super::access::{Access, Password};
use super::os;
use crate::signaling::Ask;
use crate::ui_link::{UiAsk, UiHub};

/// A line in the audit log, for the owner of the computer; also for the
/// account's Atividade screen, while bound to one.
pub fn audit(entry: &str) {
    super::kept::audit_to(super::user_log_file("audit"), entry);
    if let Some(activity) = ACTIVITY.get() {
        let _ = activity.send(entry.to_owned());
    }
}

/// Where the installed app sends what the audit writes (unset elsewhere).
static ACTIVITY: std::sync::OnceLock<tokio::sync::mpsc::UnboundedSender<String>> =
    std::sync::OnceLock::new();

/// The unattended access, in this user's folder.
pub fn access() -> Result<Access> {
    Ok(Access::new(os::data_dir()?, audit))
}

/// The binding to an account, as this computer (the app's key) sees it.
pub fn owners() -> Result<crate::owners::Owners> {
    Ok(access()?.owners(Arc::new(crate::device_key(None)?)))
}

/// Only for the installed app, run by its user: the one that opens at
/// login reads these files.
fn require_installed() -> Result<()> {
    if os::is_root() {
        bail!(
            "{}, rode sem sudo: o acesso é do usuário que instalou",
            os::ON_THIS_SYSTEM
        );
    }
    if !os::installed() {
        bail!("instale primeiro ({} service install)", os::PROGRAM);
    }
    Ok(())
}

/// `service password`: sets (or changes) the unattended password, or turns
/// it off.
pub fn password(off: bool) -> Result<()> {
    require_installed()?;
    let (access, owners) = (access()?, owners()?);
    if off {
        access.password_off(&owners)?;
        println!("Acesso não assistido desligado.");
        return Ok(());
    }
    let password = Zeroizing::new(rpassword::prompt_password("Nova senha: ")?);
    let again = Zeroizing::new(rpassword::prompt_password("Repita a senha: ")?);
    if *password != *again {
        bail!("as senhas não são iguais");
    }
    access.set_password(&password, &owners)?;
    println!("Acesso não assistido ligado (depois do login deste usuário).");
    println!(
        "Durante a sessão, {} mostra quem está conectado.",
        os::THIS_COMPUTER
    );
    Ok(())
}

/// The Segurança tab's buttons: the password typed in the app's own screen
/// (`Some`), or turned off (`None`).
pub fn set_password_from_app(password: Option<&str>) -> Result<()> {
    require_installed()?;
    let (access, owners) = (access()?, owners()?);
    match password {
        Some(password) => access.set_password(password, &owners),
        None => access.password_off(&owners),
    }
}

/// `service technicians`.
pub fn technicians(revoke: Option<&str>, revoke_all: bool) -> Result<()> {
    if revoke.is_some() || revoke_all {
        require_installed()?;
    }
    access()?.technicians_command(revoke, revoke_all)
}

/// `service account`: binds this computer to the account with list
/// `chain`, unbinds it (`off`), or shows the binding.
pub fn account(chain: Option<&str>, off: bool) -> Result<()> {
    let owners = owners()?;
    if chain.is_none() && !off {
        Access::show_account(&owners);
        return Ok(());
    }
    require_installed()?;
    if off {
        access()?.unbind_account(&owners)?;
        println!("Conta desvinculada: todo acesso volta a pedir aceite.");
        return Ok(());
    }
    access()?.bind_account(&owners, chain.unwrap_or_default())
}

/// The installed app's unattended access, shown on `hub`.
pub fn login_access(key: &Arc<DeviceKey>, hub: &UiHub) -> Option<super::LoginAccess> {
    if !os::installed() {
        return None;
    }
    let access = access().ok()?;
    let owners = access.owners(key.clone());
    access.show_on(hub, &owners);
    let (activity, activity_out) = tokio::sync::mpsc::unbounded_channel();
    let _ = ACTIVITY.set(activity);
    tokio::spawn(crate::owners::report_activity(owners.clone(), activity_out));
    let (attempts, attempts_out) = tokio::sync::mpsc::unbounded_channel();
    Some(super::LoginAccess {
        unattended: UnattendedAccess {
            key: key.public_key(),
            lockout: Lockout::default(),
            password: Arc::new(Password {
                access: access.clone(),
                ui: hub.clone(),
                owners: Some(owners.clone()),
            }),
            attempts,
        },
        owners: Arc::new(owners.clone()),
        attempts: attempts_out,
        ask: Box::new(LoginAsk(UiAsk(hub.clone()), owners, access)),
    })
}

/// Asks on the app's window; lets in, without asking, the technicians and
/// the bound account's devices (while the password is on, for the
/// technicians).
struct LoginAsk(UiAsk, crate::owners::Owners, Access);

impl Ask for LoginAsk {
    fn ask<'a>(
        &'a mut self,
        name: &'a str,
        id: hext_signal::DeviceId,
    ) -> BoxFuture<'a, Option<hext_proto::Permissions>> {
        self.0.ask(name, id)
    }

    fn show_id(&mut self, id: hext_signal::DeviceId) {
        self.0.show_id(id);
    }

    fn unattended(&self) -> bool {
        self.2.verifier().is_some()
    }

    fn bound(&self, key: &hext_crypto::PublicKey) -> bool {
        self.2.technicians().bound(key)
    }

    fn owner<'a>(
        &'a mut self,
        key: &'a hext_crypto::PublicKey,
    ) -> BoxFuture<'a, Option<crate::owners::Way>> {
        Box::pin(super::access::owner_way(&self.1, key))
    }
}

/// Takes back the token kept before a restart, if it is still good, and
/// gives what restarts this computer.
pub fn own_restart(resumes: &ResumeTokens) -> Option<Arc<dyn Restart>> {
    if !os::installed() {
        return None;
    }
    let dir = os::data_dir().ok()?;
    super::kept::restore(&dir, resumes);
    Some(Arc::new(UserRestart(dir)))
}

/// Restarts this computer as its user would. The token waits in the user's
/// folder for the app's next start, at login.
struct UserRestart(PathBuf);

impl Restart for UserRestart {
    fn restart(&self, kept: &KeptResume, who: &str) -> std::result::Result<(), String> {
        super::kept::keep(&self.0, kept)?;
        audit(&format!("reinício pedido: {who}"));
        os::restart(&self.0)
    }

    fn revoke(&self) {
        super::kept::revoke(&self.0);
    }
}
