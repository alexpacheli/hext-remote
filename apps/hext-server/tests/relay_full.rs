//! The relay without the per-device limit: who is told to use it, and who
//! it lets in.

use std::sync::Arc;
use std::time::Duration;

use hext_crypto::DeviceKey;
use hext_server::relay_full::RelayFull;
use hext_server::{Config, Db, State};
use hext_signal::{Client, ClientMsg, RelayHint, Role, ServerMsg};

const TOKEN: &str = "s3cr3t";

/// A server with the relay without the limit, for the accounts in `list`.
async fn server(list: &std::path::Path) -> (String, Arc<State>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let mut state = State::new(Db::in_memory().unwrap());
    state.set_relay_full(RelayFull {
        url: "https://relay-full.example.org".into(),
        quic_port: 7843,
        accounts: list.to_owned(),
    });
    let state = Arc::new(state);
    let config = Config {
        relay_token: Some(TOKEN.into()),
        ..Default::default()
    };
    tokio::spawn(hext_server::serve(listener, state.clone(), config));
    (url, state)
}

/// The relay's question about `key`: the status code.
async fn may_use(url: &str, key: &DeviceKey) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let request = format!(
        "POST /v1/relay-access-full HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\
         Authorization: Bearer {TOKEN}\r\nX-Iroh-Endpoint-Id: {}\r\nContent-Length: 0\r\n\r\n",
        key.public_key()
    );
    let mut stream = tokio::net::TcpStream::connect(url.trim_start_matches("ws://"))
        .await
        .unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response[9..12].parse().unwrap()
}

async fn recv(client: &mut Client) -> ServerMsg {
    tokio::time::timeout(Duration::from_secs(5), client.recv())
        .await
        .expect("a message in time")
        .unwrap()
}

#[tokio::test]
async fn a_listed_account_gets_the_relay_and_whoever_it_lets_in_too() {
    let dir = std::env::temp_dir().join(format!("hext-relay-full-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let list = dir.join("relay-livre.txt");
    std::fs::write(&list, "# contas\nVIP@exemplo.com\n").unwrap();
    let (url, state) = server(&list).await;

    // A computer of the listed account (one of its devices, here), and a
    // technician from nowhere.
    let vip = DeviceKey::generate().unwrap();
    let other = DeviceKey::generate().unwrap();
    // Another device of the account, not in the session.
    let vip_too = DeviceKey::generate().unwrap();
    let hex = |key: &DeviceKey| -> String {
        key.public_key()
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    let (hex, hex_too) = (hex(&vip), hex(&vip_too));
    state
        .db
        .tamper(&format!(
            "INSERT INTO accounts (email, password, verified, created_at) VALUES ('vip@exemplo.com', 'x', 1, 0);
             INSERT INTO account_members (account_id, public_key) VALUES (1, X'{hex}'), (1, X'{hex_too}');"
        ))
        .unwrap();

    let mut host = Client::connect(&url, &vip, "Servidor", "0.1.0", Role::Host)
        .await
        .unwrap();
    let mut viewer = Client::connect(&url, &other, "Técnico", "0.1.0", Role::Viewer)
        .await
        .unwrap();
    assert_eq!(
        host.relay(),
        Some(&RelayHint {
            url: "https://relay-full.example.org".into(),
            quic_port: 7843,
        })
    );
    assert_eq!(viewer.relay(), None);
    assert_eq!(may_use(&url, &vip).await, 200);
    assert_eq!(may_use(&url, &other).await, 403);
    assert_eq!(may_use(&url, &vip_too).await, 200);

    // Once the listed computer lets the technician in, the technician may
    // use that relay too (it is where the computer is).
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
    host.send(&ClientMsg::Accept {
        request,
        addrs: vec![],
        session_code: "c0de".into(),
    })
    .await
    .unwrap();
    assert!(matches!(
        recv(&mut viewer).await,
        ServerMsg::Accepted { .. }
    ));
    assert_eq!(may_use(&url, &other).await, 200);

    // Off the list: no more, from the next check on (but for the two in
    // the session, for its first minutes).
    std::fs::write(&list, "# ninguém\n").unwrap();
    assert_eq!(may_use(&url, &vip_too).await, 403);
    let again = Client::connect(&url, &vip, "Servidor", "0.1.0", Role::Viewer)
        .await
        .unwrap();
    assert_eq!(again.relay(), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A viewer with `key` asks `host` with the password, and the host lets the
/// request through (as it does by itself).
async fn with_password(url: &str, host: &mut Client, key: &DeviceKey) -> Client {
    let mut viewer = Client::connect(url, key, "Técnico", "0.1.0", Role::Viewer)
        .await
        .unwrap();
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
    let ServerMsg::Request { request, .. } = recv(host).await else {
        panic!("expected a request");
    };
    host.send(&ClientMsg::Accept {
        request,
        addrs: vec![],
        session_code: "c0de".into(),
    })
    .await
    .unwrap();
    assert!(matches!(
        recv(&mut viewer).await,
        ServerMsg::Accepted { .. }
    ));
    viewer
}

#[tokio::test]
async fn with_the_password_a_stranger_gets_the_relay_only_once_it_got_in() {
    let dir = std::env::temp_dir().join(format!("hext-relay-full-pw-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let list = dir.join("relay-livre.txt");
    std::fs::write(&list, "vip@exemplo.com\n").unwrap();
    let (url, state) = server(&list).await;
    let vip = DeviceKey::generate().unwrap();
    let hex: String = vip
        .public_key()
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    state
        .db
        .tamper(&format!(
            "INSERT INTO accounts (email, password, verified, created_at) VALUES ('vip@exemplo.com', 'x', 1, 0);
             INSERT INTO account_members (account_id, public_key) VALUES (1, X'{hex}');"
        ))
        .unwrap();
    let mut host = Client::connect(&url, &vip, "Servidor", "0.1.0", Role::Host)
        .await
        .unwrap();

    // Two strangers, each with a request with the password the listed
    // computer takes by itself: neither may use the relay (together they
    // would have a tunnel of their own) before the computer says it got in.
    let (one, two) = (
        DeviceKey::generate().unwrap(),
        DeviceKey::generate().unwrap(),
    );
    let first = with_password(&url, &mut host, &one).await;
    let second = with_password(&url, &mut host, &two).await;
    assert_eq!(may_use(&url, &one).await, 403);
    assert_eq!(may_use(&url, &two).await, 403);

    // A wrong password: still not. The right one: in.
    host.send(&ClientMsg::Attempt {
        viewer: second.id(),
        entered: false,
    })
    .await
    .unwrap();
    host.send(&ClientMsg::Attempt {
        viewer: first.id(),
        entered: true,
    })
    .await
    .unwrap();
    let mut admitted = false;
    for _ in 0..50 {
        if may_use(&url, &one).await == 200 {
            admitted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(admitted);
    assert_eq!(may_use(&url, &two).await, 403);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn without_the_relay_its_question_is_not_answered() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let state = Arc::new(State::new(Db::in_memory().unwrap()));
    let config = Config {
        relay_token: Some(TOKEN.into()),
        ..Default::default()
    };
    tokio::spawn(hext_server::serve(listener, state, config));
    let key = DeviceKey::generate().unwrap();
    let client = Client::connect(&url, &key, "PC", "0.1.0", Role::Viewer)
        .await
        .unwrap();
    assert_eq!(client.relay(), None);
    assert_eq!(may_use(&url, &key).await, 404);
}
