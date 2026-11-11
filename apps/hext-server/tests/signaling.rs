//! The server in-process with real WebSocket clients.

use std::sync::Arc;
use std::time::{Duration, Instant};

use hext_crypto::DeviceKey;
use hext_server::{Config, Db, State};
use hext_signal::{Client, ClientError, ClientMsg, DeviceId, PROTOCOL, Rejection, Role, ServerMsg};

/// A server on a random local port.
async fn server() -> (String, Arc<State>) {
    server_with(Config::default()).await
}

async fn server_with(config: Config) -> (String, Arc<State>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let state = Arc::new(State::new(Db::in_memory().unwrap()));
    tokio::spawn(hext_server::serve(listener, state.clone(), config));
    (url, state)
}

/// A plain HTTP request to the server; the status code and body.
async fn http(url: &str, request: &str) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = url.trim_start_matches("ws://");
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let status = response[9..12].parse().unwrap();
    let body = response.split("\r\n\r\n").nth(1).unwrap_or("").to_owned();
    (status, body)
}

fn relay_question(token: &str, key: &str) -> String {
    format!(
        "POST /v1/relay-access HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\
         Authorization: Bearer {token}\r\nX-Iroh-Endpoint-Id: {key}\r\nContent-Length: 0\r\n\r\n"
    )
}

#[tokio::test]
async fn only_registered_devices_may_use_the_relay() {
    let config = Config {
        relay_token: Some("s3cr3t".into()),
        ..Default::default()
    };
    let (url, _) = server_with(config).await;
    let known = DeviceKey::generate().unwrap();
    login(&url, &known, "PC").await.close().await.unwrap();
    let known = known.public_key().to_string();
    let stranger = DeviceKey::generate().unwrap().public_key().to_string();

    assert_eq!(
        http(&url, &relay_question("s3cr3t", &known)).await,
        (200, "true".to_owned())
    );
    assert_eq!(
        http(&url, &relay_question("s3cr3t", &stranger)).await.0,
        403
    );
    // Only the relay (which has the secret) may ask.
    assert_eq!(http(&url, &relay_question("wrong", &known)).await.0, 401);
    assert_eq!(
        http(&url, &relay_question("s3cr3t", "nonsense")).await.0,
        400
    );
}

#[tokio::test]
async fn without_a_relay_secret_nobody_is_answered() {
    let (url, _) = server().await;
    let key = DeviceKey::generate().unwrap();
    login(&url, &key, "PC").await;
    let question = relay_question("", &key.public_key().to_string());
    assert_eq!(http(&url, &question).await.0, 404);
}

async fn login(url: &str, key: &DeviceKey, name: &str) -> Client {
    Client::connect(url, key, name, "0.1.0", Role::Host)
        .await
        .unwrap()
}

async fn recv(client: &mut Client) -> ServerMsg {
    tokio::time::timeout(Duration::from_secs(5), client.recv())
        .await
        .expect("a message in time")
        .unwrap()
}

/// Waits until the server has `n` devices online.
async fn wait_online(state: &State, n: usize) {
    for _ in 0..200 {
        if state.online() == n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("expected {n} online, got {}", state.online());
}

#[tokio::test]
async fn a_device_keeps_its_id() {
    let (url, _) = server().await;
    let key = DeviceKey::generate().unwrap();
    let first = login(&url, &key, "PC").await;
    let id = first.id();
    first.close().await.unwrap();
    let again = login(&url, &key, "PC").await;
    assert_eq!(again.id(), id);
    let other = login(&url, &DeviceKey::generate().unwrap(), "Mac").await;
    assert_ne!(other.id(), id);
}

#[tokio::test]
async fn request_accept_and_decline() {
    let (url, state) = server().await;
    let host_key = DeviceKey::generate().unwrap();
    let mut host = login(&url, &host_key, "Host").await;
    let viewer_key = DeviceKey::generate().unwrap();
    let mut viewer = login(&url, &viewer_key, "Técnico").await;

    let ask = hext_signal::chain::BindAsk::new(&viewer_key, [3; 32], host.id());
    viewer
        .send(&ClientMsg::Connect {
            request: 1,
            to: host.id(),
            unattended: false,
            // (Without the password there is nothing to remember, nor an
            // account to bind to: dropped.)
            remember: true,
            account: Some(ask),
        })
        .await
        .unwrap();
    let ServerMsg::Request {
        request,
        from,
        from_key,
        from_name,
        unattended,
        remember,
        account,
    } = recv(&mut host).await
    else {
        panic!("expected a request");
    };
    assert!(!unattended && !remember && account.is_none());
    assert_eq!(
        (from, from_key, from_name.as_str()),
        (viewer.id(), viewer_key.public_key(), "Técnico")
    );
    host.send(&ClientMsg::Accept {
        request,
        addrs: vec!["192.168.0.10:7420".into()],
        session_code: "s3cr3t".into(),
    })
    .await
    .unwrap();
    assert_eq!(
        recv(&mut viewer).await,
        ServerMsg::Accepted {
            request: 1,
            host_key: host_key.public_key(),
            addrs: vec!["192.168.0.10:7420".into()],
            session_code: "s3cr3t".into(),
        }
    );

    viewer
        .send(&ClientMsg::Connect {
            request: 2,
            to: host.id(),
            unattended: false,
            remember: false,
            account: None,
        })
        .await
        .unwrap();
    let ServerMsg::Request { request, .. } = recv(&mut host).await else {
        panic!("expected a request");
    };
    host.send(&ClientMsg::Reject { request }).await.unwrap();
    assert_eq!(
        recv(&mut viewer).await,
        ServerMsg::Rejected {
            request: 2,
            reason: Rejection::Declined
        }
    );

    assert_eq!(
        audit(&state, host.id(), 4).await,
        ["requested", "accepted", "requested", "declined"]
    );
}

/// The audit events about `to`, once there are `n` (they are written in
/// the background).
async fn audit(state: &State, to: DeviceId, n: usize) -> Vec<String> {
    for _ in 0..200 {
        let events: Vec<String> = state
            .db
            .audit_of(to)
            .unwrap()
            .into_iter()
            .map(|(_, event)| event)
            .collect();
        if events.len() >= n {
            return events;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("audit entries never arrived");
}

#[tokio::test]
async fn offline_and_unknown_ids_look_the_same() {
    let (url, _) = server().await;
    let mut viewer = login(&url, &DeviceKey::generate().unwrap(), "V").await;
    let gone_key = DeviceKey::generate().unwrap();
    let gone = login(&url, &gone_key, "Off").await;
    let gone_id = gone.id();
    gone.close().await.unwrap();
    // Whatever ID was never assigned (one in a million chance otherwise).
    let unknown = DeviceId::new(123_456_789).unwrap();
    for (request, to) in [(1, gone_id), (2, unknown)] {
        // The closed one may take a moment to be noticed.
        let mut reason = None;
        for _ in 0..50 {
            viewer
                .send(&ClientMsg::Connect {
                    request,
                    to,
                    unattended: false,
                    remember: false,
                    account: None,
                })
                .await
                .unwrap();
            if let ServerMsg::Rejected { reason: r, .. } = recv(&mut viewer).await {
                reason = Some(r);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(reason, Some(Rejection::Offline));
    }
}

#[tokio::test]
async fn cannot_connect_to_itself() {
    let (url, _) = server().await;
    let mut me = login(&url, &DeviceKey::generate().unwrap(), "Me").await;
    let to = me.id();
    me.send(&ClientMsg::Connect {
        request: 5,
        to,
        unattended: false,
        remember: false,
        account: None,
    })
    .await
    .unwrap();
    assert_eq!(
        recv(&mut me).await,
        ServerMsg::Rejected {
            request: 5,
            reason: Rejection::SelfConnect
        }
    );
}

#[tokio::test]
async fn guessing_ids_is_rate_limited() {
    let (url, _) = server().await;
    let mut viewer = login(&url, &DeviceKey::generate().unwrap(), "V").await;
    let mut reasons = Vec::new();
    for request in 0..12u64 {
        let to = DeviceId::new(200_000_000 + request as u32).unwrap();
        viewer
            .send(&ClientMsg::Connect {
                request,
                to,
                unattended: false,
                remember: false,
                account: None,
            })
            .await
            .unwrap();
        if let ServerMsg::Rejected { reason, .. } = recv(&mut viewer).await {
            reasons.push(reason);
        }
    }
    assert_eq!(reasons[..10], [Rejection::Offline; 10]);
    assert_eq!(reasons[10..], [Rejection::TooManyRequests; 2]);
}

#[tokio::test]
async fn reconnecting_does_not_reset_the_limit() {
    let (url, _) = server().await;
    let key = DeviceKey::generate().unwrap();
    let mut last = None;
    for round in 0..2u64 {
        let mut viewer = login(&url, &key, "V").await;
        for i in 0..6u64 {
            let request = round * 10 + i;
            let to = DeviceId::new(300_000_000 + request as u32).unwrap();
            viewer
                .send(&ClientMsg::Connect {
                    request,
                    to,
                    unattended: false,
                    remember: false,
                    account: None,
                })
                .await
                .unwrap();
            if let ServerMsg::Rejected { reason, .. } = recv(&mut viewer).await {
                last = Some(reason);
            }
        }
        viewer.close().await.unwrap();
    }
    // 12 requests in two connections: the last ones are over the limit of 10.
    assert_eq!(last, Some(Rejection::TooManyRequests));
}

#[tokio::test]
async fn one_waiting_request_per_viewer_and_host() {
    let (url, _) = server().await;
    let mut host = login(&url, &DeviceKey::generate().unwrap(), "H").await;
    let mut viewer = login(&url, &DeviceKey::generate().unwrap(), "V").await;
    let to = host.id();
    viewer
        .send(&ClientMsg::Connect {
            request: 1,
            to,
            unattended: false,
            remember: false,
            account: None,
        })
        .await
        .unwrap();
    assert!(matches!(recv(&mut host).await, ServerMsg::Request { .. }));
    // Asking again while the first waits does not pile up on the host.
    viewer
        .send(&ClientMsg::Connect {
            request: 2,
            to,
            unattended: false,
            remember: false,
            account: None,
        })
        .await
        .unwrap();
    assert_eq!(
        recv(&mut viewer).await,
        ServerMsg::Rejected {
            request: 2,
            reason: Rejection::Busy
        }
    );
}

/// Asks `host` from `viewer` and has the host answer; the viewer's answer.
async fn ask(viewer: &mut Client, host: &mut Client, request: u64, accept: bool) -> ServerMsg {
    let to = host.id();
    viewer
        .send(&ClientMsg::Connect {
            request,
            to,
            unattended: false,
            remember: false,
            account: None,
        })
        .await
        .unwrap();
    let ServerMsg::Request { request: asked, .. } = recv(host).await else {
        panic!("expected a request");
    };
    let answer = if accept {
        ClientMsg::Accept {
            request: asked,
            addrs: vec![],
            session_code: "c".into(),
        }
    } else {
        ClientMsg::Reject { request: asked }
    };
    host.send(&answer).await.unwrap();
    recv(viewer).await
}

/// Sends a request that the server refuses at once; its reason.
async fn refused(viewer: &mut Client, to: DeviceId, request: u64) -> Rejection {
    refused_as(viewer, to, request, false).await
}

async fn refused_as(
    viewer: &mut Client,
    to: DeviceId,
    request: u64,
    unattended: bool,
) -> Rejection {
    viewer
        .send(&ClientMsg::Connect {
            request,
            to,
            unattended,
            remember: false,
            account: None,
        })
        .await
        .unwrap();
    match recv(viewer).await {
        ServerMsg::Rejected { reason, .. } => reason,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn a_flooded_host_closes_to_strangers_only() {
    let (url, state) = server().await;
    let mut host = login(&url, &DeviceKey::generate().unwrap(), "H").await;
    let mut usual = login(&url, &DeviceKey::generate().unwrap(), "Técnico").await;
    let mut strangers = Vec::new();
    for name in ["A", "B", "C", "D"] {
        strangers.push(login(&url, &DeviceKey::generate().unwrap(), name).await);
    }
    let to = host.id();

    assert!(matches!(
        ask(&mut usual, &mut host, 1, true).await,
        ServerMsg::Accepted { .. }
    ));
    // One stranger alone cannot close the host: past its share it is the
    // only one refused.
    for request in 0..3 {
        assert!(matches!(
            ask(&mut strangers[0], &mut host, request, false).await,
            ServerMsg::Rejected {
                reason: Rejection::Declined,
                ..
            }
        ));
    }
    assert_eq!(
        refused(&mut strangers[0], to, 3).await,
        Rejection::TooManyRequests
    );
    // Asking again while a request waits does not count either.
    let mut waiting = strangers.remove(1);
    waiting
        .send(&ClientMsg::Connect {
            request: 0,
            to,
            unattended: false,
            remember: false,
            account: None,
        })
        .await
        .unwrap();
    let ServerMsg::Request { request: asked, .. } = recv(&mut host).await else {
        panic!("expected a request");
    };
    for request in 1..5 {
        assert_eq!(refused(&mut waiting, to, request).await, Rejection::Busy);
    }
    host.send(&ClientMsg::Reject { request: asked })
        .await
        .unwrap();
    recv(&mut waiting).await;
    // 10 so far (the usual technician's first request included): the next
    // stranger closes the host to strangers.
    for request in 5..7 {
        ask(&mut waiting, &mut host, request, false).await;
    }
    for request in 0..3 {
        ask(&mut strangers[1], &mut host, request, false).await;
    }
    assert_eq!(
        refused(&mut strangers[2], to, 0).await,
        Rejection::TooManyRequests
    );
    // A device the host accepted before still gets through.
    assert!(matches!(
        ask(&mut usual, &mut host, 2, true).await,
        ServerMsg::Accepted { .. }
    ));
    assert!(
        audit(&state, to, 12)
            .await
            .contains(&"host_locked".to_owned())
    );
}

#[tokio::test]
async fn new_devices_per_address_are_limited() {
    let (url, _) = server().await;
    let known = DeviceKey::generate().unwrap();
    login(&url, &known, "K").await.close().await.unwrap();
    for _ in 0..9 {
        login(&url, &DeviceKey::generate().unwrap(), "N").await;
    }
    let refused = Client::connect(
        &url,
        &DeviceKey::generate().unwrap(),
        "N",
        "0.1.0",
        Role::Host,
    )
    .await;
    assert!(
        matches!(refused, Err(ClientError::Server(_))),
        "{:?}",
        refused.err()
    );
    // A device that already has an ID still gets in.
    login(&url, &known, "K").await;
}

#[tokio::test]
async fn connections_at_once_per_key_are_capped() {
    let (url, _) = server().await;
    let key = DeviceKey::generate().unwrap();
    let mut open = Vec::new();
    for _ in 0..hext_server::signal::CONNECTIONS_PER_KEY {
        open.push(
            Client::connect(&url, &key, "PC", "0.1.0", Role::Viewer)
                .await
                .unwrap(),
        );
    }
    let refused = Client::connect(&url, &key, "PC", "0.1.0", Role::Viewer).await;
    let Err(ClientError::Server(message)) = refused else {
        panic!("expected a refusal");
    };
    assert!(message.contains("conexões abertas demais"), "{message}");
    // One closes: room for another.
    open.pop().unwrap().close().await.unwrap();
    let mut again = None;
    for _ in 0..50 {
        if let Ok(client) = Client::connect(&url, &key, "PC", "0.1.0", Role::Viewer).await {
            again = Some(client);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(again.is_some());
}

#[tokio::test]
async fn a_viewer_leaving_cancels_its_request() {
    let (url, state) = server().await;
    let mut host = login(&url, &DeviceKey::generate().unwrap(), "H").await;
    let mut viewer = login(&url, &DeviceKey::generate().unwrap(), "V").await;
    viewer
        .send(&ClientMsg::Connect {
            request: 1,
            to: host.id(),
            unattended: false,
            remember: false,
            account: None,
        })
        .await
        .unwrap();
    let ServerMsg::Request { request, .. } = recv(&mut host).await else {
        panic!("expected a request");
    };
    viewer.close().await.unwrap();
    assert_eq!(recv(&mut host).await, ServerMsg::Cancelled { request });
    wait_online(&state, 1).await;
}

#[tokio::test]
async fn unanswered_requests_time_out() {
    let (url, state) = server().await;
    let mut host = login(&url, &DeviceKey::generate().unwrap(), "H").await;
    let mut viewer = login(&url, &DeviceKey::generate().unwrap(), "V").await;
    viewer
        .send(&ClientMsg::Connect {
            request: 3,
            to: host.id(),
            unattended: false,
            remember: false,
            account: None,
        })
        .await
        .unwrap();
    let ServerMsg::Request { request, .. } = recv(&mut host).await else {
        panic!("expected a request");
    };
    // As if the timeout had passed.
    state
        .expire(Instant::now() + hext_server::signal::REQUEST_TIMEOUT)
        .await;
    assert_eq!(
        recv(&mut viewer).await,
        ServerMsg::Rejected {
            request: 3,
            reason: Rejection::Timeout
        }
    );
    assert_eq!(recv(&mut host).await, ServerMsg::Cancelled { request });
    // A late answer goes nowhere.
    host.send(&ClientMsg::Accept {
        request,
        addrs: vec![],
        session_code: String::new(),
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn only_the_host_can_answer() {
    let (url, _) = server().await;
    let mut host = login(&url, &DeviceKey::generate().unwrap(), "H").await;
    let mut viewer = login(&url, &DeviceKey::generate().unwrap(), "V").await;
    let mut other = login(&url, &DeviceKey::generate().unwrap(), "X").await;
    viewer
        .send(&ClientMsg::Connect {
            request: 1,
            to: host.id(),
            unattended: false,
            remember: false,
            account: None,
        })
        .await
        .unwrap();
    let ServerMsg::Request { request, .. } = recv(&mut host).await else {
        panic!("expected a request");
    };
    // Another device answering the host's request is ignored.
    other
        .send(&ClientMsg::Accept {
            request,
            addrs: vec!["evil:1".into()],
            session_code: "x".into(),
        })
        .await
        .unwrap();
    host.send(&ClientMsg::Reject { request }).await.unwrap();
    assert_eq!(
        recv(&mut viewer).await,
        ServerMsg::Rejected {
            request: 1,
            reason: Rejection::Declined
        }
    );
}

#[tokio::test]
async fn a_viewer_with_the_same_key_leaves_the_host_alone() {
    let (url, state) = server().await;
    let key = DeviceKey::generate().unwrap();
    let mut host = login(&url, &key, "PC").await;
    let mut viewer = Client::connect(&url, &key, "PC", "0.1.0", Role::Viewer)
        .await
        .unwrap();
    assert_eq!(viewer.id(), host.id());
    wait_online(&state, 2).await;

    // Requests for the ID still reach the host…
    let mut other = login(&url, &DeviceKey::generate().unwrap(), "X").await;
    other
        .send(&ClientMsg::Connect {
            request: 1,
            to: host.id(),
            unattended: false,
            remember: false,
            account: None,
        })
        .await
        .unwrap();
    assert!(matches!(recv(&mut host).await, ServerMsg::Request { .. }));
    // …and the viewer can still ask.
    let to = other.id();
    viewer
        .send(&ClientMsg::Connect {
            request: 2,
            to,
            unattended: false,
            remember: false,
            account: None,
        })
        .await
        .unwrap();
    assert!(matches!(recv(&mut other).await, ServerMsg::Request { .. }));

    // A device logged in only as a viewer takes no requests.
    let lone = Client::connect(
        &url,
        &DeviceKey::generate().unwrap(),
        "V",
        "0.1.0",
        Role::Viewer,
    )
    .await
    .unwrap();
    let to = lone.id();
    other
        .send(&ClientMsg::Connect {
            request: 3,
            to,
            unattended: false,
            remember: false,
            account: None,
        })
        .await
        .unwrap();
    assert_eq!(
        recv(&mut other).await,
        ServerMsg::Rejected {
            request: 3,
            reason: Rejection::Offline
        }
    );
}

#[tokio::test]
async fn logging_in_again_replaces_the_old_connection() {
    let (url, state) = server().await;
    let key = DeviceKey::generate().unwrap();
    let mut old = login(&url, &key, "PC").await;
    let new = login(&url, &key, "PC").await;
    assert_eq!(old.id(), new.id());
    assert!(matches!(old.recv().await, Err(ClientError::Server(_))));
    wait_online(&state, 1).await;
}

/// A raw WebSocket, to send what a well-behaved client would not.
mod raw {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    pub type Ws = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    pub async fn connect(url: &str) -> Ws {
        let (ws, _) = tokio_tungstenite::connect_async(format!("{url}/v1/signal"))
            .await
            .unwrap();
        ws
    }

    pub async fn recv_text(ws: &mut Ws) -> Option<String> {
        while let Some(msg) = ws.next().await {
            match msg.ok()? {
                Message::Text(text) => return Some(text.to_string()),
                Message::Close(_) => return None,
                _ => {}
            }
        }
        None
    }

    pub async fn send_text(ws: &mut Ws, text: String) {
        let _ = ws.send(Message::text(text)).await;
    }
}

#[tokio::test]
async fn a_wrong_signature_is_refused() {
    let (url, state) = server().await;
    let mut ws = raw::connect(&url).await;
    let challenge = raw::recv_text(&mut ws).await.unwrap();
    let ServerMsg::Challenge { nonce, .. } = serde_json::from_str(&challenge).unwrap() else {
        panic!("expected a challenge");
    };
    let key = DeviceKey::generate().unwrap();
    // Signed some other challenge: a replayed login.
    let mut other = nonce;
    other[0] ^= 1;
    let hello = ClientMsg::Hello {
        protocol: PROTOCOL,
        public_key: key.public_key(),
        signature: key.sign(&hext_signal::auth_message(&other)),
        device_name: "X".into(),
        app_version: "0".into(),
        accepts_requests: true,
        notices: true,
    };
    raw::send_text(&mut ws, hello.to_json()).await;
    let reply: ServerMsg = serde_json::from_str(&raw::recv_text(&mut ws).await.unwrap()).unwrap();
    assert!(matches!(reply, ServerMsg::Error { .. }), "{reply:?}");
    assert!(raw::recv_text(&mut ws).await.is_none(), "connection closed");
    assert_eq!(state.online(), 0);
}

#[tokio::test]
async fn messages_before_login_close_the_connection() {
    let (url, state) = server().await;
    let mut ws = raw::connect(&url).await;
    raw::recv_text(&mut ws).await.unwrap(); // the challenge
    raw::send_text(
        &mut ws,
        ClientMsg::Connect {
            request: 1,
            to: DeviceId::new(482_913_557).unwrap(),
            unattended: false,
            remember: false,
            account: None,
        }
        .to_json(),
    )
    .await;
    assert!(raw::recv_text(&mut ws).await.is_none());
    raw::send_text(&mut ws, "{".into()).await;
    assert_eq!(state.online(), 0);
}

#[tokio::test]
async fn health() {
    let (url, _) = server().await;
    let addr = url.trim_start_matches("ws://");
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("ok"));
}

#[tokio::test]
async fn unattended_requests_reach_the_host_marked_and_leave_no_trust() {
    let (url, _) = server().await;
    let mut host = login(&url, &DeviceKey::generate().unwrap(), "Host").await;
    let mut viewer = login(&url, &DeviceKey::generate().unwrap(), "Técnico").await;
    let to = host.id();

    // A viewer past the host with a request of this kind has shown nothing
    // of the password: it keeps being limited like a stranger.
    for request in 1..=3 {
        viewer
            .send(&ClientMsg::Connect {
                request,
                to,
                unattended: true,
                remember: false,
                account: None,
            })
            .await
            .unwrap();
        let ServerMsg::Request {
            request: asked,
            unattended,
            ..
        } = recv(&mut host).await
        else {
            panic!("expected a request");
        };
        assert!(unattended);
        host.send(&ClientMsg::Accept {
            request: asked,
            addrs: Vec::new(),
            session_code: "s3cr3t".into(),
        })
        .await
        .unwrap();
        assert!(matches!(
            recv(&mut viewer).await,
            ServerMsg::Accepted { .. }
        ));
    }
    viewer
        .send(&ClientMsg::Connect {
            request: 4,
            to,
            unattended: true,
            remember: false,
            account: None,
        })
        .await
        .unwrap();
    assert_eq!(
        recv(&mut viewer).await,
        ServerMsg::Rejected {
            request: 4,
            reason: Rejection::TooManyRequests
        }
    );
}

/// Asks `host` from `viewer` with the password and has the host let it
/// through, as it does with nobody to ask.
async fn with_password(viewer: &mut Client, host: &mut Client, request: u64) {
    send_with_password(viewer, host.id(), request).await;
    let ServerMsg::Request { request: asked, .. } = recv(host).await else {
        panic!("expected a request");
    };
    host.send(&ClientMsg::Accept {
        request: asked,
        addrs: Vec::new(),
        session_code: "s3cr3t".into(),
    })
    .await
    .unwrap();
    assert!(matches!(recv(viewer).await, ServerMsg::Accepted { .. }));
}

async fn send_with_password(viewer: &mut Client, to: DeviceId, request: u64) {
    viewer
        .send(&ClientMsg::Connect {
            request,
            to,
            unattended: true,
            remember: false,
            account: None,
        })
        .await
        .unwrap();
}

/// The same as [`refused`], with the password.
async fn refused_with_password(viewer: &mut Client, to: DeviceId, request: u64) -> Rejection {
    refused_as(viewer, to, request, true).await
}

/// The host says how `viewer`'s attempt went; back once the server took it
/// in. The audit is written in the background, so its entries are counted
/// by name.
async fn report(host: &mut Client, viewer: DeviceId, entered: bool, state: &State) {
    let to = host.id();
    let event = if entered { "entered" } else { "wrong_password" };
    let count = || {
        let events = state.db.audit_of(to).unwrap();
        events.iter().filter(|(_, e)| e == event).count()
    };
    let before = count();
    host.send(&ClientMsg::Attempt { viewer, entered })
        .await
        .unwrap();
    for _ in 0..200 {
        if count() > before {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the server never took {event} in");
}

/// A host and a technician.
async fn pair(url: &str) -> (Client, Client) {
    let host = login(url, &DeviceKey::generate().unwrap(), "Host").await;
    let viewer = login(url, &DeviceKey::generate().unwrap(), "Técnico").await;
    (host, viewer)
}

#[tokio::test]
async fn who_got_in_with_the_password_comes_back_as_often_as_it_takes() {
    let (url, state) = server().await;
    let (mut host, mut viewer) = pair(&url).await;
    let id = viewer.id();

    // Well past the 3 a stranger gets, each one as after a drop or a restart.
    for request in 1..=8 {
        with_password(&mut viewer, &mut host, request).await;
        report(&mut host, id, true, &state).await;
    }
}

#[tokio::test]
async fn wrong_passwords_never_get_past_the_limit_of_strangers() {
    let (url, state) = server().await;
    let (mut host, mut viewer) = pair(&url).await;
    let (id, to) = (viewer.id(), host.id());

    for request in 1..=3 {
        with_password(&mut viewer, &mut host, request).await;
        report(&mut host, id, false, &state).await;
    }
    assert_eq!(
        refused_with_password(&mut viewer, to, 4).await,
        Rejection::TooManyRequests
    );
    assert!(!state.db.entered_within(to, id, 3600).unwrap());
}

#[tokio::test]
async fn the_first_wrong_password_ends_the_trust() {
    let (url, state) = server().await;
    let (mut host, mut viewer) = pair(&url).await;
    let (id, to) = (viewer.id(), host.id());

    with_password(&mut viewer, &mut host, 1).await;
    report(&mut host, id, true, &state).await;
    // The password changed, or the key was stolen: from here on, a stranger.
    with_password(&mut viewer, &mut host, 2).await;
    report(&mut host, id, false, &state).await;
    assert!(!state.db.entered_within(to, id, 3600).unwrap());
    // (The first request is on the count already.)
    with_password(&mut viewer, &mut host, 3).await;
    with_password(&mut viewer, &mut host, 4).await;
    assert_eq!(
        refused_with_password(&mut viewer, to, 5).await,
        Rejection::TooManyRequests
    );
    // The right password puts it back.
    report(&mut host, id, true, &state).await;
    with_password(&mut viewer, &mut host, 6).await;
}

#[tokio::test]
async fn a_wrong_password_reported_late_still_ends_the_trust_on_the_record() {
    let (url, state) = server().await;
    let (mut host, mut viewer) = pair(&url).await;
    let (id, to) = (viewer.id(), host.id());

    with_password(&mut viewer, &mut host, 1).await;
    report(&mut host, id, true, &state).await;
    // No request of it open: the host was away from the server meanwhile.
    report(&mut host, id, false, &state).await;
    assert!(!state.db.entered_within(to, id, 3600).unwrap());
}

#[tokio::test]
async fn a_viewer_cannot_vouch_for_itself() {
    let (url, state) = server().await;
    let (mut host, mut viewer) = pair(&url).await;
    let mut friend = login(&url, &DeviceKey::generate().unwrap(), "Amigo").await;
    let (id, to, friend_id) = (viewer.id(), host.id(), friend.id());

    for request in 1..=3 {
        with_password(&mut viewer, &mut host, request).await;
        // In its own name, or in that of a device that never heard of it.
        let entered = ClientMsg::Attempt {
            viewer: id,
            entered: true,
        };
        viewer.send(&entered).await.unwrap();
        friend.send(&entered).await.unwrap();
        // Back once the server read it.
        assert_eq!(
            refused(&mut friend, friend_id, request).await,
            Rejection::SelfConnect
        );
    }
    assert_eq!(
        refused_with_password(&mut viewer, to, 4).await,
        Rejection::TooManyRequests
    );
    // Not even the host's word counts without a request it let through.
    let mut other = login(&url, &DeviceKey::generate().unwrap(), "Outro").await;
    let other_id = other.id();
    host.send(&ClientMsg::Attempt {
        viewer: other_id,
        entered: true,
    })
    .await
    .unwrap();
    with_password(&mut other, &mut host, 1).await;
    assert!(!state.db.entered_within(to, other_id, 3600).unwrap());
}

#[tokio::test]
async fn being_accepted_is_not_getting_in_with_the_password() {
    let (url, state) = server().await;
    let (mut host, mut viewer) = pair(&url).await;
    let (id, to) = (viewer.id(), host.id());

    // Answered by a person: there is no password to say anything about.
    ask(&mut viewer, &mut host, 1, true).await;
    host.send(&ClientMsg::Attempt {
        viewer: id,
        entered: true,
    })
    .await
    .unwrap();
    with_password(&mut viewer, &mut host, 2).await;
    assert!(!state.db.entered_within(to, id, 3600).unwrap());
}

#[tokio::test]
async fn getting_in_with_the_password_does_not_open_the_way_to_bother_people() {
    let (url, state) = server().await;
    let (mut host, mut viewer) = pair(&url).await;
    let (id, to) = (viewer.id(), host.id());

    with_password(&mut viewer, &mut host, 1).await;
    report(&mut host, id, true, &state).await;
    for request in 2..=3 {
        ask(&mut viewer, &mut host, request, false).await;
    }
    assert_eq!(
        refused(&mut viewer, to, 4).await,
        Rejection::TooManyRequests
    );
    with_password(&mut viewer, &mut host, 5).await;
}

#[tokio::test]
async fn a_flood_does_not_lock_out_who_got_in_with_the_password() {
    let (url, state) = server().await;
    let (mut host, mut viewer) = pair(&url).await;
    let (id, to) = (viewer.id(), host.id());
    with_password(&mut viewer, &mut host, 1).await;
    report(&mut host, id, true, &state).await;

    // 1 + 3 + 3 + 3 = 10 on the host's count; the next stranger closes it.
    let mut strangers = Vec::new();
    for name in ["A", "B", "C", "D"] {
        strangers.push(login(&url, &DeviceKey::generate().unwrap(), name).await);
    }
    for stranger in &mut strangers[..3] {
        for request in 0..3 {
            ask(stranger, &mut host, request, false).await;
        }
    }
    assert_eq!(
        refused_with_password(&mut strangers[3], to, 0).await,
        Rejection::TooManyRequests
    );
    with_password(&mut viewer, &mut host, 2).await;
}

#[tokio::test]
async fn the_server_says_it_takes_attempts() {
    let (url, _) = server().await;
    let client = login(&url, &DeviceKey::generate().unwrap(), "PC").await;
    assert!(client.takes_attempts());
}

#[tokio::test]
async fn a_host_without_unattended_access_says_so() {
    let (url, _) = server().await;
    let mut host = login(&url, &DeviceKey::generate().unwrap(), "Host").await;
    let mut viewer = login(&url, &DeviceKey::generate().unwrap(), "Técnico").await;
    viewer
        .send(&ClientMsg::Connect {
            request: 1,
            to: host.id(),
            unattended: true,
            remember: false,
            account: None,
        })
        .await
        .unwrap();
    let ServerMsg::Request { request, .. } = recv(&mut host).await else {
        panic!("expected a request");
    };
    host.send(&ClientMsg::NoUnattended { request })
        .await
        .unwrap();
    assert_eq!(
        recv(&mut viewer).await,
        ServerMsg::Rejected {
            request: 1,
            reason: Rejection::UnattendedOff
        }
    );
}

#[tokio::test]
async fn a_locked_host_tells_the_viewer_while_it_waits() {
    let (url, _) = server().await;
    let mut host = login(&url, &DeviceKey::generate().unwrap(), "Host").await;
    let mut viewer = login(&url, &DeviceKey::generate().unwrap(), "Técnico").await;
    viewer
        .send(&ClientMsg::Connect {
            request: 1,
            to: host.id(),
            unattended: false,
            remember: false,
            account: None,
        })
        .await
        .unwrap();
    let ServerMsg::Request { request, .. } = recv(&mut host).await else {
        panic!("expected a request");
    };
    // Only for the request of this host: another number goes nowhere.
    host.send(&ClientMsg::Waiting {
        request: request + 1,
    })
    .await
    .unwrap();
    host.send(&ClientMsg::Waiting { request }).await.unwrap();
    assert_eq!(recv(&mut viewer).await, ServerMsg::Waiting { request: 1 });
}

#[tokio::test]
async fn presence_shows_only_computers_that_let_this_one_in() {
    let (url, state) = server().await;
    let (mut host, mut viewer) = pair(&url).await;
    let mut stranger = login(&url, &DeviceKey::generate().unwrap(), "Outro").await;
    let mut entered = login(&url, &DeviceKey::generate().unwrap(), "Senha").await;
    let nobody: DeviceId = "123 456 789".parse().unwrap();
    let ids = [host.id(), stranger.id(), entered.id(), nobody, viewer.id()];
    assert!(viewer.takes_presence());

    // Online, but it never let this one in: like an unknown ID.
    assert_eq!(viewer.presence(&ids).await.unwrap(), vec![]);

    assert!(matches!(
        ask(&mut viewer, &mut host, 1, true).await,
        ServerMsg::Accepted { .. }
    ));
    let viewer_id = viewer.id();
    with_password(&mut viewer, &mut entered, 2).await;
    report(&mut entered, viewer_id, true, &state).await;
    // Declining does not count.
    assert!(matches!(
        ask(&mut viewer, &mut stranger, 3, false).await,
        ServerMsg::Rejected { .. }
    ));
    let mut expected = vec![host.id(), entered.id()];
    expected.sort_unstable_by_key(|id| id.get());
    assert_eq!(viewer.presence(&ids).await.unwrap(), expected);
    // With the names they logged in with (what `service name` set), and
    // only theirs.
    let (online, mut names) = viewer.presence_named(&ids).await.unwrap();
    assert_eq!(online, expected);
    names.sort_unstable_by_key(|named| named.id.get());
    let mut wanted = vec![
        hext_signal::HostName {
            id: host.id(),
            name: "Host".into(),
        },
        hext_signal::HostName {
            id: entered.id(),
            name: "Senha".into(),
        },
    ];
    wanted.sort_unstable_by_key(|named| named.id.get());
    assert_eq!(names, wanted);

    // Offline once it leaves.
    host.close().await.unwrap();
    wait_online(&state, 3).await;
    assert_eq!(viewer.presence(&ids).await.unwrap(), vec![entered.id()]);
}

#[tokio::test]
async fn presence_questions_are_limited() {
    let (url, _) = server().await;
    let (_host, mut viewer) = pair(&url).await;
    for _ in 0..6 {
        assert_eq!(viewer.presence(&[]).await.unwrap(), vec![]);
    }
    viewer
        .send(&ClientMsg::Presence { ids: vec![] })
        .await
        .unwrap();
    let answer = tokio::time::timeout(Duration::from_millis(300), viewer.recv()).await;
    assert!(answer.is_err(), "past the limit, no answer: {answer:?}");
}

#[tokio::test]
async fn an_ask_to_bind_to_an_account_goes_only_with_the_password() {
    let (url, _) = server().await;
    let mut host = login(&url, &DeviceKey::generate().unwrap(), "Host").await;
    let viewer_key = DeviceKey::generate().unwrap();
    let mut viewer = login(&url, &viewer_key, "Técnico").await;
    let to = host.id();
    let ask = hext_signal::chain::BindAsk::new(&viewer_key, [3; 32], to);
    viewer
        .send(&ClientMsg::Connect {
            request: 1,
            to,
            unattended: true,
            remember: false,
            account: Some(ask.clone()),
        })
        .await
        .unwrap();
    let ServerMsg::Request {
        from_key, account, ..
    } = recv(&mut host).await
    else {
        panic!("expected a request");
    };
    // As the technician signed it: the host checks it with the key the
    // connection will prove.
    assert_eq!(account, Some(ask));
    assert!(account.unwrap().holds(&from_key, to));
}
