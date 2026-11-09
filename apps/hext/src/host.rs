use std::net::{IpAddr, SocketAddr, UdpSocket};

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use hext_codec::{EncoderPreference, EncoderSettings};
use hext_net::{EndpointConfig, NetError};
use hext_proto::{PeerInfo, RejectReason};
use hext_session::files::OwnFiles;
use hext_session::host::{self, HostConfig, LocalDesktop};
use hext_session::{ResumeTokens, SessionCodes, SessionError};
use ring::rand::{SecureRandom, SystemRandom};
use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::ui_link::{UiAsk, UiHub};
use crate::{APP_VERSION, CodecChoice, HostArgs, device_name, exe_name};

/// Delay after a wrong access code, so guessing is limited to one try per second.
const WRONG_CODE_PENALTY: Duration = Duration::from_secs(1);

pub async fn run(args: HostArgs) -> Result<()> {
    let key = Arc::new(crate::device_key(args.device_key.as_deref())?);
    let server = crate::server(&args.server);
    let relay = crate::relay_for(&args.relay, server)?;
    let has_relay = relay.is_some();
    if let (Some(server), true) = (server, has_relay) {
        // Registered before the endpoint reaches the relay, which serves
        // only registered devices (a first run would be turned away).
        let name = device_name();
        let register = crate::signaling::login(server, &key, &name);
        match tokio::time::timeout(Duration::from_secs(10), register).await {
            Ok(Ok(client)) => {
                let _ = client.close().await;
            }
            Ok(Err(e)) => warn!("{e:#}"),
            Err(_) => warn!("servidor de sinalização: sem resposta"),
        }
    }
    let endpoint = hext_net::bind(
        &key,
        &EndpointConfig {
            relay: relay.clone(),
            bind: Some(args.listen),
            accept: true,
        },
    )
    .await?;
    let code = match args.code {
        Some(code) => code,
        None => random_code()?,
    };
    // An all-whitespace code would normalize to empty and admit anyone.
    if code.chars().filter(|c| c.is_ascii_digit()).count() < 4 {
        anyhow::bail!("o código de acesso precisa de ao menos 4 dígitos");
    }
    if args.test_pattern {
        warn!("transmitindo a imagem de teste, não a tela (--test-pattern)");
    } else {
        // Keep running either way: Accessibility takes effect for the next
        // session; Screen Recording (macOS) only after restarting the host.
        if let Err(e) = hext_capture::preflight() {
            warn!("{e}");
        }
        hext_input::preflight();
    }

    // Where other machines reach this one: the address listened on, or the
    // LAN address when listening on all; and the port actually bound for
    // that address family (one socket per family; `--listen` may say 0).
    let reachable = match args.listen.ip() {
        ip if ip.is_unspecified() => local_ip(),
        ip => Some(ip),
    };
    let port = reachable
        .and_then(|ip| {
            let bound = endpoint.bound_sockets();
            bound
                .into_iter()
                .find(|s| s.is_ipv4() == ip.is_ipv4())
                .map(|s| s.port())
        })
        .unwrap_or(args.listen.port());
    let ip = reachable.map_or_else(|| "<IP-deste-PC>".to_owned(), |ip| ip.to_string());
    println!();
    println!("  Hext Remote — aguardando conexão");
    println!("  Endereço:          {ip}:{port}");
    println!("  Código de acesso:  {}", group(&code));
    println!("  Chave:             {}", key.public_key());
    println!();
    println!("  No outro computador:");
    // The port only when it is not the one `view` assumes.
    let target = if port == hext_proto::DEFAULT_PORT {
        ip.clone()
    } else {
        format!("{ip}:{port}")
    };
    println!(
        "    {} view {target} --code {code} --key {}",
        exe_name(),
        key.public_key()
    );
    if let Some(server) = server {
        // The server option only when it is not the one `view` assumes.
        let option = if server == crate::DEFAULT_SERVER {
            String::new()
        } else {
            format!(" --server {server}")
        };
        println!(
            "  ou, pelo ID (mostrado abaixo): {} view <ID>{option}",
            exe_name()
        );
    }
    println!();

    let session_codes = SessionCodes::default();
    let resumes = ResumeTokens::default();
    let encoder = EncoderSettings {
        bitrate_bps: args.bitrate_mbps * 1_000_000,
        max_fps: args.fps as f32,
        preference: match args.encoder {
            CodecChoice::Auto => EncoderPreference::Auto,
            CodecChoice::Software => EncoderPreference::Software,
            CodecChoice::Hardware => EncoderPreference::Hardware,
        },
    };
    let config = Arc::new(HostConfig {
        owners: None,
        peer: PeerInfo::local(APP_VERSION, device_name()),
        access_code: Some(code),
        session_codes: session_codes.clone(),
        desktop: Arc::new(LocalDesktop {
            display: args.display,
            test_pattern: args.test_pattern,
            encoder,
        }),
        presence: None,
        unattended: None,
        resumes: resumes.clone(),
        restart: None,
        files: None,
    });
    // One session at a time, handshake included: attempts are serialized,
    // which together with the penalty rate-limits code guessing.
    let slot = Arc::new(Semaphore::new(1));

    if let Some(server) = server.map(str::to_owned) {
        // Viewers accepted through the server get this endpoint's current
        // addresses: the relay and the direct ones `iroh` found.
        tokio::spawn(crate::signaling::host(
            server,
            key.clone(),
            device_name(),
            (endpoint.clone(), relay.clone()),
            (session_codes, resumes),
            crate::signaling::no_attempts(),
            slot.clone(),
            Box::new(crate::signaling::Terminal::new()),
        ));
    }

    accept(&endpoint, config, slot, tokio::signal::ctrl_c()).await;
    endpoint.close().await;
    Ok(())
}

/// The portable app's host: by ID only (no access code), asking through the
/// interface and showing it the session (`hub`), this user's screen. Runs for
/// as long as the app does.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub async fn portable(hub: UiHub) -> Result<()> {
    let key = Arc::new(crate::device_key(None)?);
    hub.set_key(&key.public_key());
    let name = device_name();
    // The user's name for this computer, theirs to change here.
    hub.set_name(name.clone(), hext_crypto::config_dir().ok());
    // (On macOS, installed is this same app, opened at login.)
    if cfg!(windows) && crate::service::installed() {
        hub.set_notice(
            "O serviço do Hext Remote está instalado: este app tem outro ID, só dele.".to_owned(),
        );
    }
    // Registered before the relay sees this computer (it serves only
    // registered ones).
    let register = crate::signaling::login(crate::DEFAULT_SERVER, &key, &name);
    match tokio::time::timeout(Duration::from_secs(10), register).await {
        Ok(Ok(client)) => {
            let _ = client.close().await;
        }
        Ok(Err(e)) => warn!("{e:#}"),
        Err(_) => warn!("servidor de sinalização: sem resposta"),
    }
    let relay = crate::relay(crate::DEFAULT_RELAY)?;
    let endpoint = hext_net::bind(
        &key,
        &EndpointConfig {
            relay: relay.clone(),
            bind: None,
            accept: true,
        },
    )
    .await?;
    // Keeps running either way: Accessibility takes effect for the next
    // session; Screen Recording (macOS) only after restarting the app.
    if let Err(e) = hext_capture::preflight() {
        warn!("{e}");
        hub.set_notice(e.to_string());
    }
    hext_input::preflight();

    let session_codes = SessionCodes::default();
    let resumes = ResumeTokens::default();
    let restart = crate::service::own_restart(&resumes);
    // Installed on the Mac: the password, the technicians and the account
    // let people in without anyone accepting (on Windows, the service does).
    let login = crate::service::login_access(&key, &hub);
    let (owners, unattended, attempts, ask): (_, _, _, Box<dyn crate::signaling::Ask>) = match login
    {
        Some(login) => (
            Some(login.owners),
            Some(login.unattended),
            login.attempts,
            login.ask,
        ),
        None => (
            None,
            None,
            crate::signaling::no_attempts(),
            Box::new(UiAsk(hub.clone())),
        ),
    };
    let config = Arc::new(HostConfig {
        owners,
        peer: PeerInfo::local(APP_VERSION, name.clone()),
        access_code: None,
        session_codes: session_codes.clone(),
        desktop: Arc::new(LocalDesktop {
            display: 0,
            test_pattern: false,
            encoder: EncoderSettings::default(),
        }),
        presence: Some(Arc::new(hub.clone())),
        unattended,
        resumes: resumes.clone(),
        restart,
        files: Some(Arc::new(OwnFiles(crate::downloads()))),
    });
    let slot = Arc::new(Semaphore::new(1));
    tokio::spawn(crate::signaling::host(
        crate::DEFAULT_SERVER.to_owned(),
        key,
        name,
        (endpoint.clone(), relay.clone()),
        (session_codes, resumes),
        attempts,
        slot.clone(),
        ask,
    ));
    accept(&endpoint, config, slot, std::future::pending::<()>()).await;
    endpoint.close().await;
    Ok(())
}

/// Serves the sessions coming to `endpoint`, one at a time (`slot`), until
/// `until` completes.
pub async fn accept(
    endpoint: &hext_net::Endpoint,
    config: Arc<HostConfig>,
    slot: Arc<Semaphore>,
    until: impl std::future::Future,
) {
    tokio::pin!(until);
    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break };
                let Ok(permit) = slot.clone().try_acquire_owned() else {
                    info!("recusando {:?}: já existe uma sessão", incoming.remote_addr());
                    incoming.refuse();
                    continue;
                };
                let config = config.clone();
                tokio::spawn(async move {
                    let remote = format!("{:?}", incoming.remote_addr());
                    let result = match incoming.await {
                        Ok(conn) => host::serve(conn, config).await,
                        Err(e) => Err(NetError::from(e).into()),
                    };
                    match result {
                        Ok(_) => {}
                        Err(SessionError::Net(NetError::Rejected(RejectReason::WrongCode))) => {
                            warn!("{remote}: código de acesso errado");
                            tokio::time::sleep(WRONG_CODE_PENALTY).await;
                        }
                        Err(e) => warn!("sessão {remote}: {e}"),
                    }
                    drop(permit);
                });
            }
            _ = &mut until => break,
        }
    }
}

/// A random 9-digit code, like "482913557".
fn random_code() -> Result<String> {
    let mut bytes = [0u8; 8];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("sem fonte de números aleatórios"))?;
    Ok(format!("{:09}", u64::from_le_bytes(bytes) % 1_000_000_000))
}

/// "482913557" → "482 913 557".
fn group(code: &str) -> String {
    let chars: Vec<char> = code.chars().collect();
    chars
        .chunks(3)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The LAN address other machines would use to reach this one.
/// Connecting a UDP socket sends nothing; it only picks the outgoing interface.
fn local_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0))).ok()?;
    socket.connect(SocketAddr::from(([192, 0, 2, 1], 9))).ok()?;
    Some(socket.local_addr().ok()?.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_nine_digits_and_grouped() {
        let code = random_code().unwrap();
        assert_eq!(code.len(), 9);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
        assert_eq!(group("482913557"), "482 913 557");
    }
}
