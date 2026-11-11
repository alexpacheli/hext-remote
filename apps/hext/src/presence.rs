//! Whether the computers on the home screen's list are online: asked of the
//! server every little while, only while the list is on screen. The server
//! answers only for computers that let this one in lately (see
//! `ServerMsg::Presence`), which are the ones on the list.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hext_signal::{Client, DeviceId, HostName};
use tokio::sync::Notify;
use tracing::{info, warn};

/// How often to ask while the list is on screen (the server takes up to 6 a
/// minute).
const EVERY: Duration = Duration::from_secs(30);
/// How long to wait after the server could not be reached.
const RETRY: Duration = Duration::from_secs(60);
/// The least time between two questions, however often the list changes:
/// within the server's limit, past which it stops answering.
const GAP: Duration = Duration::from_secs(10);

/// Asks about `wanted()` (nothing: the list is not on screen, and the
/// connection closes) and gives the answer to `show` (`None`: unknown):
/// who is online, and the names they go by now.
/// `wake` asks again at once: the list came on screen or changed. Logs in
/// with the viewer's key, `key`: the one those computers know.
pub async fn watch(
    key: PathBuf,
    wanted: impl Fn() -> Vec<DeviceId>,
    show: impl Fn(Option<(Vec<DeviceId>, Vec<HostName>)>),
    wake: Arc<Notify>,
) {
    let mut client: Option<Client> = None;
    let mut asked: Option<Instant> = None;
    loop {
        let ids = wanted();
        let mut wait = EVERY;
        if ids.is_empty() {
            if let Some(client) = client.take() {
                let _ = client.close().await;
            }
            show(None);
        } else {
            if client.is_none() {
                match login(&key).await {
                    Ok(new) if new.takes_presence() => client = Some(new),
                    Ok(old) => {
                        // Until the app opens again: servers do not change
                        // that often.
                        info!("o servidor de sinalização não diz quem está online");
                        let _ = old.close().await;
                        show(None);
                        return;
                    }
                    Err(e) => {
                        warn!("quem está online: {e:#}");
                        wait = RETRY;
                    }
                }
            }
            if let Some(connected) = &mut client {
                if let Some(since) = asked.map(|at| at.elapsed()) {
                    tokio::time::sleep(GAP.saturating_sub(since)).await;
                }
                asked = Some(Instant::now());
                match connected.presence_named(&ids).await {
                    Ok(online) => show(Some(online)),
                    Err(e) => {
                        warn!("quem está online: {e}");
                        client = None;
                        show(None);
                        wait = RETRY;
                    }
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = wake.notified() => {}
        }
    }
}

async fn login(key: &std::path::Path) -> anyhow::Result<Client> {
    let key = crate::device_key(Some(key))?;
    crate::signaling::login(crate::DEFAULT_SERVER, &key, &crate::device_name()).await
}
