//! What the host's user decides holds on the host: the permissions granted
//! and the button that ends a session.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hext_codec::DecoderPreference;
use hext_crypto::DeviceKey;
use hext_net::EndpointConfig;
use hext_proto::{Hello, InputEvent, PeerInfo, Permissions, VideoFrame};
use hext_session::host::{ActiveSession, BoxFuture, Desktop, HostConfig, Presence, Screen};
use hext_session::{Grant, SessionCodes, client, host};
use tokio::sync::{Notify, mpsc};

/// A desktop that only counts the input reaching it.
#[derive(Default)]
struct Counting {
    injected: Arc<AtomicU32>,
}

impl Desktop for Counting {
    fn open(
        &self,
        frames: mpsc::Sender<VideoFrame>,
    ) -> BoxFuture<'_, hext_session::Result<Box<dyn Screen>>> {
        Box::pin(async move {
            Ok(Box::new(CountingScreen {
                _frames: frames,
                injected: self.injected.clone(),
            }) as Box<dyn Screen>)
        })
    }

    fn bitrate_bps(&self) -> u32 {
        1_000_000
    }
}

struct CountingScreen {
    /// Open while the session runs: a closed one ends it.
    _frames: mpsc::Sender<VideoFrame>,
    injected: Arc<AtomicU32>,
}

impl Screen for CountingScreen {
    fn request_keyframe(&self) {}

    fn inject(&mut self, _event: &InputEvent) {
        self.injected.fetch_add(1, Ordering::SeqCst);
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
}

/// An indicator keeping what it was told.
#[derive(Default)]
struct Indicator {
    shown: Mutex<Option<ActiveSession>>,
    end: Mutex<Option<Arc<Notify>>>,
    ended: AtomicU32,
}

impl Presence for Indicator {
    fn started(&self, session: &ActiveSession) -> Arc<Notify> {
        *self.shown.lock().unwrap() = Some(session.clone());
        let end = Arc::new(Notify::new());
        *self.end.lock().unwrap() = Some(end.clone());
        end
    }

    fn ended(&self) {
        self.ended.fetch_add(1, Ordering::SeqCst);
    }
}

struct Running {
    session: client::ClientHandle,
    injected: Arc<AtomicU32>,
    indicator: Arc<Indicator>,
    host: tokio::task::JoinHandle<hext_session::Result<PeerInfo>>,
    _endpoints: (hext_net::Endpoint, hext_net::Endpoint),
}

/// A session accepted by the host's user with `permissions`.
async fn session_with(permissions: Permissions) -> Running {
    let accept = EndpointConfig {
        accept: true,
        ..Default::default()
    };
    let host_endpoint = hext_net::bind(&DeviceKey::generate().unwrap(), &accept)
        .await
        .unwrap();
    let addr = host_endpoint.addr();
    let viewer_key = DeviceKey::generate().unwrap();
    let codes = SessionCodes::default();
    let code = codes.issue(
        viewer_key.public_key(),
        Grant {
            permissions,
            name: "Técnico".into(),
            id: "482 913 557".into(),
        },
    );
    let desktop = Counting::default();
    let injected = desktop.injected.clone();
    let indicator = Arc::new(Indicator::default());
    let config = Arc::new(HostConfig {
        peer: PeerInfo::local("test", "host"),
        access_code: None,
        session_codes: codes,
        desktop: Arc::new(desktop),
        presence: Some(indicator.clone()),
    });
    let accepting = host_endpoint.clone();
    let host = tokio::spawn(async move {
        let conn = accepting.accept().await.unwrap().await.unwrap();
        host::serve(conn, config).await
    });
    let viewer_endpoint = hext_net::bind(&viewer_key, &EndpointConfig::default())
        .await
        .unwrap();
    let conn = hext_net::connect(&viewer_endpoint, addr).await.unwrap();
    let session = client::start(
        conn,
        Hello {
            peer: PeerInfo::local("test", "viewer"),
            access_code: code,
        },
        DecoderPreference::Software,
        |_| {},
    )
    .await
    .unwrap();
    Running {
        session,
        injected,
        indicator,
        host,
        _endpoints: (host_endpoint, viewer_endpoint),
    }
}

/// Sends every kind of input a viewer could try; returns how much of it
/// reached the desktop.
async fn try_input(running: &Running) -> u32 {
    for event in [
        InputEvent::MouseMove { x: 0.5, y: 0.5 },
        InputEvent::Key {
            scancode: 0x1E,
            pressed: true,
        },
        InputEvent::SecureAttention,
    ] {
        running.session.send_input(event);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    running.injected.load(Ordering::SeqCst)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_viewer_learns_its_permissions() {
    let granted = Permissions {
        clipboard: true,
        ..Permissions::CONTROL
    };
    let running = session_with(granted).await;
    assert_eq!(running.session.permissions, granted);
    let shown = running.indicator.shown.lock().unwrap().clone().unwrap();
    assert_eq!(shown.name, "Técnico");
    assert_eq!(shown.id.as_deref(), Some("482 913 557"));
    assert_eq!(shown.permissions, granted);
}

#[tokio::test(flavor = "multi_thread")]
async fn control_needs_the_permission() {
    let allowed = session_with(Permissions::CONTROL).await;
    assert_eq!(try_input(&allowed).await, 3);

    // Even from a viewer that ignores what it was told: this one sends
    // input all the same.
    let refused = session_with(Permissions::VIEW_ONLY).await;
    assert_eq!(try_input(&refused).await, 0);
    assert!(!refused.session.is_finished(), "the session goes on");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_host_ends_the_session() {
    let running = session_with(Permissions::CONTROL).await;
    assert_eq!(running.indicator.ended.load(Ordering::SeqCst), 0);
    let end = running.indicator.end.lock().unwrap().clone().unwrap();
    end.notify_one();

    tokio::time::timeout(Duration::from_secs(5), running.host)
        .await
        .expect("the session ends")
        .unwrap()
        .unwrap();
    assert_eq!(running.indicator.ended.load(Ordering::SeqCst), 1);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !running.session.is_finished() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the viewer was told"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
