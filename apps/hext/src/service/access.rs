//! The unattended access, kept in one folder: the password, the technicians
//! trusted without it and the account binding. The Windows service keeps it
//! in the computer's folder (only the system and the administrators write
//! there); the Mac app opened at login, in its user's folder (F6 marco 3:
//! after the user signs in, not at the sign-in screen).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use hext_crypto::pake::Verifier;
use hext_crypto::{DeviceKey, PublicKey};
use hext_session::host::UnattendedPassword;
use tracing::warn;

use crate::owners::{Owners, Way};
use crate::ui_link::UiHub;

/// The shortest password taken: guesses are limited, but it may be all that
/// stands between the internet and this computer.
pub const MIN_PASSWORD: usize = 10;

/// How often the interface's list of technicians, and whether the password
/// is on, are checked against the files (the command line changes them).
const FILE_CHECK: Duration = Duration::from_secs(3);

/// How often the bound account's list is fetched again.
pub const OWNERS_CHECK: Duration = Duration::from_secs(60);

/// The folder of the unattended access, and where its audit lines go.
#[derive(Clone)]
pub struct Access {
    dir: PathBuf,
    audit: fn(&str),
}

impl Access {
    pub fn new(dir: PathBuf, audit: fn(&str)) -> Self {
        Self { dir, audit }
    }

    /// What is kept of the password; no file: the access is off.
    pub fn password_file(&self) -> PathBuf {
        self.dir.join("unattended.key")
    }

    /// The technicians trusted without the password (F4 marco 12).
    pub fn technicians(&self) -> crate::technicians::Technicians {
        crate::technicians::Technicians::new(self.dir.join("technicians.json"))
    }

    /// This computer's binding to an account, as `key` (this computer's)
    /// sees it.
    pub fn owners(&self, key: Arc<DeviceKey>) -> Owners {
        Owners::new(
            self.dir.join("owners.json"),
            key,
            crate::DEFAULT_SERVER.to_owned(),
        )
    }

    pub fn audit(&self, entry: &str) {
        (self.audit)(entry);
    }

    /// The password, if it is on.
    pub fn verifier(&self) -> Option<Verifier> {
        Verifier::load(&self.password_file()).unwrap_or_else(|e| {
            warn!("{e}");
            None
        })
    }

    /// Sets (or changes) the password: whoever came in under the old one
    /// starts from nobody.
    pub fn set_password(&self, password: &str, owners: &Owners) -> Result<()> {
        if password.chars().count() < MIN_PASSWORD {
            bail!("a senha precisa de ao menos {MIN_PASSWORD} caracteres");
        }
        // (Before the new one: if anything fails, nobody keeps a way in.)
        self.revoke_bound(owners)?;
        Verifier::new(password)?.save(&self.password_file())?;
        self.audit("senha do acesso não assistido definida");
        Ok(())
    }

    /// Turns the password off, with whoever came in under it.
    pub fn password_off(&self, owners: &Owners) -> Result<()> {
        self.revoke_bound(owners)?;
        Verifier::remove(&self.password_file())?;
        self.audit("acesso não assistido desligado");
        Ok(())
    }

    /// The technicians bound under the old password, and an account bound
    /// with it: a new one (or none) starts from nobody.
    fn revoke_bound(&self, owners: &Owners) -> Result<()> {
        let technicians = self.technicians();
        let had = technicians.list().len();
        technicians.clear()?;
        if had > 0 {
            self.audit(&format!(
                "técnicos vinculados revogados com a troca da senha: {had}"
            ));
            println!("Os técnicos vinculados ({had}) voltam a precisar da senha.");
        }
        if owners.unbind_if_by_password()? {
            self.audit("conta vinculada com a senha desvinculada com a troca da senha");
            println!("A conta vinculada por um técnico com a senha foi desvinculada.");
        }
        Ok(())
    }

    /// `service technicians`: lists the technicians trusted without the
    /// password, or revokes them.
    pub fn technicians_command(&self, revoke: Option<&str>, revoke_all: bool) -> Result<()> {
        let store = self.technicians();
        if revoke_all {
            let list = store.list();
            store.clear()?;
            self.audit(&format!(
                "todos os técnicos vinculados revogados: {}",
                list.len()
            ));
            println!(
                "Técnicos revogados: {}. Voltam a precisar da senha.",
                list.len()
            );
            return Ok(());
        }
        if let Some(which) = revoke {
            let gone = store.revoke(which)?;
            if gone.is_empty() {
                bail!("nenhum técnico vinculado com o ID ou a chave {which}");
            }
            for t in &gone {
                self.audit(&format!(
                    "técnico desvinculado: \"{}\" (ID {})",
                    t.name, t.id
                ));
                println!(
                    "Revogado: {} (ID {}). Volta a precisar da senha.",
                    t.name, t.id
                );
            }
            return Ok(());
        }
        let list = store.list();
        if list.is_empty() {
            println!("Nenhum técnico vinculado: todos precisam da senha.");
            return Ok(());
        }
        println!("Técnicos que entram sem a senha:");
        for t in list {
            println!(
                "  {} (ID {}), desde {}",
                t.name,
                t.id,
                crate::technicians::date(t.since)
            );
        }
        println!("Para revogar: HextRemote service technicians --revoke <ID>");
        Ok(())
    }

    /// `service account` with no change: the binding, as it stands.
    pub fn show_account(owners: &Owners) {
        match owners.bound() {
            Some((id, name)) => println!(
                "Vinculado à conta da lista {}{}: os dispositivos dela entram sem aceite.",
                hex::encode(id),
                name.map(|n| format!(", como \"{n}\"")).unwrap_or_default()
            ),
            None => println!("Nenhuma conta vinculada: todo acesso pede aceite."),
        }
    }

    /// Unbinds the account: every access asks again.
    pub fn unbind_account(&self, owners: &Owners) -> Result<()> {
        owners.unbind()?;
        self.audit("conta desvinculada: os acessos voltam a pedir aceite");
        Ok(())
    }

    /// Binds the account with list `chain` (hex), as an owner of this
    /// computer asked.
    pub fn bind_account(&self, owners: &Owners, chain: &str) -> Result<()> {
        let id = crate::owners::parse_id(chain)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let bound = runtime.block_on(owners.bind(id))?;
        let devices = bound.devices().len();
        self.audit(&format!(
            "conta vinculada (lista {}): {devices} dispositivo(s) entram sem aceite",
            hex::encode(id)
        ));
        println!(
            "Vinculado. Os {devices} dispositivo(s) da conta entram sem aceite; quem sair da conta \
             deixa de entrar (e a sessão dele cai)."
        );
        Ok(())
    }

    /// Keeps the interface (`ui`) up to date: the technicians, whether the
    /// password is on and the bound account, as the files and the account
    /// change.
    pub fn show_on(&self, ui: &UiHub, owners: &Owners) {
        ui.set_technicians(self.technicians(), self.audit);
        let (access, watched) = (self.clone(), ui.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(FILE_CHECK);
            loop {
                tick.tick().await;
                watched.technicians_changed();
                watched.set_password(access.verifier().is_some());
            }
        });
        let shown = ui.clone();
        tokio::spawn(crate::owners::keep_fresh(
            owners.clone(),
            OWNERS_CHECK,
            move |text| shown.set_owners(text),
        ));
    }
}

/// How the device with `key` comes in by the bound account; `None`: it
/// does not (or nothing is bound).
pub async fn owner_way(owners: &Owners, key: &PublicKey) -> Option<Way> {
    // The file changes from the command line; the list from the account
    // (one just removed no longer comes in).
    owners.load();
    owners.bound()?;
    // One of the account: checked against the latest list. Anyone else: at
    // most every half minute (a device that just joined), or strangers would
    // hold up every request. Bound but not yet on the account's list (a
    // technician just bound it with the password, and their app is inserting
    // it): every time, or the technician's next click would wait for the
    // half minute.
    let checked = if owners.way(key).is_some() || !owners.listed() {
        owners.refresh().await
    } else {
        owners.refresh_if_stale(Duration::from_secs(30)).await
    };
    if let Err(e) = checked {
        warn!("conta vinculada: {e:#}");
    }
    owners.way(key)
}

/// The password as the host sees it, the technicians it lets in without it
/// (shown on `ui`), and the account binding a technician with the password
/// may ask for (`owners`).
pub struct Password {
    pub access: Access,
    pub ui: UiHub,
    pub owners: Option<Owners>,
}

impl UnattendedPassword for Password {
    fn verifier(&self) -> Option<Verifier> {
        self.access.verifier()
    }

    fn audit(&self, entry: &str) {
        self.access.audit(entry);
    }

    fn bind(&self, viewer: &PublicKey, grant: &hext_session::Grant) -> bool {
        match self
            .access
            .technicians()
            .bind(viewer, &grant.id, &grant.name)
        {
            Ok(()) => {
                self.ui.technicians_changed();
                true
            }
            Err(e) => {
                warn!("técnico vinculado: {e:#}");
                self.audit(&format!("não foi possível vincular o técnico: {e:#}"));
                false
            }
        }
    }

    fn bind_account(&self, viewer: &PublicKey, chain: [u8; 32], grant: &hext_session::Grant) {
        let Some(owners) = self.owners.clone() else {
            return;
        };
        let (viewer, ui, access) = (*viewer, self.ui.clone(), self.access.clone());
        let who = format!("{} ({})", grant.name, grant.id);
        tokio::spawn(async move {
            match owners.bind_asked(chain, &viewer).await {
                Ok(true) => access.audit(&format!(
                    "conta vinculada (lista {}) a pedido de {who}, que entrou com a senha: os \
                     dispositivos dela entram sem aceite, com o computador em Meus computadores",
                    hex::encode(chain)
                )),
                Ok(false) => {}
                Err(e) => {
                    warn!("vínculo à conta: {e:#}");
                    access.audit(&format!("vínculo à conta pedido por {who} recusado: {e:#}"));
                }
            }
            ui.set_owners(owners.summary());
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet(_: &str) {}

    /// An access of its own for test `name`, nothing set.
    fn access(name: &str) -> (Access, Owners) {
        let dir = std::env::temp_dir().join(format!("hext-access-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let access = Access::new(dir, quiet);
        let owners = access.owners(Arc::new(DeviceKey::generate().unwrap()));
        (access, owners)
    }

    #[test]
    fn a_new_password_starts_from_nobody_and_off_takes_it_away() {
        let (access, owners) = access("password");
        assert!(access.verifier().is_none(), "off until someone sets it");
        assert!(
            access.set_password("curta", &owners).is_err(),
            "too short a password is refused"
        );
        assert!(access.verifier().is_none());

        access.set_password("uma senha bem longa", &owners).unwrap();
        assert!(access.verifier().is_some());
        let technician = PublicKey::from_bytes([7; 32]);
        access
            .technicians()
            .bind(&technician, "482913557", "Técnico")
            .unwrap();
        assert!(access.technicians().bound(&technician));

        // Changed: the technician bound under the old one needs the new one.
        access
            .set_password("outra senha bem longa", &owners)
            .unwrap();
        assert!(!access.technicians().bound(&technician));
        assert!(access.verifier().is_some());

        access
            .technicians()
            .bind(&technician, "482913557", "Técnico")
            .unwrap();
        access.password_off(&owners).unwrap();
        assert!(access.verifier().is_none());
        assert!(!access.technicians().bound(&technician));
    }
}
