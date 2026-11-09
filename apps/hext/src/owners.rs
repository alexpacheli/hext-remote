//! The account bound to this computer (F5, marcos 5 and 6): its devices come
//! in without anyone accepting, as do those of the accounts it shares this
//! computer with. The decision is this computer's: it keeps the account's
//! chain, checks every signature itself and takes no older one than it saw
//! ([`hext_signal::chain`]). The server only hands the chains out.
//!
//! Bound by an administrator (`service account <chain>`), with the chain's
//! ID from the app of a device in the account: a chain the server made up
//! has another ID. The file (`owners.json`, in the computer's folder) is
//! theirs only to change. Or by a technician who got in with the unattended
//! password, which an administrator set ([`Owners::bind_asked`]): the ask
//! is signed by the technician's key, so the server cannot change the chain.

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
    /// Bound by a technician with the unattended password
    /// ([`Owners::bind_asked`]), not by an administrator: changing or
    /// turning off the password undoes it, as it does the technicians.
    #[serde(default)]
    by_password: bool,
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
    /// One fetch and write at a time.
    busy: tokio::sync::Mutex<()>,
    /// When the chains were last fetched.
    fetched: Mutex<Option<std::time::Instant>>,
}

impl Owners {
    /// The binding kept in `file`, for this computer's `key`.
    pub fn new(file: PathBuf, key: Arc<DeviceKey>, server: String) -> Self {
        let owners = Self(Arc::new(Inner {
            file,
            key,
            server,
            checked: Mutex::new(None),
            busy: tokio::sync::Mutex::new(()),
            fetched: Mutex::new(None),
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

    /// Bound, and this computer on the bound account's list (as last
    /// checked).
    pub fn listed(&self) -> bool {
        let checked = self.0.checked.lock().expect("lock");
        checked
            .as_ref()
            .is_some_and(|checked| checked.own.computer(&self.host()).is_some())
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
                "Vinculado a uma conta, mas este computador não está em Meus computadores dela: \
                 ninguém entra por ela."
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
        let _busy = self.0.busy.lock().await;
        let chains = self.fetch(&[id]).await?;
        let own = chains
            .iter()
            .find(|(chain, _)| chain.id == id)
            .context("o servidor não entregou a lista dessa conta")?;
        if own.0.computer(&self.host()).is_none() {
            bail!(
                "este computador não está em Meus computadores dessa conta: insira-o na lista pelo \
                 app antes"
            );
        }
        let stored = Stored {
            chain: id,
            entries: own.1.clone(),
            shared: shared_of(&own.0, &self.host(), &chains, &[]),
            by_password: false,
        };
        self.store(&stored)?;
        self.load();
        Ok(own.0.clone())
    }

    /// Binds this computer to chain `id` at the ask of `viewer`, who got in
    /// with the unattended password (an administrator set it: that is their
    /// consent, as for a technician who asks to be remembered). `viewer`
    /// must be a device of that chain. This computer need not be on its list
    /// yet (the technician's app puts it there right after), but nobody
    /// comes in by the account until it is. Bound to another account by an
    /// administrator: refused (they unbind first); one bound with the
    /// password is replaced (whoever has the password could do it anyway).
    /// Whether it bound now (`false`: it was bound to this one already).
    pub async fn bind_asked(&self, id: ChainId, viewer: &PublicKey) -> Result<bool> {
        let _busy = self.0.busy.lock().await;
        if let Some(stored) = self.stored() {
            if stored.chain == id {
                return Ok(false);
            }
            if !stored.by_password {
                bail!(
                    "este computador já está vinculado a outra conta por um administrador: ele \
                     desvincula antes (HextRemote service account --off)"
                );
            }
        }
        let chains = tokio::time::timeout(FETCH_TIME, self.fetch(&[id]))
            .await
            .context("servidor de sinalização: sem resposta")??;
        let own = chains
            .iter()
            .find(|(chain, _)| chain.id == id)
            .context("o servidor não entregou a lista dessa conta")?;
        if !own.0.is_device(viewer) {
            bail!("quem pediu não está na lista de dispositivos dessa conta");
        }
        let stored = Stored {
            chain: id,
            entries: own.1.clone(),
            shared: shared_of(&own.0, &self.host(), &chains, &[]),
            by_password: true,
        };
        self.store(&stored)?;
        self.load();
        Ok(true)
    }

    /// Unbinds if a technician bound it with the password (the password
    /// changes or goes off). Whether it did.
    pub fn unbind_if_by_password(&self) -> Result<bool> {
        if !self.stored().is_some_and(|stored| stored.by_password) {
            return Ok(false);
        }
        self.unbind()?;
        Ok(true)
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
        let _busy = self.0.busy.lock().await;
        *self.0.fetched.lock().expect("lock") = Some(std::time::Instant::now());
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
        let kept_shared = verified(&stored.shared);
        let shared = shared_of(own, &self.host(), &chains, &kept_shared);
        let fresh = Stored {
            chain: stored.chain,
            entries: entries.clone(),
            shared,
            by_password: stored.by_password,
        };
        // Unbound (or bound again) from the command line meanwhile: that
        // stands.
        if self.stored().is_none_or(|now| now.chain != stored.chain) {
            self.load();
            bail!("a conta vinculada mudou enquanto a lista era conferida");
        }
        if fresh.entries != stored.entries || fresh.shared != stored.shared {
            self.store(&fresh)?;
        }
        *self.0.checked.lock().expect("lock") = check(&fresh).ok();
        Ok(())
    }

    /// [`Self::refresh`], unless the chains were fetched less than `age`
    /// ago.
    pub async fn refresh_if_stale(&self, age: Duration) -> Result<()> {
        let fresh = self
            .0
            .fetched
            .lock()
            .expect("lock")
            .is_some_and(|at| at.elapsed() < age);
        if fresh { Ok(()) } else { self.refresh().await }
    }

    fn store(&self, stored: &Stored) -> Result<()> {
        let text = serde_json::to_string(stored)?;
        let next = self.0.file.with_extension("new");
        std::fs::write(&next, text).with_context(|| next.display().to_string())?;
        std::fs::rename(&next, &self.0.file).with_context(|| self.0.file.display().to_string())
    }

    /// The chains the server has for this computer (and `known`), each
    /// checked; the ones that do not hold are left out.
    async fn connect(&self) -> Result<Client> {
        let client = Client::connect(
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
        Ok(client)
    }

    /// Sends entries of the audit to the server, for the account's
    /// Atividade screen.
    pub(crate) async fn send_events(&self, events: &[String]) -> Result<()> {
        let mut client = self.connect().await?;
        let mut result = Ok(());
        for chunk in events.chunks(hext_signal::account::MAX_EVENTS) {
            let call = AccountCall::HostEvents {
                events: chunk.to_vec(),
                chain: self.bound().map(|(id, _)| id),
            };
            if let Err(e) = client.account(call).await {
                result = Err(e.into());
                break;
            }
        }
        let _ = client.close().await;
        result
    }

    async fn fetch(&self, known: &[ChainId]) -> Result<Vec<(Chain, Vec<Entry>)>> {
        let mut client = self.connect().await?;
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
        // The file as it is now: unbound from the command line, nobody stays.
        self.load();
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

/// Sends what the audit writes to the server, for the Atividade screen of
/// the account (marco 7), while this computer is bound to one: not
/// otherwise. A few at a time; if the server does not answer, they stay
/// only in the audit here, which is the record that counts.
pub async fn report_activity(
    owners: Owners,
    mut entries: tokio::sync::mpsc::UnboundedReceiver<String>,
) {
    while let Some(first) = entries.recv().await {
        // What comes together (a session's start and its first file) goes
        // together.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let mut batch = vec![first];
        while let Ok(more) = entries.try_recv() {
            if batch.len() < 200 {
                batch.push(more);
            }
        }
        if owners.bound().is_none() {
            continue;
        }
        match tokio::time::timeout(FETCH_TIME, owners.send_events(&batch)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!("atividade da conta: {e:#}"),
            Err(_) => warn!("atividade da conta: servidor sem resposta"),
        }
    }
}

/// The chains of the accounts `own` shares computer `host` with, one for
/// each, as found among `chains`: the longest version, if every other one
/// the server sent and the one `kept` are its start. Otherwise (a fork, an
/// older one) the one kept stays.
fn shared_of(
    own: &Chain,
    host: &PublicKey,
    chains: &[(Chain, Vec<Entry>)],
    kept: &[(Chain, Vec<Entry>)],
) -> Vec<Vec<Entry>> {
    let shares = own.shares_of(host, 0);
    let mut out = Vec::new();
    for id in shares.keys() {
        if *id == own.id {
            continue;
        }
        let versions: Vec<_> = chains.iter().filter(|(c, _)| c.id == *id).collect();
        let before = kept.iter().find(|(c, _)| c.id == *id);
        let chosen = versions
            .iter()
            .max_by_key(|(c, _)| c.len)
            .filter(|(c, entries)| {
                versions
                    .iter()
                    .all(|(other, _)| c.extends(entries, other.len, &other.head))
                    && before.is_none_or(|(k, _)| c.extends(entries, k.len, &k.head))
            });
        match (chosen, before) {
            (Some((_, entries)), _) => out.push(entries.clone()),
            (None, Some((_, entries))) => {
                if !versions.is_empty() {
                    warn!("lista compartilhada mais velha ou bifurcada: fica a guardada");
                }
                out.push(entries.clone());
            }
            (None, None) => {
                if !versions.is_empty() {
                    warn!("lista compartilhada em versões que não batem: recusada");
                }
            }
        }
    }
    out
}

/// The chains in `lists` that hold, with their entries.
fn verified(lists: &[Vec<Entry>]) -> Vec<(Chain, Vec<Entry>)> {
    lists
        .iter()
        .filter_map(|entries| Some((Chain::verify(entries).ok()?, entries.clone())))
        .collect()
}

fn check(stored: &Stored) -> Result<Checked> {
    let own = Chain::verify(&stored.entries)?;
    if own.id != stored.chain {
        bail!("a lista guardada não é a da conta vinculada");
    }
    // (One of each: a second one with the same ID is left out.)
    let mut shared: Vec<Chain> = Vec::new();
    for (chain, _) in verified(&stored.shared) {
        if !shared.iter().any(|c| c.id == chain.id) {
            shared.push(chain);
        }
    }
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

#[cfg(test)]
mod tests {
    use hext_signal::chain::{Allowed, Op};

    use super::*;

    fn key() -> DeviceKey {
        DeviceKey::generate().unwrap()
    }

    /// `entries` and one more, signed by `by`.
    fn push(entries: &[Entry], op: Op, by: &DeviceKey) -> (Chain, Vec<Entry>) {
        let mut entries = entries.to_vec();
        entries.push(Chain::verify(&entries).unwrap().sign(op, by));
        (Chain::verify(&entries).unwrap(), entries)
    }

    fn add(name: &str) -> Op {
        Op::AddDevice {
            device: key().public_key(),
            name: name.into(),
        }
    }

    #[test]
    fn a_shared_list_never_goes_back_nor_forks() {
        // The other account: its list, then one more device; and a fork of
        // the first one (signed by a device it had then).
        let theirs = key();
        let v1 = vec![Entry::create(&theirs, "Técnico", key().public_key())];
        let first = (Chain::verify(&v1).unwrap(), v1.clone());
        let second = push(&v1, add("Notebook"), &theirs);
        let fork = push(&v1, add("Intruso"), &theirs);

        // This account shares the computer with it.
        let mine = key();
        let host = key().public_key();
        let own = vec![Entry::create(&mine, "Dono", key().public_key())];
        let (_, own) = push(
            &own,
            Op::AddComputer {
                host,
                id: "123456789".parse().unwrap(),
                name: "Escritório".into(),
            },
            &mine,
        );
        let (own, _) = push(
            &own,
            Op::Share {
                host,
                with: first.0.id,
                name: "tecnico@example.com".into(),
                allowed: Allowed::default(),
                until: None,
            },
            &mine,
        );

        let one = |chains: &[(Chain, Vec<Entry>)], kept: &[(Chain, Vec<Entry>)]| {
            shared_of(&own, &host, chains, kept)
        };
        // Newer: taken.
        assert_eq!(
            one(std::slice::from_ref(&second), std::slice::from_ref(&first)),
            vec![second.1.clone()]
        );
        // Older, a fork, or missing: the one kept stays.
        assert_eq!(
            one(std::slice::from_ref(&first), std::slice::from_ref(&second)),
            vec![second.1.clone()]
        );
        assert_eq!(
            one(std::slice::from_ref(&fork), std::slice::from_ref(&second)),
            vec![second.1.clone()]
        );
        assert_eq!(
            one(&[], std::slice::from_ref(&second)),
            vec![second.1.clone()]
        );
        // Two versions that do not agree, nothing kept: neither.
        assert!(one(&[second.clone(), fork.clone()], &[]).is_empty());
        // Two that agree: the longer.
        assert_eq!(
            one(&[first.clone(), second.clone()], &[]),
            vec![second.1.clone()]
        );
        // A list it is not shared with: left out.
        let stranger = vec![Entry::create(&key(), "X", key().public_key())];
        let stranger = (Chain::verify(&stranger).unwrap(), stranger);
        assert!(one(&[stranger], &[]).is_empty());
    }
}
