//! Between whoever owns the sessions and the interface the user sees: the
//! service and a process of the user (a local pipe), or this process when
//! portable (the same messages, in memory).
//!
//! The interface is not trusted with more than it decides: it answers
//! requests and ends sessions, and it may be replaced at any time (a logoff,
//! a crash), so the [`UiHub`] keeps what a new one must show.

use std::future::Future;
use std::sync::{Arc, Mutex};

use hext_proto::Permissions;
use hext_session::host::{ActiveSession, BoxFuture, Presence};
use hext_signal::DeviceId;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Notify, mpsc, oneshot};
use tracing::warn;

use crate::agent::{Task, read_msg, write_msg};
use crate::signaling::{ANSWER_TIME, Ask};

/// Printed by a viewer opened by the interface (`--report-to-ui`) once it
/// is connected.
pub const VIEWER_CONNECTED: &str = "hext-ui: conectado";

/// Printed by a viewer asking a computer whose screen is locked (the
/// interface shows it, too).
pub const HOST_LOCKED: &str = "O computador está bloqueado: peça para alguém acordar a tela.";

/// Owner of the sessions → interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToUi {
    /// This computer's ID.
    Id(String),
    /// Something the user should know (a permission missing, for example).
    Notice(String),
    /// Someone asks for access; the answer is [`FromUi::Answer`].
    Ask {
        ask: u64,
        name: String,
        id: String,
        /// How long the user has to answer.
        seconds: u64,
    },
    /// The request is over (cancelled, or time ran out).
    AskOver {
        ask: u64,
    },
    SessionStarted(SessionView),
    SessionEnded,
}

/// Interface → owner of the sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FromUi {
    /// `None`: refused.
    Answer {
        ask: u64,
        permissions: Option<Permissions>,
    },
    /// The user pressed Encerrar.
    EndSession,
}

/// The session going on, as shown on the indicator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionView {
    pub name: String,
    pub id: Option<String>,
    pub permissions: Permissions,
    pub unattended: bool,
}

/// The owner's end: the interface connected now (at most one), and what it
/// must show. Cheap to clone (shared).
#[derive(Clone, Default)]
pub struct UiHub(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    /// The interface connected now, by connection number.
    ui: Option<(u64, mpsc::UnboundedSender<ToUi>)>,
    next_ui: u64,
    id: Option<String>,
    notice: Option<String>,
    session: Option<(SessionView, Arc<Notify>)>,
    /// The request on screen, waiting for its answer.
    asking: Option<(u64, oneshot::Sender<Option<Permissions>>)>,
    next_ask: u64,
}

impl State {
    fn send(&self, msg: ToUi) {
        if let Some((_, ui)) = &self.ui {
            let _ = ui.send(msg);
        }
    }
}

impl UiHub {
    /// Serves an interface connected through `pipe` until it disconnects;
    /// it replaces the one before, if any.
    pub async fn attach<P: AsyncRead + AsyncWrite + Send + 'static>(&self, pipe: P) {
        let (mut reader, mut writer) = tokio::io::split(pipe);
        let (tx, mut rx) = mpsc::unbounded_channel::<ToUi>();
        let number = {
            let mut state = self.0.lock().expect("lock");
            let number = state.next_ui;
            state.next_ui += 1;
            // A question on the interface being replaced is left unanswered:
            // that is a no.
            state.asking = None;
            // What the new interface must show.
            if let Some(id) = &state.id {
                let _ = tx.send(ToUi::Id(id.clone()));
            }
            if let Some(notice) = &state.notice {
                let _ = tx.send(ToUi::Notice(notice.clone()));
            }
            if let Some((session, _)) = &state.session {
                let _ = tx.send(ToUi::SessionStarted(session.clone()));
            }
            state.ui = Some((number, tx));
            number
        };
        // Forgets this interface however this ends (also when cancelled).
        let _attached = Attached {
            hub: self.clone(),
            number,
        };
        let _writing = Task(tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if write_msg(&mut writer, &msg).await.is_err() {
                    break;
                }
            }
        }));
        loop {
            match read_msg::<_, FromUi>(&mut reader).await {
                Ok(Some(msg)) => self.received(msg),
                Ok(None) => break,
                Err(e) => {
                    warn!("interface: {e}");
                    break;
                }
            }
        }
    }

    fn received(&self, msg: FromUi) {
        let mut state = self.0.lock().expect("lock");
        match msg {
            FromUi::Answer { ask, permissions } => {
                if state.asking.as_ref().is_some_and(|(a, _)| *a == ask)
                    && let Some((_, answer)) = state.asking.take()
                {
                    let _ = answer.send(permissions);
                }
            }
            FromUi::EndSession => {
                if let Some((_, end)) = &state.session {
                    end.notify_one();
                }
            }
        }
    }

    /// Ends the session going on, as if the user pressed Encerrar.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn end_session(&self) {
        self.received(FromUi::EndSession);
    }

    /// Whether an interface is connected.
    #[cfg_attr(not(any(windows, test)), allow(dead_code))]
    pub fn connected(&self) -> bool {
        self.0.lock().expect("lock").ui.is_some()
    }

    pub fn set_id(&self, id: DeviceId) {
        let mut state = self.0.lock().expect("lock");
        let id = id.to_string();
        state.id = Some(id.clone());
        state.send(ToUi::Id(id));
    }

    pub fn set_notice(&self, notice: String) {
        let mut state = self.0.lock().expect("lock");
        state.notice = Some(notice.clone());
        state.send(ToUi::Notice(notice));
    }

    /// Puts a request on the interface; `None` if there is none connected.
    pub fn ask(&self, name: &str, id: DeviceId) -> Option<Answer> {
        let mut state = self.0.lock().expect("lock");
        state.ui.as_ref()?;
        let ask = state.next_ask;
        state.next_ask += 1;
        let (tx, rx) = oneshot::channel();
        state.asking = Some((ask, tx));
        state.send(ToUi::Ask {
            ask,
            name: name.to_owned(),
            id: id.to_string(),
            seconds: ANSWER_TIME.as_secs(),
        });
        Some(Answer {
            hub: self.clone(),
            ask,
            answer: Some(rx),
        })
    }
}

/// An interface's connection to the hub, until dropped.
struct Attached {
    hub: UiHub,
    number: u64,
}

impl Drop for Attached {
    fn drop(&mut self) {
        let mut state = self.hub.0.lock().expect("lock");
        if state.ui.as_ref().is_some_and(|(n, _)| *n == self.number) {
            state.ui = None;
            state.asking = None; // nobody left to answer: a no
        }
    }
}

/// A request on the interface. Dropped unanswered (cancelled, or time ran
/// out), it leaves the screen.
pub struct Answer {
    hub: UiHub,
    ask: u64,
    answer: Option<oneshot::Receiver<Option<Permissions>>>,
}

impl Answer {
    /// What the user granted; `None`: refused, or no one is left to answer.
    pub fn wait(mut self) -> impl Future<Output = Option<Permissions>> {
        let answer = self.answer.take();
        async move {
            // Held until the answer: this is what takes it off the screen.
            let _on_screen = self;
            match answer {
                Some(answer) => answer.await.ok().flatten(),
                None => None,
            }
        }
    }
}

impl Drop for Answer {
    fn drop(&mut self) {
        let mut state = self.hub.0.lock().expect("lock");
        if state.asking.as_ref().is_some_and(|(a, _)| *a == self.ask) {
            state.asking = None;
        }
        state.send(ToUi::AskOver { ask: self.ask });
    }
}

impl Presence for UiHub {
    fn started(&self, session: &ActiveSession) -> Arc<Notify> {
        let end = Arc::new(Notify::new());
        let view = SessionView {
            name: session.name.clone(),
            id: session.id.clone(),
            permissions: session.permissions,
            unattended: session.unattended,
        };
        let mut state = self.0.lock().expect("lock");
        state.session = Some((view.clone(), end.clone()));
        state.send(ToUi::SessionStarted(view));
        end
    }

    fn ended(&self) {
        let mut state = self.0.lock().expect("lock");
        state.session = None;
        state.send(ToUi::SessionEnded);
    }
}

/// Asks through the interface (nothing else to fall back on: portable, the
/// interface is this process).
pub struct UiAsk(pub UiHub);

impl Ask for UiAsk {
    fn ask<'a>(&'a mut self, name: &'a str, id: DeviceId) -> BoxFuture<'a, Option<Permissions>> {
        Box::pin(async move {
            match self.0.ask(name, id) {
                Some(answer) => answer.wait().await,
                None => None,
            }
        })
    }

    fn show_id(&mut self, id: DeviceId) {
        self.0.set_id(id);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::DuplexStream;

    use super::*;

    fn id() -> DeviceId {
        DeviceId::new(482_913_557).unwrap()
    }

    /// A fake interface, connected to `hub`.
    fn connect(hub: &UiHub) -> (DuplexStream, tokio::task::JoinHandle<()>) {
        let (ours, theirs) = tokio::io::duplex(1 << 16);
        let hub = hub.clone();
        (ours, tokio::spawn(async move { hub.attach(theirs).await }))
    }

    async fn next(ui: &mut DuplexStream) -> ToUi {
        tokio::time::timeout(Duration::from_secs(5), read_msg(ui))
            .await
            .expect("a message in time")
            .unwrap()
            .expect("a message")
    }

    /// Waits until the hub has `ui` connected.
    async fn settled(hub: &UiHub) {
        for _ in 0..500 {
            if hub.connected() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("the interface never connected");
    }

    fn session() -> ActiveSession {
        ActiveSession {
            name: "Técnico".into(),
            id: Some("111 222 333".into()),
            permissions: Permissions::CONTROL,
            unattended: false,
        }
    }

    #[tokio::test]
    async fn the_user_answers_with_permissions() {
        let hub = UiHub::default();
        let (mut ui, _attached) = connect(&hub);
        settled(&hub).await;
        let answer = hub.ask("Técnico", id()).unwrap();
        let ToUi::Ask {
            ask,
            name,
            id: from,
            seconds,
        } = next(&mut ui).await
        else {
            panic!("expected the question");
        };
        assert_eq!((name.as_str(), from.as_str()), ("Técnico", "482 913 557"));
        assert_eq!(seconds, ANSWER_TIME.as_secs());
        let reply = FromUi::Answer {
            ask,
            permissions: Some(Permissions::VIEW_ONLY),
        };
        write_msg(&mut ui, &reply).await.unwrap();
        assert_eq!(answer.wait().await, Some(Permissions::VIEW_ONLY));
    }

    #[tokio::test]
    async fn an_answer_to_another_question_counts_for_nothing() {
        let hub = UiHub::default();
        let (mut ui, _attached) = connect(&hub);
        settled(&hub).await;
        let old = hub.ask("A", id()).unwrap();
        let ToUi::Ask { ask: old_ask, .. } = next(&mut ui).await else {
            panic!("expected the question");
        };
        drop(old);
        assert_eq!(next(&mut ui).await, ToUi::AskOver { ask: old_ask });

        // The user clicks on the old window just as the next one arrives.
        let answer = hub.ask("B", id()).unwrap();
        let ToUi::Ask { ask, .. } = next(&mut ui).await else {
            panic!("expected the question");
        };
        let late = FromUi::Answer {
            ask: old_ask,
            permissions: Some(Permissions::CONTROL),
        };
        write_msg(&mut ui, &late).await.unwrap();
        let refused = FromUi::Answer {
            ask,
            permissions: None,
        };
        write_msg(&mut ui, &refused).await.unwrap();
        assert_eq!(answer.wait().await, None);
    }

    #[tokio::test]
    async fn a_request_given_up_leaves_the_screen() {
        let hub = UiHub::default();
        let (mut ui, _attached) = connect(&hub);
        settled(&hub).await;
        let waiting = tokio::spawn(hub.ask("A", id()).unwrap().wait());
        let ToUi::Ask { ask, .. } = next(&mut ui).await else {
            panic!("expected the question");
        };
        waiting.abort();
        assert_eq!(next(&mut ui).await, ToUi::AskOver { ask });
    }

    #[tokio::test]
    async fn an_interface_that_goes_is_a_no() {
        let hub = UiHub::default();
        let (ui, _attached) = connect(&hub);
        settled(&hub).await;
        let answer = hub.ask("A", id()).unwrap();
        drop(ui);
        assert_eq!(answer.wait().await, None);
        for _ in 0..500 {
            if !hub.connected() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(hub.ask("B", id()).is_none(), "nobody to ask");
    }

    #[tokio::test]
    async fn encerrar_ends_only_the_session_going_on() {
        let hub = UiHub::default();
        let (mut ui, _attached) = connect(&hub);
        settled(&hub).await;
        // Pressed with no session: must not end the next one.
        write_msg(&mut ui, &FromUi::EndSession).await.unwrap();
        hub.set_id(id());
        assert!(matches!(next(&mut ui).await, ToUi::Id(_)));

        let end = hub.started(&session());
        assert!(matches!(next(&mut ui).await, ToUi::SessionStarted(_)));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), end.notified())
                .await
                .is_err(),
            "nobody pressed it during this session"
        );
        write_msg(&mut ui, &FromUi::EndSession).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), end.notified())
            .await
            .expect("ended");
        hub.ended();
        assert_eq!(next(&mut ui).await, ToUi::SessionEnded);
    }

    #[tokio::test]
    async fn a_new_interface_is_told_what_is_going_on() {
        let hub = UiHub::default();
        hub.set_id(id());
        hub.set_notice("aviso".into());
        let _end = hub.started(&session());

        let (mut ui, _attached) = connect(&hub);
        assert_eq!(next(&mut ui).await, ToUi::Id("482 913 557".into()));
        assert_eq!(next(&mut ui).await, ToUi::Notice("aviso".into()));
        let ToUi::SessionStarted(view) = next(&mut ui).await else {
            panic!("the indicator comes back");
        };
        assert_eq!(view.name, "Técnico");
    }

    #[tokio::test]
    async fn a_session_without_an_interface_is_kept_for_the_next() {
        let hub = UiHub::default();
        assert!(hub.ask("A", id()).is_none());
        let end = hub.started(&session());
        hub.ended();
        drop(end);
        let (mut ui, _attached) = connect(&hub);
        settled(&hub).await;
        hub.set_id(id());
        assert!(
            matches!(next(&mut ui).await, ToUi::Id(_)),
            "no session left over"
        );
    }
}
