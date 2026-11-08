//! The account bound to this computer (F5, marcos 5 and 6): its devices come
//! in without anyone accepting, as do those of the accounts it shares this
//! computer with. The decision is this computer's: it keeps the account's
//! chain, checks every signature itself and takes no older one than it saw
//! ([`hext_signal::chain`]). The server only hands the chains out.
//!
//! Bound by an administrator (`service account <chain>`), with the chain's
//! ID from the app of a device in the account: a chain the server made up
//! has another ID. The file (`owners.json`, in the computer's folder) is
//! theirs only to change.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use hext_crypto::{DeviceKey, PublicKey};
use hext_proto::Permissions;
use hext_signal::chain::{Allowed, Chain, ChainId, Entry};
use hext_signal::{AccountCall, AccountReply, Client, Role};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// How long a refresh may take before the computer goes by what it has.
const FETCH_TIME: Duration = Duration::from_secs(8);

/// What is kept: the bound chain and the shared accounts' chains, as
/// fetched (checked again on load).
#[derive(Serialize, Deserialize, Default)]
struct Stored {
    #[serde(with = "hex_id")]
    chain: ChainId,
    entries: Vec<Entry>,
    #[serde(default)]
    shared: Vec<Vec<Entry>>,
}

/// The chains, checked.
#[derive(Clone)]
struct Checked {
    own: Chain,
    shared: Vec<Chain>,
}

/// How someone comes in by the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Way {
    pub permissions: Permissions,
    /// As the audit says it.
    pub via: String,
}

/// The account bound here, if any. Cheap to clone (shared).
#[derive(Clone)]
pub struct Owners(Arc<Inner>);

struct Inner {
    file: PathBuf,
    key: Arc<DeviceKey>,
    server: String,
    /// What the file says, checked (`None`: not bound, or it does not hold).
    checked: Mutex<Option<Checked>>,
}

impl Owners {
    /// The binding kept in `file`, for this computer's `key`.
    pub fn new(file: PathBuf, key: Arc<DeviceKey>, server: String) -> Self {
        let owners = Self(Arc::new(Inner {
            file,
            key,
            server,
            checked: Mutex::new(None),
        }));
        owners.load();
        owners
    }

    fn host(&self) -> PublicKey {
        self.0.key.public_key()
    }

    fn stored(&self) -> Option<Stored> {
        let text = std::fs::read_to_string(&self.0.file).ok()?;
        serde_json::from_str(&text)
            .inspect_err(|e| warn!("{}: {e}", self.0.file.display()))
            .ok()
    }

    /// Reads the file again (it changes from the command line).
    pub fn load(&self) {
        let checked = self.stored().and_then(|stored| check(&stored).ok());
        *self.0.checked.lock().expect("lock") = checked;
    }

    /// The bound chain's ID and the account's name for this computer.
    pub fn bound(&self) -> Option<(ChainId, Option<String>)> {
        let checked = self.0.checked.lock().expect("lock");
        let checked = checked.as_ref()?;
        let name = checked.own.computer(&self.host()).map(|c| c.name.clone());
        Some((checked.own.id, name))
    }

    /// What the home screen says of the binding (`None`: not bound).
    pub fn summary(&self) -> Option<String> {
        let checked = self.0.checked.lock().expect("lock").clone();
        let Some(checked) = checked else {
            return self.stored().map(|_| {
                "Vinculado a uma conta, mas a lista dela não confere: ninguém entra por ela agora."
                    .to_owned()
            });
        };
        let host = self.host();
        if checked.own.computer(&host).is_none() {
            return Some(
                "Vinculado a uma conta que tirou este computador da lista: ninguém entra por ela."
                    .to_owned(),
            );
        }
        let devices = checked.own.devices().len();
        let mut text = format!("Os {devices} dispositivo(s) da conta vinculada entram sem aceite");
        let shares = checked.own.shares_of(&host, unix_now());
        if !shares.is_empty() {
            let names: Vec<_> = shares.values().map(|s| s.name.as_str()).collect();
            text.push_str(&format!(", e por compartilhamento: {}", names.join(", ")));
        }
        text.push('.');
        Some(text)
    }

    /// How `viewer` comes in by the account now (`None`: it does not).
    pub fn way(&self, viewer: &PublicKey) -> Option<Way> {
        let checked = self.0.checked.lock().expect("lock").clone()?;
        let host = self.host();
        // Out of the account's list, nobody comes in by it.
        checked.own.computer(&host)?;
        if checked.own.is_device(viewer) {
            return Some(Way {
                permissions: owner_permissions(),
                via: "entrou como dono".to_owned(),
            });
        }
        let now = unix_now();
        let shares = checked.own.shares_of(&host, now);
        checked.shared.iter().find_map(|other| {
            let share = shares.get(&other.id)?;
            other.is_device(viewer).then(|| Way {
                permissions: permissions(share.allowed),
                via: format!("por compartilhamento com {}", share.name),
            })
        })
    }

    /// Binds this computer to chain `id` (from the app of a device in it):
    /// fetched, checked, and with this computer on its list.
    pub async fn bind(&self, id: ChainId) -> Result<Chain> {
        let chains = self.fetch(&[id]).await?;
        let own = chains
            .iter()
            .find(|(chain, _)| chain.id == id)
            .context("o servidor não entregou a lista dessa conta")?;
        if own.0.computer(&self.host()).is_none() {
            bail!(
                "este computador não está em Meus computadores dessa conta: ponha-o na lista pelo \
                 app antes"
            );
        }
        let stored = Stored {
            chain: id,
            entries: own.1.clone(),
            shared: shared_of(&own.0, &self.host(), &chains),
        };
        self.store(&stored)?;
        self.load();
        Ok(own.0.clone())
    }

    /// Unbinds: nobody comes in by the account any more.
    pub fn unbind(&self) -> Result<()> {
        match std::fs::remove_file(&self.0.file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).context(self.0.file.display().to_string()),
        }
        *self.0.checked.lock().expect("lock") = None;
        Ok(())
    }

    /// Fetches the chains again: a device out of the account no longer comes
    /// in. A chain older than the one kept, or none, is refused: the one kept
    /// stays, but the server is not believed (logged).
    pub async fn refresh(&self) -> Result<()> {
        let Some(stored) = self.stored() else {
            return Ok(());
        };
        let chains = tokio::time::timeout(FETCH_TIME, self.fetch(&[stored.chain]))
            .await
            .context("servidor de sinalização: sem resposta")??;
        let Some((own, entries)) = chains.iter().find(|(c, _)| c.id == stored.chain) else {
            // Hidden, or the account is gone: nobody comes in by it until the
            // list is back.
            *self.0.checked.lock().expect("lock") = None;
            bail!("o servidor não entregou a lista da conta vinculada");
        };
        let kept = check(&stored).ok();
        if let Some(kept) = &kept
            && !own.extends(entries, kept.own.len, &kept.own.head)
        {
            bail!("a lista da conta veio mais velha que a guardada: ignorada");
        }
        let shared = shared_of(own, &self.host(), &chains);
        let fresh = Stored {
            chain: stored.chain,
            entries: entries.clone(),
            shared,
        };
        if fresh.entries != stored.entries || fresh.shared != stored.shared {
            self.store(&fresh)?;
        }
        *self.0.checked.lock().expect("lock") = check(&fresh).ok();
        Ok(())
    }

    fn store(&self, stored: &Stored) -> Result<()> {
        let text = serde_json::to_string(stored)?;
        std::fs::write(&self.0.file, text).with_context(|| self.0.file.display().to_string())
    }

    /// The chains the server has for this computer (and `known`), each
    /// checked; the ones that do not hold are left out.
    async fn fetch(&self, known: &[ChainId]) -> Result<Vec<(Chain, Vec<Entry>)>> {
        let mut client = Client::connect(
            &self.0.server,
            &self.0.key,
            &crate::device_name(),
            crate::APP_VERSION,
            Role::Viewer,
        )
        .await
        .context("servidor de sinalização")?;
        if !client.takes_accounts() {
            let _ = client.close().await;
            bail!("o servidor ainda não tem contas");
        }
        let reply = client
            .account(AccountCall::ChainHost {
                known: known.to_vec(),
            })
            .await;
        let _ = client.close().await;
        let AccountReply::HostChains { chains } = reply? else {
            bail!("resposta inesperada do servidor");
        };
        let checked = tokio::task::spawn_blocking(move || {
            chains
                .into_iter()
                .filter_map(|entries| match Chain::verify(&entries) {
                    Ok(chain) => Some((chain, entries)),
                    Err(e) => {
                        warn!("lista de conta recusada: {e}");
                        None
                    }
                })
                .collect()
        })
        .await?;
        Ok(checked)
    }
}

impl hext_session::host::Owners for Owners {
    fn still(&self, viewer: &PublicKey) -> bool {
        self.way(viewer).is_some()
    }
}

/// Keeps the binding fresh: every `every`, the chains again; `show` gets
/// what the home screen says of it.
pub async fn keep_fresh(owners: Owners, every: Duration, show: impl Fn(Option<String>)) {
    let mut tick = tokio::time::interval(every);
    loop {
        tick.tick().await;
        owners.load();
        if owners.bound().is_none() && owners.stored().is_none() {
            show(None);
            continue;
        }
        if let Err(e) = owners.refresh().await {
            warn!("conta vinculada: {e:#}");
        } else {
            info!("conta vinculada: lista conferida");
        }
        show(owners.summary());
    }
}

/// The chains of the accounts `own` shares computer `host` with, as found
/// among `chains`.
fn shared_of(own: &Chain, host: &PublicKey, chains: &[(Chain, Vec<Entry>)]) -> Vec<Vec<Entry>> {
    let shares = own.shares_of(host, 0);
    chains
        .iter()
        .filter(|(chain, _)| chain.id != own.id && shares.contains_key(&chain.id))
        .map(|(_, entries)| entries.clone())
        .collect()
}

fn check(stored: &Stored) -> Result<Checked> {
    let own = Chain::verify(&stored.entries)?;
    if own.id != stored.chain {
        bail!("a lista guardada não é a da conta vinculada");
    }
    let shared = stored
        .shared
        .iter()
        .filter_map(|entries| Chain::verify(entries).ok())
        .collect();
    Ok(Checked { own, shared })
}

/// What the account's own devices may do: all a technician needs.
fn owner_permissions() -> Permissions {
    Permissions {
        control: true,
        clipboard: true,
        files: true,
        restart: true,
    }
}

fn permissions(allowed: Allowed) -> Permissions {
    Permissions {
        control: allowed.control,
        clipboard: allowed.clipboard,
        files: allowed.files,
        restart: allowed.restart,
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A chain's ID as typed (`service account <id>`): 64 hex digits.
pub fn parse_id(text: &str) -> Result<ChainId> {
    let mut id = [0; 32];
    hex::decode_to_slice(text.trim(), &mut id).context("ID da lista inválido")?;
    Ok(id)
}

mod hex_id {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(d)?;
        let mut out = [0; 32];
        hex::decode_to_slice(text, &mut out).map_err(D::Error::custom)?;
        Ok(out)
    }
}
