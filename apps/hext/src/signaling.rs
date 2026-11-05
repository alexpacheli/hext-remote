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
use crate::ui_link::HOST_LOCKED;

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
    name.chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        })
        .take(MAX_NAME)
        .collect()
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
    (endpoint, has_relay): (Endpoint, bool),
    (codes, resumes): (SessionCodes, ResumeTokens),
    mut attempts: mpsc::UnboundedReceiver<Attempt>,
    slot: Arc<Semaphore>,
    mut ask: Box<dyn Ask>,
) {
    let mut retry = RETRY_FIRST;
    let mut shown: Option<DeviceId> = None;
    loop {
        match Client::connect(&server, &key, &name, APP_VERSION, Role::Host).await {
            Ok(mut client) => {
                retry = RETRY_FIRST;
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
        } = msg
        else {
            continue;
        };
        let from_name = shown_name(&from_name);
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
                    permissions: Permissions {
                        clipboard: true,
                        restart: true,
                        ..Permissions::CONTROL
                    },
                    name: from_name,
                    id: from.to_string(),
                    needs_password: true,
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
    ClientMsg::Accept {
        request,
        addrs: hext_net::addr_to_strings(&endpoint.addr()),
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
/// `endpoint` (directly or through the relay). Returns the connection and
/// the one-time code to present. `unattended`: with the password, so nobody
/// is asked.
pub async fn connect(
    mut client: Client,
    id: DeviceId,
    known: &KnownHosts,
    endpoint: &Endpoint,
    unattended: bool,
) -> Result<(Connection, String)> {
    if unattended {
        println!("Conectando a {id} com a senha...");
    } else {
        println!("Pedindo acesso a {id}... aguardando o outro computador aceitar.");
    }
    let on_waiting = || println!("{HOST_LOCKED}");
    let access = match client.request_access(id, unattended, on_waiting).await? {
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

    let addr = hext_net::addr_from_strings(&access.host_key, &access.addrs)?;
    let conn = match tokio::time::timeout(CONNECT_TIMEOUT, hext_net::connect(endpoint, addr)).await
    {
        Ok(Ok(conn)) => conn,
        Ok(Err(e)) => bail!("o outro computador aceitou, mas não foi possível conectar nele: {e}"),
        Err(_) => {
            bail!("o outro computador aceitou, mas não respondeu (nem direto nem pelo relay)")
        }
    };
    // The handshake proved the host holds that key.
    if check == Check::New {
        known
            .remember(id, &access.host_key)
            .with_context(|| known.path().display().to_string())?;
        info!("primeira conexão com {id}: chave guardada");
    }
    Ok((conn, access.session_code))
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
