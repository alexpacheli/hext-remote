//! Connecting by ID through `hext-server`.
//!
//! The host stays logged in, shows its ID and asks its user about each
//! request; on a yes it hands the viewer (through the server) where to
//! connect and a one-time session code. The viewer pins the host's device
//! key, remembered per ID (trust on first use).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use hext_crypto::{DeviceKey, PublicKey};
use hext_net::{Connection, Endpoint};
use hext_proto::Permissions;
use hext_session::host::{Attempt, BoxFuture};
use hext_session::{Grant, ResumeTokens, SessionCodes};
use hext_signal::{Check, Client, ClientMsg, DeviceId, KnownHosts, Rejection, Role, ServerMsg};
use tokio::sync::{Semaphore, mpsc};
use tracing::{info, warn};

use crate::APP_VERSION;

/// How long the host's user has to answer (the server gives up at 60 s).
pub const ANSWER_TIME: Duration = Duration::from_secs(55);
/// Waits between attempts to reach the server, doubling up to the last.
const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// How long the host waits for its relay before answering a request.
const RELAY_WAIT: Duration = Duration::from_secs(5);
/// How long a viewer coming back waits for the session it left to wind down.
const DISPLACE_WAIT: Duration = Duration::from_secs(5);
/// How long to try reaching the host (directly or through the relay).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// How much of a device's name is shown.
const MAX_NAME: usize = 64;

/// A name as the other side sent it, made safe to show: nothing that moves
/// a terminal's cursor or reverses text around the name.
pub fn shown_name(name: &str) -> String {
    name.chars().filter(|&c| shows(c)).take(MAX_NAME).collect()
}

/// Whether `c` stays in a name shown: not a control character, nor a mark
/// that reverses the text.
pub fn shows(c: char) -> bool {
    !c.is_control()
        && !matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Asks the host's user whether a device may connect.
pub trait Ask: Send {
    /// The question for `name` (ID `id`): what the user granted, or `None`
    /// for a no. A future dropped before it ends (the request was
    /// cancelled, or time ran out) is a no.
    fn ask<'a>(&'a mut self, name: &'a str, id: DeviceId) -> BoxFuture<'a, Option<Permissions>>;

    /// This computer's ID, once the server gave it (or a different one).
    fn show_id(&mut self, id: DeviceId);

    /// Whether the unattended password is on here.
    fn unattended(&self) -> bool {
        false
    }

    /// Whether the question will wait for someone to wake the screen.
    fn locked(&self) -> bool {
        false
    }

    /// Whether the device with `key` is a technician this computer trusts
    /// without the password (F4 marco 12). Only asked while the password is
    /// on.
    fn bound(&self, _key: &PublicKey) -> bool {
        false
    }

    /// How the device with `key` comes in by the account bound here (F5),
    /// without anyone accepting; `None`: it does not.
    fn owner<'a>(&'a mut self, _key: &'a PublicKey) -> BoxFuture<'a, Option<crate::owners::Way>> {
        Box::pin(async { None })
    }
}

/// What a session nobody accepted may do: all a technician needs.
fn unattended_permissions() -> Permissions {
    Permissions {
        clipboard: true,
        files: true,
        restart: true,
        ..Permissions::CONTROL
    }
}

/// Asks on the terminal.
pub struct Terminal(mpsc::UnboundedReceiver<String>);

impl Terminal {
    pub fn new() -> Self {
        Self(stdin_lines())
    }
}

impl Ask for Terminal {
    fn ask<'a>(&'a mut self, name: &'a str, id: DeviceId) -> BoxFuture<'a, Option<Permissions>> {
        Box::pin(async move {
            // Lines typed before the question are not answers to it.
            while self.0.try_recv().is_ok() {}
            println!();
            println!("  \"{name}\" (ID {id}) quer acessar este computador.");
            println!("  Aceitar? [s/N]");
            let line = self.0.recv().await?;
            matches!(
                line.trim().to_lowercase().as_str(),
                "s" | "sim" | "y" | "yes"
            )
            .then_some(Permissions::CONTROL)
        })
    }

    fn show_id(&mut self, id: DeviceId) {
        println!("  Seu ID: {id}");
        println!();
    }
}

/// Host side: stays logged in to `server` (reconnecting when it drops)
/// and asks the user (`ask`) about each request. Viewers it accepts get
/// `endpoint`'s addresses (relay and direct) to reach it; `slot` is free
/// while no session runs.
#[allow(clippy::too_many_arguments)]
pub async fn host(
    server: String,
    key: Arc<DeviceKey>,
    name: String,
    (endpoint, relay): (Endpoint, Option<hext_net::RelayUrl>),
    (codes, resumes): (SessionCodes, ResumeTokens),
    mut attempts: mpsc::UnboundedReceiver<Attempt>,
    slot: Arc<Semaphore>,
    mut ask: Box<dyn Ask>,
) {
    let has_relay = relay.is_some();
    // The server may name another relay (the account's, without the limit),
    // or take it back: on each login.
    let relays = hext_net::RelaySwitch::new(relay);
    let mut retry = RETRY_FIRST;
    let mut shown: Option<DeviceId> = None;
    loop {
        match Client::connect(&server, &key, &name, APP_VERSION, Role::Host).await {
            Ok(mut client) => {
                retry = RETRY_FIRST;
                let named = client.relay().map(|r| (r.url.as_str(), r.quic_port));
                relays.follow(&endpoint, named).await;
                if shown != Some(client.id()) {
                    ask.show_id(client.id());
                    shown = Some(client.id());
                } else {
                    info!("reconectado ao servidor");
                }
                if let Err(e) = serve(
                    &mut client,
                    ask.as_mut(),
                    (&endpoint, has_relay),
                    (&codes, &resumes),
                    &mut attempts,
                    &slot,
                )
                .await
                {
                    warn!("servidor de sinalização: {e}");
                }
            }
            Err(e) => warn!("servidor de sinalização: {e} (tentando de novo em {retry:?})"),
        }
        tokio::time::sleep(retry).await;
        retry = (retry * 2).min(RETRY_MAX);
    }
}

/// For a host without the password: no outcome ever comes.
pub fn no_attempts() -> mpsc::UnboundedReceiver<Attempt> {
    mpsc::unbounded_channel().1
}

/// What to tell the server about `attempt`.
fn attempt_message(attempt: &Attempt) -> Option<ClientMsg> {
    let viewer = attempt.viewer.parse().ok()?;
    Some(ClientMsg::Attempt {
        viewer,
        entered: attempt.entered,
    })
}

/// Answers requests, and reports `attempts`, until the connection to the
/// server ends. Outcomes that find no connection wait for the next one; one
/// caught by the connection dropping is lost, and the next one makes up for
/// it.
async fn serve(
    client: &mut Client,
    ask: &mut dyn Ask,
    (endpoint, has_relay): (&Endpoint, bool),
    (codes, resumes): (&SessionCodes, &ResumeTokens),
    attempts: &mut mpsc::UnboundedReceiver<Attempt>,
    slot: &Semaphore,
) -> Result<(), hext_signal::ClientError> {
    loop {
        let msg = tokio::select! {
            // First: a wrong password counts before that viewer's next request.
            biased;
            Some(attempt) = attempts.recv() => {
                if client.takes_attempts()
                    && let Some(msg) = attempt_message(&attempt)
                {
                    client.send(&msg).await?;
                }
                continue;
            }
            msg = client.recv() => msg?,
        };
        let ServerMsg::Request {
            request,
            from,
            from_key,
            from_name,
            unattended,
            remember,
            account,
        } = msg
        else {
            continue;
        };
        let from_name = shown_name(&from_name);
        // Only as the technician signed it, for this computer: a server
        // passing it on cannot point it elsewhere.
        let me = client.id();
        let account = account
            .filter(|ask| ask.holds(&from_key, me))
            .map(|ask| ask.chain);
        // The session it lost may still be on here: the host is the last to
        // know.
        let back = unattended && resumes.displace(&from_key);
        if back {
            let freed = async {
                while slot.available_permits() == 0 {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            };
            let _ = tokio::time::timeout(DISPLACE_WAIT, freed).await;
        }
        if slot.available_permits() == 0 {
            info!("pedido de {from_name} ({from}) recusado: já existe uma sessão");
            client.send(&ClientMsg::Reject { request }).await?;
            continue;
        }
        if unattended {
            // Nobody to ask: the token or the password decides, in the session.
            let answer = if back || ask.unattended() {
                info!("pedido de {from_name} ({from}) com a senha de acesso não assistido");
                let grant = Grant {
                    permissions: unattended_permissions(),
                    name: from_name,
                    id: from.to_string(),
                    needs_password: true,
                    remember,
                    bound: false,
                    via: None,
                    account,
                };
                accept(request, from_key, grant, (endpoint, has_relay), codes).await
            } else {
                info!(
                    "pedido de {from_name} ({from}) com senha, mas o acesso não assistido está desligado"
                );
                ClientMsg::NoUnattended { request }
            };
            client.send(&answer).await?;
            continue;
        }
        // A device of the account bound here (or of one it shares this
        // computer with): nobody to ask. The code is good only from the
        // device holding that key, and the session checks it still is.
        if let Some(way) = ask.owner(&from_key).await {
            info!("pedido de {from_name} ({from}), {}: aceito", way.via);
            let grant = Grant {
                permissions: way.permissions,
                name: from_name,
                id: from.to_string(),
                needs_password: false,
                remember: false,
                bound: true,
                via: Some(way.via),
                account: None,
            };
            let answer = accept(request, from_key, grant, (endpoint, has_relay), codes).await;
            client.send(&answer).await?;
            continue;
        }
        // A technician trusted here: nobody to ask, and no password. The code
        // is good only from the device holding that key.
        if ask.unattended() && ask.bound(&from_key) {
            info!("pedido de {from_name} ({from}), técnico vinculado: aceito sem senha");
            let grant = Grant {
                permissions: unattended_permissions(),
                name: from_name,
                id: from.to_string(),
                needs_password: false,
                remember: false,
                bound: true,
                via: None,
                account: None,
            };
            let answer = accept(request, from_key, grant, (endpoint, has_relay), codes).await;
            client.send(&answer).await?;
            continue;
        }
        if ask.locked() {
            client.send(&ClientMsg::Waiting { request }).await?;
        }
        // One deadline for the question, whatever else arrives meanwhile.
        let deadline = tokio::time::sleep(ANSWER_TIME);
        tokio::pin!(deadline);
        let question = ask.ask(&from_name, from);
        tokio::pin!(question);
        let accepted = loop {
            tokio::select! {
                answer = &mut question => break answer,
                msg = client.recv() => match msg? {
                    ServerMsg::Cancelled { request: r } if r == request => {
                        println!("  O pedido foi cancelado.");
                        break None;
                    }
                    // Others asking meanwhile wait their turn by being told
                    // no: the user answers one at a time.
                    ServerMsg::Request { request: other, .. } => {
                        client.send(&ClientMsg::Reject { request: other }).await?;
                    }
                    _ => {}
                },
                _ = &mut deadline => {
                    println!("  Sem resposta: recusado.");
                    break None;
                }
            }
        };
        // A session may have started (by IP) while the user decided.
        let busy = slot.available_permits() == 0;
        if accepted.is_some() && busy {
            println!("  Já existe uma sessão: o pedido foi recusado.");
        }
        let answer = if let Some(permissions) = accepted.filter(|_| !busy) {
            println!("  Aceito: conectando...");
            let grant = Grant {
                permissions,
                name: from_name.clone(),
                id: from.to_string(),
                needs_password: false,
                remember: false,
                bound: false,
                via: None,
                account: None,
            };
            accept(request, from_key, grant, (endpoint, has_relay), codes).await
        } else {
            ClientMsg::Reject { request }
        };
        client.send(&answer).await?;
    }
}

/// The yes to request `request`: where to connect, and a code good only from
/// the device that asked.
async fn accept(
    request: u64,
    from_key: PublicKey,
    grant: Grant,
    (endpoint, has_relay): (&Endpoint, bool),
    codes: &SessionCodes,
) -> ClientMsg {
    // The relay must be among the addresses: without it, a viewer on another
    // network has nothing to fall back on. Right after starting (or a network
    // change) it may still be connecting.
    if has_relay && endpoint.addr().relay_urls().next().is_none() {
        let _ = tokio::time::timeout(RELAY_WAIT, endpoint.online()).await;
    }
    let addrs = hext_net::addr_to_strings(&endpoint.addr());
    info!("endereços passados ao visualizador: {addrs:?}");
    ClientMsg::Accept {
        request,
        addrs,
        session_code: codes.issue(from_key, grant),
    }
}

/// Lines typed on the terminal, read on their own thread.
fn stdin_lines() -> mpsc::UnboundedReceiver<String> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// Logs in to `server` as a viewer: this also registers the device, which
/// our relay requires before it serves it.
pub async fn login(server: &str, key: &DeviceKey, name: &str) -> Result<Client> {
    Client::connect(server, key, name, APP_VERSION, Role::Viewer)
        .await
        .context("servidor de sinalização")
}

/// Viewer side: asks host `id` for access through `client` (logged in),
/// checks its key against the one remembered for that ID, and connects from
/// `endpoint` (directly or through the relay). Returns the connection, the
/// one-time code to present, and the host's key to keep once the session
/// gets in ([`Pin::keep`]). `unattended`: with the password, so nobody
/// is asked; `remember`: and asking to be trusted from then on, without it.
/// `on_waiting` runs if the host says its screen is locked.
pub async fn connect(
    mut client: Client,
    id: DeviceId,
    known: &KnownHosts,
    endpoint: &Endpoint,
    (unattended, remember, account): (bool, bool, Option<hext_signal::chain::BindAsk>),
    on_waiting: impl FnMut() + Send,
) -> Result<(Connection, String, Pin)> {
    if unattended {
        println!("Conectando a {id} com a senha...");
    } else {
        println!("Pedindo acesso a {id}... aguardando o outro computador aceitar.");
    }
    let access = match client
        .request_access(id, (unattended, remember), account, on_waiting)
        .await?
    {
        Ok(access) => access,
        Err(reason) => bail!("{}", rejection(reason)),
    };
    let _ = client.close().await;

    let check = known
        .check(id, &access.host_key)
        .with_context(|| known.path().display().to_string())?;
    if let Check::Changed { before } = check {
        bail!(
            "a chave do computador {id} mudou (antes {before}, agora {}). Pode ser outro computador \
             se passando por ele. Se ele foi reinstalado e você confia nisso, apague a linha do ID \
             {} em {}",
            access.host_key,
            id.get(),
            known.path().display()
        );
    }

    info!(
        "endereços do host: {:?}; os nossos: {:?}",
        access.addrs,
        hext_net::addr_to_strings(&endpoint.addr())
    );
    let addr = hext_net::addr_from_strings(&access.host_key, &access.addrs)?;
    let conn = match tokio::time::timeout(CONNECT_TIMEOUT, hext_net::connect(endpoint, addr)).await
    {
        Ok(Ok(conn)) => conn,
        Ok(Err(e)) => bail!("o outro computador aceitou, mas não foi possível conectar nele: {e}"),
        Err(_) => {
            bail!("o outro computador aceitou, mas não respondeu (nem direto nem pelo relay)")
        }
    };
    hext_net::log_paths(&conn, "visualizador");
    let pin = Pin {
        known: known.clone(),
        id,
        key: access.host_key,
        new: check == Check::New,
    };
    Ok((conn, access.session_code, pin))
}

/// A host's key, to remember for its ID once the session got in: holding
/// the key (QUIC) is not all, it must also have let us in (and, with the
/// password, proved it knows it). Until then, nothing is written: a server
/// pointing at someone else's computer leaves no trace to trust later.
pub struct Pin {
    known: KnownHosts,
    id: DeviceId,
    key: PublicKey,
    /// First connection to this ID.
    new: bool,
}

impl Pin {
    /// The session got in: the key is the host's. Written even when it was
    /// known (the same key), so what is on file is what was proved last.
    pub fn keep(self) -> Result<()> {
        self.known
            .remember(self.id, &self.key)
            .with_context(|| self.known.path().display().to_string())?;
        if self.new {
            info!("primeira conexão com {}: chave guardada", self.id);
        }
        Ok(())
    }
}

fn rejection(reason: Rejection) -> &'static str {
    match reason {
        Rejection::Declined => "o outro computador recusou o acesso",
        Rejection::Offline => "esse ID não está online",
        Rejection::Timeout => "ninguém respondeu no outro computador a tempo",
        Rejection::TooManyRequests => {
            "muitas tentativas seguidas, deste computador ou para esse ID; tente de novo mais tarde"
        }
        Rejection::Busy => "o outro computador já tem um pedido esperando resposta",
        Rejection::SelfConnect => "esse é o ID deste computador",
        Rejection::UnattendedOff => "esse computador não está com o acesso por senha ligado",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_only_text_to_show() {
        assert_eq!(shown_name("PC do João"), "PC do João");
        assert_eq!(shown_name("a\u{1b}[2Jb\nc"), "a[2Jbc");
        assert_eq!(shown_name("abc\u{202E}gnp.exe"), "abcgnp.exe");
        assert_eq!(shown_name(&"x".repeat(500)).chars().count(), MAX_NAME);
    }

    /// Nobody at this computer, with the password on.
    struct Nobody;

    impl Ask for Nobody {
        fn ask<'a>(&'a mut self, _: &'a str, _: DeviceId) -> BoxFuture<'a, Option<Permissions>> {
            Box::pin(async { None })
        }

        fn show_id(&mut self, _: DeviceId) {}

        fn unattended(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn the_outcome_of_a_request_reaches_the_server() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let db = hext_server::Db::in_memory().unwrap();
        let state = Arc::new(hext_server::State::new(db));
        let config = hext_server::Config::default();
        tokio::spawn(hext_server::serve(listener, state.clone(), config));

        let host_key = DeviceKey::generate().unwrap();
        let viewer_key = DeviceKey::generate().unwrap();
        let mut host = Client::connect(&url, &host_key, "Host", "t", Role::Host)
            .await
            .unwrap();
        let mut viewer = Client::connect(&url, &viewer_key, "Técnico", "t", Role::Viewer)
            .await
            .unwrap();
        let (to, id) = (host.id(), viewer.id());
        let endpoint = hext_net::bind(&host_key, &hext_net::EndpointConfig::default())
            .await
            .unwrap();
        let (codes, resumes) = (SessionCodes::default(), ResumeTokens::default());
        let slot = Semaphore::new(1);
        let (attempts, mut outcomes) = mpsc::unbounded_channel();
        let mut nobody = Nobody;

        let technician = async {
            let request = ClientMsg::Connect {
                request: 1,
                to,
                unattended: true,
                remember: false,
                account: None,
            };
            viewer.send(&request).await.unwrap();
            let answer = viewer.recv().await.unwrap();
            assert!(matches!(answer, ServerMsg::Accepted { .. }), "{answer:?}");
            let attempt = Attempt {
                viewer: id.to_string(),
                entered: true,
            };
            attempts.send(attempt).unwrap();
            for _ in 0..500 {
                if state.db.entered_within(to, id, 3600).unwrap() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("the server never heard");
        };
        tokio::select! {
            () = technician => {}
            end = serve(
                &mut host,
                &mut nobody,
                (&endpoint, false),
                (&codes, &resumes),
                &mut outcomes,
                &slot,
            ) => panic!("the host left: {end:?}"),
        }
    }

    /// Trusts `bound` without the password (on or off: `on`); asking the
    /// user is a no, and counted.
    struct Trusting {
        bound: PublicKey,
        on: bool,
        asked: usize,
    }

    impl Ask for Trusting {
        fn ask<'a>(&'a mut self, _: &'a str, _: DeviceId) -> BoxFuture<'a, Option<Permissions>> {
            self.asked += 1;
            Box::pin(async { None })
        }

        fn show_id(&mut self, _: DeviceId) {}

        fn unattended(&self) -> bool {
            self.on
        }

        fn bound(&self, key: &PublicKey) -> bool {
            *key == self.bound
        }
    }

    /// Sends `request` as a technician to a host that trusts it (with the
    /// password `on` or off): the answer, how many times the user was asked,
    /// and what the code grants (if one came).
    async fn as_technician(request: (bool, bool), on: bool) -> (ServerMsg, usize, Option<Grant>) {
        as_technician_asking(request, on, None).await
    }

    /// The same, also asking the host to bind to account `[4; 32]`, with
    /// the ask signed by the technician's key (`Some(true)`) or another
    /// (`Some(false)`).
    async fn as_technician_asking(
        request: (bool, bool),
        on: bool,
        signed_by_it: Option<bool>,
    ) -> (ServerMsg, usize, Option<Grant>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let db = hext_server::Db::in_memory().unwrap();
        let state = Arc::new(hext_server::State::new(db));
        tokio::spawn(hext_server::serve(
            listener,
            state,
            hext_server::Config::default(),
        ));
        let host_key = DeviceKey::generate().unwrap();
        let viewer_key = DeviceKey::generate().unwrap();
        let mut host = Client::connect(&url, &host_key, "Host", "t", Role::Host)
            .await
            .unwrap();
        let mut viewer = Client::connect(&url, &viewer_key, "Técnico", "t", Role::Viewer)
            .await
            .unwrap();
        let to = host.id();
        let endpoint = hext_net::bind(&host_key, &hext_net::EndpointConfig::default())
            .await
            .unwrap();
        let (codes, resumes) = (SessionCodes::default(), ResumeTokens::default());
        let slot = Semaphore::new(1);
        let mut outcomes = no_attempts();
        let mut ask = Trusting {
            bound: viewer_key.public_key(),
            on,
            asked: 0,
        };
        let (unattended, remember) = request;
        let technician = async {
            let other = DeviceKey::generate().unwrap();
            let account = signed_by_it.map(|own| {
                let key = if own { &viewer_key } else { &other };
                hext_signal::chain::BindAsk::new(key, [4; 32], to)
            });
            let request = ClientMsg::Connect {
                request: 1,
                to,
                unattended,
                remember,
                account,
            };
            viewer.send(&request).await.unwrap();
            viewer.recv().await.unwrap()
        };
        let answer = tokio::select! {
            answer = technician => answer,
            end = serve(
                &mut host,
                &mut ask,
                (&endpoint, false),
                (&codes, &resumes),
                &mut outcomes,
                &slot,
            ) => panic!("the host left: {end:?}"),
        };
        let grant = match &answer {
            ServerMsg::Accepted { session_code, .. } => {
                codes.redeem(session_code, &viewer_key.public_key())
            }
            _ => None,
        };
        (answer, ask.asked, grant)
    }

    #[tokio::test]
    async fn a_bound_technician_is_let_in_without_anyone_asked() {
        let (answer, asked, grant) = as_technician((false, false), true).await;
        assert!(matches!(answer, ServerMsg::Accepted { .. }), "{answer:?}");
        assert_eq!(asked, 0);
        let grant = grant.expect("a code for the technician's device");
        assert!(grant.bound && !grant.needs_password, "{grant:?}");
        assert!(grant.permissions.restart && grant.permissions.files);
    }

    #[tokio::test]
    async fn with_the_password_off_a_bound_technician_waits_for_a_yes() {
        let (answer, asked, _) = as_technician((false, false), false).await;
        assert!(matches!(answer, ServerMsg::Rejected { .. }), "{answer:?}");
        assert_eq!(asked, 1);
    }

    #[tokio::test]
    async fn asking_to_be_remembered_reaches_the_session() {
        let (_, _, grant) = as_technician((true, true), true).await;
        let grant = grant.expect("a code");
        assert!(grant.needs_password && grant.remember && !grant.bound);
        let (_, _, grant) = as_technician((true, false), true).await;
        assert!(!grant.expect("a code").remember);
    }

    #[tokio::test]
    async fn an_ask_to_bind_to_an_account_reaches_the_session_only_as_signed() {
        let (_, _, grant) = as_technician_asking((true, false), true, Some(true)).await;
        assert_eq!(grant.expect("a code").account, Some([4; 32]));
        // Signed by someone else (a server pointing it at its own account):
        // dropped.
        let (_, _, grant) = as_technician_asking((true, false), true, Some(false)).await;
        assert_eq!(grant.expect("a code").account, None);
    }

    #[test]
    fn an_attempt_names_the_viewer_as_the_grant_did() {
        let viewer: DeviceId = "482 913 557".parse().unwrap();
        let attempt = Attempt {
            viewer: viewer.to_string(),
            entered: false,
        };
        assert_eq!(
            attempt_message(&attempt),
            Some(ClientMsg::Attempt {
                viewer,
                entered: false
            })
        );
        let unknown = Attempt {
            viewer: "?".into(),
            entered: true,
        };
        assert_eq!(attempt_message(&unknown), None);
    }
}
