//! The account on this computer (F5, marco 2): optional. The calls go on a
//! signaling connection of the viewer's key, so the login is this
//! computer's technician's: the server ties it to that key, and the token
//! kept here is worth nothing on another computer.
//!
//! The token stays in the user's own folder (`account.json`), never in the
//! pages: they get the answers without it.

use std::path::PathBuf;

use hext_crypto::{DeviceKey, PublicKey};
use hext_signal::chain::{self, Chain, Entry, Op};
use hext_signal::{AccountCall, AccountError, AccountReply, AccountView, Client, Join};
use serde::{Deserialize, Serialize};
use tracing::warn;

const FILE: &str = "account.json";
/// The latest chain seen: a server handing an older one is caught.
const CHAIN_FILE: &str = "chain.json";

/// The chain this computer last saw (of the account it is logged in to).
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
struct Known {
    id: String,
    len: u64,
    head: String,
}

/// The account's devices, as the home screen shows them.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DevicesView {
    /// `none`: the account has no list yet; `outside`: this computer is not
    /// on it; `member`: it is.
    pub state: &'static str,
    pub devices: Vec<DeviceItem>,
    pub computers: Vec<ComputerItem>,
    /// This computer is on the list as a computer.
    pub listed: bool,
    /// The list's ID (hex; empty with no list), for binding a computer.
    pub chain: String,
    /// This computer can be bound to the account from here (the service
    /// is installed).
    pub bindable: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ComputerItem {
    pub host: String,
    pub id: String,
    pub name: String,
    /// This computer.
    pub me: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DeviceItem {
    pub key: String,
    pub name: String,
    /// This computer.
    pub me: bool,
}

/// The login kept between runs.
#[derive(Serialize, Deserialize)]
struct Stored {
    email: String,
    token: String,
}

/// Where the login is kept, next to the viewer's key.
pub struct Account {
    file: PathBuf,
    key: PathBuf,
    server: String,
}

impl Account {
    /// The account of the viewer's key `key` (its folder keeps the login).
    pub fn new(key: PathBuf) -> Self {
        let file = key.with_file_name(FILE);
        Self {
            file,
            key,
            server: crate::DEFAULT_SERVER.to_owned(),
        }
    }

    fn stored(&self) -> Option<Stored> {
        let text = std::fs::read_to_string(&self.file).ok()?;
        serde_json::from_str(&text)
            .inspect_err(|e| warn!("{}: {e}", self.file.display()))
            .ok()
    }

    fn store(&self, stored: &Stored) {
        let written = serde_json::to_string(stored)
            .map_err(anyhow::Error::from)
            .and_then(|text| {
                if let Some(dir) = self.file.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&self.file, text)?;
                Ok(())
            });
        if let Err(e) = written {
            warn!("{}: {e:#}", self.file.display());
        }
    }

    fn forget(&self) {
        let _ = std::fs::remove_file(&self.file);
        let _ = std::fs::remove_file(self.file.with_file_name(CHAIN_FILE));
    }

    fn known(&self) -> Option<Known> {
        let text = std::fs::read_to_string(self.file.with_file_name(CHAIN_FILE)).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn set_known(&self, chain: &Chain) {
        let known = Known {
            id: hex::encode(chain.id),
            len: chain.len,
            head: hex::encode(chain.head),
        };
        if self.known().as_ref() == Some(&known) {
            return;
        }
        let text = serde_json::to_string(&known).expect("serializes");
        if let Err(e) = std::fs::write(self.file.with_file_name(CHAIN_FILE), text) {
            warn!("{CHAIN_FILE}: {e}");
        }
    }

    fn key(&self) -> Result<DeviceKey, String> {
        crate::device_key(Some(&self.key)).map_err(|e| format!("{e:#}"))
    }

    /// One connection for one operation (the server limits connections per
    /// address).
    fn link(&self) -> Link<'_> {
        Link {
            account: self,
            client: None,
            resumed: false,
        }
    }

    /// The account's chain, every signature checked, and no older than the
    /// one seen before; `None`: no chain yet.
    async fn chain(&self, link: &mut Link<'_>) -> Result<Option<(Vec<Entry>, Chain)>, String> {
        let entries = match link.call(AccountCall::ChainGet).await? {
            AccountReply::Chain { entries } => entries,
            other => return Err(answer_error(&other)),
        };
        if entries.is_empty() {
            return match self.known() {
                // It had one: the server lost it, or hides it.
                Some(_) => Err(TAMPERED.to_owned()),
                None => Ok(None),
            };
        }
        let checked = entries.clone();
        let chain = tokio::task::spawn_blocking(move || Chain::verify(&checked))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("{TAMPERED} ({e})"))?;
        if let Some(known) = self.known() {
            let head = hex::decode(&known.head)
                .ok()
                .and_then(|h| h.try_into().ok());
            let same_chain = known.id == hex::encode(chain.id);
            let goes_on = head.is_some_and(|head| chain.extends(&entries, known.len, &head));
            if !same_chain || !goes_on {
                return Err(TAMPERED.to_owned());
            }
        }
        self.set_known(&chain);
        Ok(Some((entries, chain)))
    }

    /// The devices and computers of the account, for the home screen.
    /// `host`: this computer's host key, if known.
    pub async fn devices(&self, host: Option<&str>) -> Result<DevicesView, String> {
        let me = self.key()?.public_key();
        let mut link = self.link();
        let found = self.chain(&mut link).await;
        link.close().await;
        let Some((_, chain)) = found? else {
            return Ok(DevicesView {
                state: "none",
                devices: Vec::new(),
                computers: Vec::new(),
                listed: false,
                chain: String::new(),
                bindable: false,
            });
        };
        let devices = chain
            .devices()
            .iter()
            .map(|d| DeviceItem {
                key: d.key.to_string(),
                name: d.name.clone(),
                me: d.key == me,
            })
            .collect();
        let state = if chain.is_device(&me) {
            "member"
        } else {
            "outside"
        };
        let computers: Vec<ComputerItem> = chain
            .computers()
            .iter()
            .map(|c| ComputerItem {
                host: c.host.to_string(),
                id: c.id.to_string(),
                name: c.name.clone(),
                me: Some(c.host.to_string().as_str()) == host,
            })
            .collect();
        let listed = computers.iter().any(|c| c.me);
        Ok(DevicesView {
            state,
            devices,
            computers,
            listed,
            chain: hex::encode(chain.id),
            bindable: false,
        })
    }

    /// Puts computer `host` (its host key), with ID `id`, in the list.
    pub async fn add_computer(&self, host: &str, id: &str, name: &str) -> Result<(), String> {
        let host: PublicKey = host.parse().map_err(|_| "chave inválida".to_owned())?;
        let id = id.parse().map_err(|e| format!("{e}"))?;
        let name: String = name.chars().take(chain::MAX_NAME).collect();
        self.sign(Op::AddComputer { host, id, name })
            .await
            .map(drop)
    }

    /// Takes computer `host` out of the list (and its shares with it).
    pub async fn remove_computer(&self, host: &str) -> Result<(), String> {
        let host: PublicKey = host.parse().map_err(|_| "chave inválida".to_owned())?;
        self.sign(Op::RemoveComputer { host }).await.map(drop)
    }

    /// Adds `op` at the end of the chain, signed by `signer`. Once more if
    /// another device added one meanwhile.
    async fn append(
        &self,
        link: &mut Link<'_>,
        op: Op,
        signer: &DeviceKey,
    ) -> Result<Chain, String> {
        for _ in 0..2 {
            let Some((_, chain)) = self.chain(link).await? else {
                return Err("A conta ainda não tem a lista de dispositivos.".into());
            };
            let entry = chain.sign(op.clone(), signer);
            let mut after = chain.clone();
            after
                .apply(&entry)
                .map_err(|e| format!("Não dá para fazer isso: {e}"))?;
            match link.call(AccountCall::ChainAppend { entry }).await {
                Ok(_) => {
                    self.set_known(&after);
                    return Ok(after);
                }
                Err(e) if e == message(AccountError::ChainConflict) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(message(AccountError::ChainConflict).to_owned())
    }

    /// Adds `op`, signed by this computer, on a connection of its own.
    pub async fn sign(&self, op: Op) -> Result<Chain, String> {
        let key = self.key()?;
        let mut link = self.link();
        let done = self.append(&mut link, op, &key).await;
        link.close().await;
        done
    }

    /// Starts the account's chain with this computer, and gives back the
    /// recovery code (shown once).
    pub async fn create_chain(&self) -> Result<String, String> {
        let key = self.key()?;
        let mut link = self.link();
        let done = async {
            if self.chain(&mut link).await?.is_some() {
                return Err("A conta já tem a lista de dispositivos.".to_owned());
            }
            let code = chain::new_code(chain::RECOVERY_CODE_LEN);
            let recovery = chain::recovery_key(&code).public_key();
            let entry = Entry::create(&key, &device_name(), recovery);
            let created = Chain::verify(std::slice::from_ref(&entry)).map_err(|e| e.to_string())?;
            link.call(AccountCall::ChainAppend { entry }).await?;
            self.set_known(&created);
            Ok(code)
        }
        .await;
        link.close().await;
        done
    }

    /// Asks to join the chain, with the code shown on a device in it.
    pub async fn join(&self, code: &str) -> Result<(), String> {
        let device = self.key()?.public_key();
        let mut link = self.link();
        let done = async {
            let Some((_, chain)) = self.chain(&mut link).await? else {
                return Err("A conta ainda não tem a lista de dispositivos.".to_owned());
            };
            if chain.is_device(&device) {
                return Ok(());
            }
            let name = device_name();
            let proof = chain::join_proof(code, &chain.id, &device, &name);
            link.call(AccountCall::ChainJoin {
                device,
                name,
                proof,
            })
            .await
            .map(drop)
        }
        .await;
        link.close().await;
        done
    }

    /// Joins the chain with the recovery code (no other device at hand).
    pub async fn recover(&self, code: &str) -> Result<(), String> {
        let recovery = chain::recovery_key(code);
        let device = self.key()?.public_key();
        let mut link = self.link();
        let done = async {
            let Some((_, chain)) = self.chain(&mut link).await? else {
                return Err("A conta ainda não tem a lista de dispositivos.".to_owned());
            };
            if recovery.public_key() != chain.recovery {
                return Err("Chave de recuperação errada.".to_owned());
            }
            let op = Op::AddDevice {
                device,
                name: device_name(),
            };
            self.append(&mut link, op, &recovery).await.map(drop)
        }
        .await;
        link.close().await;
        done
    }

    /// Lets in the device that asked to join with `code`, if one did; its
    /// name.
    pub async fn approve(&self, code: &str) -> Result<Option<String>, String> {
        let key = self.key()?;
        let mut link = self.link();
        let done = async {
            let Some((_, chain)) = self.chain(&mut link).await? else {
                return Ok(None);
            };
            let joins = match link.call(AccountCall::ChainPending).await? {
                AccountReply::Joins { joins } => joins,
                other => return Err(answer_error(&other)),
            };
            let proven = |j: &&Join| {
                !chain.is_device(&j.device)
                    && chain::join_proof_ok(code, &chain.id, &j.device, &j.name, &j.proof)
            };
            let Some(join) = joins.iter().find(proven) else {
                return Ok(None);
            };
            let op = Op::AddDevice {
                device: join.device,
                name: join.name.clone(),
            };
            self.append(&mut link, op, &key).await?;
            Ok(Some(join.name.clone()))
        }
        .await;
        link.close().await;
        done
    }

    /// Takes device `key` out of the account.
    pub async fn remove(&self, key: &str) -> Result<(), String> {
        let device: PublicKey = key.parse().map_err(|_| "dispositivo inválido".to_owned())?;
        self.sign(Op::RemoveDevice { device }).await.map(drop)
    }

    /// Whether a login is kept (it may have ended on the server).
    pub fn kept(&self) -> bool {
        self.stored().is_some()
    }

    /// Makes `call` on a connection of its own (see [`Link::call`]).
    pub async fn call(&self, call: AccountCall) -> Result<AccountReply, String> {
        let mut link = self.link();
        let reply = link.call(call).await;
        link.close().await;
        reply
    }

    /// The account logged in here, asked of the server; `None` if there is
    /// none (or it ended there).
    pub async fn current(&self) -> Option<AccountView> {
        if !self.kept() {
            return None;
        }
        match self.call(AccountCall::Me).await {
            Ok(AccountReply::Account(view)) => Some(view),
            _ => None,
        }
    }

    async fn connect(&self) -> anyhow::Result<Client> {
        let key = crate::device_key(Some(&self.key))?;
        let client = crate::signaling::login(&self.server, &key, &crate::device_name()).await?;
        if !client.takes_accounts() {
            let _ = client.close().await;
            anyhow::bail!("o servidor ainda não tem contas");
        }
        Ok(client)
    }
}

/// A connection for the calls of one operation, opened at the first and
/// logged in (with the kept login) once.
pub struct Link<'a> {
    account: &'a Account,
    client: Option<Client>,
    resumed: bool,
}

impl Link<'_> {
    /// Makes `call`, logged in first with the kept login when the call needs
    /// one. A token in the answer stays here: the answer goes without it.
    /// Errors come as the text to show.
    pub async fn call(&mut self, call: AccountCall) -> Result<AccountReply, String> {
        let account = self.account;
        let needs_login = matches!(
            call,
            AccountCall::Logout
                | AccountCall::Me
                | AccountCall::ChainGet
                | AccountCall::ChainAppend { .. }
                | AccountCall::ChainJoin { .. }
                | AccountCall::ChainPending
                | AccountCall::TotpSetup
                | AccountCall::TotpEnable { .. }
                | AccountCall::ChangePassword { .. }
        );
        let logout = matches!(call, AccountCall::Logout);
        if self.client.is_none() {
            self.client = Some(account.connect().await.map_err(|e| format!("{e:#}"))?);
        }
        let client = self.client.as_mut().expect("just connected");
        if needs_login && !self.resumed {
            let Some(stored) = account.stored() else {
                return if logout {
                    Ok(AccountReply::Ok)
                } else {
                    Err(message(AccountError::NotLoggedIn).to_owned())
                };
            };
            let token = stored.token;
            match client.account(AccountCall::Resume { token }).await {
                Ok(AccountReply::Account(_)) => self.resumed = true,
                Ok(AccountReply::Error {
                    error: AccountError::NotLoggedIn,
                }) => {
                    account.forget();
                    return if logout {
                        Ok(AccountReply::Ok)
                    } else {
                        Err(message(AccountError::NotLoggedIn).to_owned())
                    };
                }
                Ok(other) => return Err(answer_error(&other)),
                Err(e) => return Err(format!("servidor de sinalização: {e}")),
            }
        }
        let reply = client
            .account(call)
            .await
            .map_err(|e| format!("servidor de sinalização: {e}"))?;
        match reply {
            AccountReply::LoggedIn {
                token,
                account: view,
            } => {
                account.store(&Stored {
                    email: view.email.clone(),
                    token,
                });
                self.resumed = true;
                Ok(AccountReply::LoggedIn {
                    token: String::new(),
                    account: view,
                })
            }
            AccountReply::Ok if logout => {
                account.forget();
                self.resumed = false;
                Ok(AccountReply::Ok)
            }
            AccountReply::Error { error } => {
                if error == AccountError::NotLoggedIn {
                    account.forget();
                    self.resumed = false;
                }
                Err(message(error).to_owned())
            }
            other => Ok(other),
        }
    }

    pub async fn close(self) {
        if let Some(client) = self.client {
            let _ = client.close().await;
        }
    }
}

/// When the server hands a chain that does not hold.
const TAMPERED: &str = "A lista de dispositivos que veio do servidor não confere com a que este \
                        computador conhece. Nada foi mudado; avise o suporte.";

/// This computer's name, as the list shows it.
fn device_name() -> String {
    let name = crate::device_name();
    name.chars().take(chain::MAX_NAME).collect()
}

fn answer_error(reply: &AccountReply) -> String {
    match reply {
        AccountReply::Error { error } => message(*error).to_owned(),
        _ => "resposta inesperada do servidor".to_owned(),
    }
}

/// What to tell the user.
pub fn message(error: AccountError) -> &'static str {
    match error {
        AccountError::InvalidEmail => "Esse e-mail não parece certo.",
        AccountError::WeakPassword => "A senha precisa ter de 10 a 128 caracteres.",
        AccountError::WrongCredentials => "E-mail ou senha errados.",
        AccountError::NotVerified => {
            "Confirme o e-mail antes: use o código que chegou na sua caixa de entrada."
        }
        AccountError::WrongCode => "Código errado ou vencido.",
        AccountError::TooManyAttempts => "Tentativas demais. Espere uns 15 minutos.",
        AccountError::NotLoggedIn => "Você não está mais na conta neste computador. Entre de novo.",
        AccountError::AlreadyEnabled => "A verificação em duas etapas já está ligada.",
        AccountError::NoSetup => "Comece a ligar a verificação em duas etapas de novo.",
        AccountError::ServerError => "O servidor teve um problema. Tente de novo daqui a pouco.",
        AccountError::ChainConflict => {
            "A lista de dispositivos mudou enquanto isso. Tente de novo."
        }
        AccountError::ChainInvalid => "O servidor não aceitou essa mudança na lista.",
        AccountError::NeedsTotp => "Ligue a verificação em duas etapas antes.",
    }
}

/// A QR code of `text`, as SVG, for the authenticator app.
pub fn qr_svg(text: &str) -> Option<String> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    Some(
        code.render::<qrcode::render::svg::Color>()
            .min_dimensions(200, 200)
            .quiet_zone(true)
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use hext_server::accounts::Mailer;

    use super::*;

    #[derive(Default)]
    struct Inbox(Mutex<Vec<String>>);

    impl Mailer for Inbox {
        fn send(&self, _to: &str, _subject: &str, body: &str) {
            self.0.lock().unwrap().push(body.to_owned());
        }
    }

    async fn code(inbox: &Inbox) -> String {
        for _ in 0..500 {
            if let Some(body) = inbox.0.lock().unwrap().last() {
                return body
                    .split(|c: char| !c.is_ascii_digit())
                    .find(|w| w.len() == 6)
                    .unwrap()
                    .to_owned();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no e-mail");
    }

    #[tokio::test]
    async fn the_login_is_kept_here_and_the_token_never_leaves() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = format!("ws://{}", listener.local_addr().unwrap());
        let inbox = Arc::new(Inbox::default());
        let state = Arc::new(hext_server::State::with_mailer(
            hext_server::Db::in_memory().unwrap(),
            inbox.clone(),
        ));
        tokio::spawn(hext_server::serve(
            listener,
            state,
            hext_server::Config::default(),
        ));

        let dir = std::env::temp_dir().join(format!("hext-account-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut account = Account::new(dir.join("viewer.key"));
        account.server = server;
        let (email, password) = ("dono@example.com".to_owned(), "cavalo correto".to_owned());

        assert_eq!(account.current().await, None);
        assert_eq!(
            account.call(AccountCall::Me).await,
            Err(message(AccountError::NotLoggedIn).to_owned())
        );
        let register = AccountCall::Register {
            email: email.clone(),
            password: password.clone(),
        };
        assert_eq!(account.call(register).await, Ok(AccountReply::Ok));
        let verify = AccountCall::Verify {
            email: email.clone(),
            code: code(&inbox).await,
            password: password.clone(),
        };
        assert_eq!(account.call(verify).await, Ok(AccountReply::Ok));
        let login = AccountCall::Login {
            email: email.clone(),
            password: "senha errada demais".into(),
            second: None,
        };
        assert_eq!(
            account.call(login).await,
            Err(message(AccountError::WrongCredentials).to_owned())
        );
        let login = AccountCall::Login {
            email: email.clone(),
            password,
            second: None,
        };
        let view = AccountView {
            email: email.clone(),
            totp: false,
        };
        assert_eq!(
            account.call(login).await,
            Ok(AccountReply::LoggedIn {
                token: String::new(),
                account: view.clone(),
            })
        );
        assert!(account.kept());
        // A new connection each call: the kept login is resumed.
        assert_eq!(account.current().await, Some(view));
        assert!(matches!(
            account.call(AccountCall::TotpSetup).await,
            Ok(AccountReply::Totp { .. })
        ));
        assert_eq!(
            account.call(AccountCall::Logout).await,
            Ok(AccountReply::Ok)
        );
        assert!(!account.kept());
        assert_eq!(account.current().await, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A server, and an account on it with 2FA, logged in from a first
    /// computer; the server's state, the inbox, the 2FA recovery codes.
    struct World {
        server: String,
        state: Arc<hext_server::State>,
        inbox: Arc<Inbox>,
        codes: Vec<String>,
        dirs: Vec<std::path::PathBuf>,
    }

    const EMAIL: &str = "dono@example.com";
    const PASSWORD: &str = "cavalo correto";

    impl World {
        async fn new() -> (Self, Account) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let server = format!("ws://{}", listener.local_addr().unwrap());
            let inbox = Arc::new(Inbox::default());
            let state = Arc::new(hext_server::State::with_mailer(
                hext_server::Db::in_memory().unwrap(),
                inbox.clone(),
            ));
            let config = hext_server::Config::default();
            tokio::spawn(hext_server::serve(listener, state.clone(), config));
            let mut world = Self {
                server,
                state,
                inbox,
                codes: Vec::new(),
                dirs: Vec::new(),
            };
            let first = world.computer();
            let register = AccountCall::Register {
                email: EMAIL.into(),
                password: PASSWORD.into(),
            };
            first.call(register).await.unwrap();
            let verify = AccountCall::Verify {
                email: EMAIL.into(),
                code: code(&world.inbox).await,
                password: PASSWORD.into(),
            };
            first.call(verify).await.unwrap();
            world.login(&first, None).await;
            let Ok(AccountReply::Totp { secret, .. }) = first.call(AccountCall::TotpSetup).await
            else {
                panic!("no 2FA secret");
            };
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let totp = hext_server::accounts::totp_code(&secret, now).unwrap();
            let Ok(AccountReply::RecoveryCodes { codes }) =
                first.call(AccountCall::TotpEnable { code: totp }).await
            else {
                panic!("no recovery codes");
            };
            world.codes = codes;
            (world, first)
        }

        /// Another computer (its own technician's key).
        fn computer(&mut self) -> Account {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("hext-chain-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let mut account = Account::new(dir.join("viewer.key"));
            account.server = self.server.clone();
            self.dirs.push(dir);
            account
        }

        async fn login(&mut self, account: &Account, second: Option<String>) {
            let login = AccountCall::Login {
                email: EMAIL.into(),
                password: PASSWORD.into(),
                second,
            };
            assert!(matches!(
                account.call(login).await,
                Ok(AccountReply::LoggedIn { .. })
            ));
        }

        /// Another computer, logged in to the account (with a 2FA recovery
        /// code).
        async fn second(&mut self) -> Account {
            let account = self.computer();
            let code = self.codes.pop().unwrap();
            self.login(&account, Some(code)).await;
            account
        }
    }

    impl Drop for World {
        fn drop(&mut self) {
            for dir in &self.dirs {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    #[tokio::test]
    async fn a_computer_joins_with_the_code_shown_on_another() {
        let (mut world, first) = World::new().await;
        assert_eq!(first.devices(None).await.unwrap().state, "none");
        let recovery = first.create_chain().await.unwrap();
        assert_eq!(recovery.len(), chain::RECOVERY_CODE_LEN + 4);
        let view = first.devices(None).await.unwrap();
        assert_eq!(view.state, "member");
        assert!(view.devices[0].me);
        assert!(first.create_chain().await.is_err(), "only once");

        let notebook = world.second().await;
        assert_eq!(notebook.devices(None).await.unwrap().state, "outside");
        let code = chain::new_code(chain::JOIN_CODE_LEN);
        // A wrong code is not let in.
        notebook
            .join(&chain::new_code(chain::JOIN_CODE_LEN))
            .await
            .unwrap();
        assert_eq!(first.approve(&code).await.unwrap(), None);
        // The right one is, typed any way.
        notebook
            .join(&code.to_lowercase().replace('-', ""))
            .await
            .unwrap();
        assert!(first.approve(&code).await.unwrap().is_some());
        let view = notebook.devices(None).await.unwrap();
        assert_eq!(view.state, "member");
        assert_eq!(view.devices.len(), 2);

        // The notebook takes the first one out; the list goes on there too.
        let first_key = first.key().unwrap().public_key().to_string();
        notebook.remove(&first_key).await.unwrap();
        assert_eq!(first.devices(None).await.unwrap().state, "outside");
        assert_eq!(notebook.devices(None).await.unwrap().devices.len(), 1);

        // With the recovery key, the first one comes back by itself.
        assert!(first.recover(&chain::new_code(20)).await.is_err());
        first.recover(&recovery).await.unwrap();
        assert_eq!(first.devices(None).await.unwrap().state, "member");
    }

    #[tokio::test]
    async fn a_dishonest_server_is_caught() {
        let (mut world, first) = World::new().await;
        first.create_chain().await.unwrap();
        let notebook = world.second().await;
        let code = chain::new_code(chain::JOIN_CODE_LEN);
        notebook.join(&code).await.unwrap();
        first.approve(&code).await.unwrap().unwrap();
        assert_eq!(notebook.devices(None).await.unwrap().state, "member");

        // The server hides the latest entry (the notebook coming in), as if
        // a removal had never happened: both computers saw it, so both see
        // through it.
        world
            .state
            .db
            .tamper("DELETE FROM account_chain WHERE seq = 1")
            .unwrap();
        assert_eq!(first.devices(None).await, Err(TAMPERED.to_owned()));
        assert_eq!(notebook.devices(None).await, Err(TAMPERED.to_owned()));

        // Or puts in a device of its own, signed with its own key.
        let intruder = DeviceKey::generate().unwrap();
        let other = DeviceKey::generate().unwrap().public_key();
        let entries = [Entry::create(&intruder, "Intruso", other)];
        let forged = serde_json::to_string(&entries[0])
            .unwrap()
            .replace('\'', "''");
        world
            .state
            .db
            .tamper(&format!(
                "DELETE FROM account_chain; INSERT INTO account_chain VALUES (1, 0, '{forged}');"
            ))
            .unwrap();
        assert!(first.devices(None).await.unwrap_err().starts_with(TAMPERED));
        // Even a computer that never saw the list does not take a forged
        // one: the recovery key is not the intruder's to name... but a
        // whole new chain looks like a new account, so it is the first one
        // seen; what holds it is that nobody's key is in it.
        let newcomer = world.second().await;
        assert_eq!(newcomer.devices(None).await.unwrap().state, "outside");
    }

    #[tokio::test]
    async fn the_accounts_computers_show_online_to_its_devices() {
        use hext_signal::{Client, Role};
        let (world, first) = World::new().await;
        first.create_chain().await.unwrap();
        // A computer, logged in as a host.
        let host_key = DeviceKey::generate().unwrap();
        let host = Client::connect(&world.server, &host_key, "Escritório", "t", Role::Host)
            .await
            .unwrap();
        let id = host.id();
        let viewer_key = first.key().unwrap();
        let mut viewer = Client::connect(&world.server, &viewer_key, "Notebook", "t", Role::Viewer)
            .await
            .unwrap();
        assert_eq!(viewer.presence(&[id]).await.unwrap(), vec![], "not yet");
        first
            .add_computer(
                &host_key.public_key().to_string(),
                &id.to_string(),
                "Escritório",
            )
            .await
            .unwrap();
        let view = first
            .devices(Some(&host_key.public_key().to_string()))
            .await
            .unwrap();
        assert_eq!(view.computers.len(), 1);
        assert!(view.listed && view.computers[0].me);
        assert_eq!(viewer.presence(&[id]).await.unwrap(), vec![id]);
        // A device of no account sees nothing.
        let stranger = DeviceKey::generate().unwrap();
        let mut stranger = Client::connect(&world.server, &stranger, "X", "t", Role::Viewer)
            .await
            .unwrap();
        assert_eq!(stranger.presence(&[id]).await.unwrap(), vec![]);
        // Out of the list, out of sight.
        first
            .remove_computer(&host_key.public_key().to_string())
            .await
            .unwrap();
        assert_eq!(viewer.presence(&[id]).await.unwrap(), vec![]);
        drop(host);
    }

    #[tokio::test]
    async fn the_bound_accounts_devices_come_in_while_on_its_list() {
        use crate::owners::{Owners, parse_id};
        use hext_session::host::Owners as _;
        use hext_signal::{Client, Role};
        let (mut world, first) = World::new().await;
        first.create_chain().await.unwrap();
        let host_key = Arc::new(DeviceKey::generate().unwrap());
        let host = Client::connect(&world.server, &host_key, "Escritório", "t", Role::Host)
            .await
            .unwrap();
        let hex_key = host_key.public_key().to_string();
        let dir = world.computer().file.parent().unwrap().to_owned();
        std::fs::create_dir_all(&dir).unwrap();
        let owners = Owners::new(
            dir.join("owners.json"),
            host_key.clone(),
            world.server.clone(),
        );
        let me = first.key().unwrap().public_key();
        assert_eq!(owners.way(&me), None, "not bound");

        let chain = parse_id(&first.devices(None).await.unwrap().chain).unwrap();
        // Off the account's computers, it is not bound.
        assert!(owners.bind(chain).await.is_err());
        first
            .add_computer(&hex_key, &host.id().to_string(), "Escritório")
            .await
            .unwrap();
        // Nor to a list the server does not have.
        assert!(owners.bind([7; 32]).await.is_err());
        owners.bind(chain).await.unwrap();
        assert_eq!(owners.bound().unwrap().0, chain);
        let way = owners.way(&me).unwrap();
        assert_eq!(way.via, "entrou como dono");
        assert!(way.permissions.control && way.permissions.files);
        assert_eq!(
            owners.way(&DeviceKey::generate().unwrap().public_key()),
            None
        );

        // A device that joins comes in after the next refresh...
        let notebook = world.second().await;
        let code = chain::new_code(chain::JOIN_CODE_LEN);
        notebook.join(&code).await.unwrap();
        first.approve(&code).await.unwrap().unwrap();
        let notebook_key = notebook.key().unwrap().public_key();
        assert!(!owners.still(&notebook_key));
        owners.refresh().await.unwrap();
        assert!(owners.still(&notebook_key));
        // ...and one taken out no longer does (a session of it ends).
        first.remove(&notebook_key.to_string()).await.unwrap();
        owners.refresh().await.unwrap();
        assert!(!owners.still(&notebook_key));
        assert!(owners.still(&me));
        // The computer taken off the list: nobody comes in by the account.
        first.remove_computer(&hex_key).await.unwrap();
        owners.refresh().await.unwrap();
        assert!(!owners.still(&me));

        // A server handing out an older list (the computer back on it) is
        // not believed, also after a restart (the file).
        world
            .state
            .db
            .tamper("DELETE FROM account_chain WHERE seq = (SELECT MAX(seq) FROM account_chain)")
            .unwrap();
        assert!(owners.refresh().await.is_err());
        assert!(!owners.still(&me));
        let again = Owners::new(dir.join("owners.json"), host_key, world.server.clone());
        assert!(again.refresh().await.is_err());
        assert!(!again.still(&me));

        owners.unbind().unwrap();
        assert_eq!(owners.bound(), None);
        drop(host);
    }

    #[test]
    fn the_authenticator_link_makes_a_qr_code() {
        let svg = qr_svg("otpauth://totp/Hext%20Remote:a%40b.com?secret=ABC&issuer=Hext%20Remote")
            .unwrap();
        assert!(svg.contains("<svg"), "{svg}");
    }
}
