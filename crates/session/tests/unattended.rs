//! The unattended password, end to end on real connections: nobody accepts,
//! so the password is all that lets a viewer in.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hext_codec::{DecoderPreference, EncoderSettings};
use hext_crypto::DeviceKey;
use hext_crypto::pake::Verifier;
use hext_net::{Endpoint, EndpointConfig, NetError, PasswordLogin};
use hext_proto::{Hello, PeerInfo, Permissions, RejectReason};
use hext_session::host::{HostConfig, LocalDesktop, UnattendedAccess, UnattendedPassword};
use hext_session::{Grant, Lockout, SessionCodes, SessionError, client, host};
use zeroize::Zeroizing;

const PASSWORD: &str = "cavalo correto";

/// The password as a service keeps it; `None`: the access is off.
struct Kept {
    password: Option<&'static str>,
    audit: Mutex<Vec<String>>,
}

impl UnattendedPassword for Kept {
    fn verifier(&self) -> Option<Verifier> {
        self.password
            .map(|password| Verifier::new(password).unwrap())
    }

    fn audit(&self, entry: &str) {
        self.audit.lock().unwrap().push(entry.to_owned());
    }
}

struct Host {
    endpoint: Endpoint,
    codes: SessionCodes,
    lockout: Lockout,
    kept: Arc<Kept>,
    /// The viewers' endpoints: a session dies with its own.
    viewers: Mutex<Vec<Endpoint>>,
}

impl Host {
    /// A host that serves sessions until the test ends.
    async fn start(password: Option<&'static str>) -> Self {
        let key = DeviceKey::generate().unwrap();
        let accept = EndpointConfig {
            accept: true,
            ..Default::default()
        };
        let endpoint = hext_net::bind(&key, &accept).await.unwrap();
        let (codes, lockout) = (SessionCodes::default(), Lockout::default());
        let kept = Arc::new(Kept {
            password,
            audit: Mutex::default(),
        });
        let config = Arc::new(HostConfig {
            peer: PeerInfo::local("test", "host"),
            access_code: None,
            session_codes: codes.clone(),
            desktop: Arc::new(LocalDesktop {
                display: 0,
                test_pattern: true,
                encoder: EncoderSettings::default(),
            }),
            presence: None,
            unattended: Some(UnattendedAccess {
                key: key.public_key(),
                lockout: lockout.clone(),
                password: kept.clone(),
            }),
        });
        let accepting = endpoint.clone();
        tokio::spawn(async move {
            while let Some(incoming) = accepting.accept().await {
                let config = config.clone();
                tokio::spawn(async move {
                    if let Ok(conn) = incoming.await {
                        let _ = host::serve(conn, config).await;
                    }
                });
            }
        });
        Self {
            endpoint,
            codes,
            lockout,
            kept,
            viewers: Mutex::default(),
        }
    }

    /// Tries to open a session as `viewer`, which got a code from the server
    /// without anyone accepting.
    async fn attempt(
        &self,
        viewer: &DeviceKey,
        password: Option<&str>,
    ) -> hext_session::Result<client::ClientHandle> {
        let code = self.codes.issue(
            viewer.public_key(),
            Grant {
                permissions: Permissions::CONTROL,
                name: "Técnico".into(),
                id: "482 913 557".into(),
                needs_password: true,
            },
        );
        let endpoint = hext_net::bind(viewer, &EndpointConfig::default())
            .await
            .unwrap();
        self.viewers.lock().unwrap().push(endpoint.clone());
        let conn = hext_net::connect(&endpoint, self.endpoint.addr())
            .await
            .unwrap();
        let login = password.map(|password| PasswordLogin {
            password: Zeroizing::new(password.to_owned()),
            viewer: viewer.public_key(),
        });
        client::start(
            conn,
            Hello {
                peer: PeerInfo::local("test", "viewer"),
                access_code: code,
            },
            login,
            DecoderPreference::Software,
            |_| {},
        )
        .await
    }

    /// The audit, once it has `lines` lines: the host finishes up after the
    /// viewer has gone.
    async fn audit(&self, lines: usize) -> Vec<String> {
        for _ in 0..500 {
            let audit = self.kept.audit.lock().unwrap().clone();
            if audit.len() >= lines {
                return audit;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the audit never got {lines} lines");
    }
}

fn rejected(result: hext_session::Result<client::ClientHandle>) -> Option<RejectReason> {
    match result {
        Err(SessionError::Net(NetError::Rejected(reason))) => Some(reason),
        Err(e) => panic!("unexpected error: {e}"),
        Ok(_) => None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_right_password_opens_a_session_and_it_is_audited() {
    let host = Host::start(Some(PASSWORD)).await;
    let viewer = DeviceKey::generate().unwrap();
    let session = host.attempt(&viewer, Some(PASSWORD)).await.unwrap();
    assert_eq!(session.permissions, Permissions::CONTROL);
    session.close().await.unwrap();

    let audit = host.audit(2).await;
    assert!(audit[0].starts_with("sessão iniciada"), "{audit:?}");
    assert!(audit[0].contains("482 913 557"), "{audit:?}");
    assert!(audit[1].starts_with("sessão encerrada"), "{audit:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_password_stays_out_and_counts() {
    let host = Host::start(Some(PASSWORD)).await;
    let viewer = DeviceKey::generate().unwrap();
    assert_eq!(
        rejected(host.attempt(&viewer, Some("cavalo errado")).await),
        Some(RejectReason::WrongPassword)
    );
    assert!(host.audit(1).await[0].contains("WrongPassword"));

    for _ in 0..4 {
        host.lockout.failed(&viewer.public_key());
    }
    // The wait now holds, and it comes before any exchange.
    let Some(RejectReason::LockedOut { seconds }) =
        rejected(host.attempt(&viewer, Some(PASSWORD)).await)
    else {
        panic!("expected to wait");
    };
    assert!(seconds > 0 && seconds <= 60, "{seconds}");
    // Another technician is not held up by it.
    let other = DeviceKey::generate().unwrap();
    host.attempt(&other, Some(PASSWORD)).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_password_the_viewer_stays_out() {
    let host = Host::start(Some(PASSWORD)).await;
    let viewer = DeviceKey::generate().unwrap();
    let result = host.attempt(&viewer, None).await;
    assert!(matches!(
        result,
        Err(SessionError::Net(NetError::PasswordNeeded))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn with_the_access_off_no_password_gets_in() {
    let host = Host::start(None).await;
    let viewer = DeviceKey::generate().unwrap();
    assert_eq!(
        rejected(host.attempt(&viewer, Some(PASSWORD)).await),
        Some(RejectReason::Denied)
    );
}
