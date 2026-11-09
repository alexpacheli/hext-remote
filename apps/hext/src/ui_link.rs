//! Between whoever owns the sessions and the interface the user sees: the
//! service and a process of the user (a local pipe), or this process when
//! portable (the same messages, in memory).
//!
//! The interface is not trusted with more than it decides: it answers
//! requests and ends sessions, and it may be replaced at any time (a logoff,
//! a crash), so the [`UiHub`] keeps what a new one must show.

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hext_proto::{ChatText, Permissions};
use hext_session::files::{Stage, Transfer};
use hext_session::host::{ActiveSession, BoxFuture, Presence};
use hext_signal::DeviceId;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Notify, mpsc, oneshot};
use tracing::warn;
use zeroize::Zeroizing;

use crate::agent::{Task, read_msg, write_msg};
use crate::signaling::{ANSWER_TIME, Ask};

/// Printed by a viewer opened by the interface (`--report-to-ui`) once it
/// is connected.
pub const VIEWER_CONNECTED: &str = "hext-ui: conectado";

/// Printed by a viewer asking a computer whose screen is locked (the
/// interface shows it, too).
pub const HOST_LOCKED: &str = "O computador está bloqueado: peça para alguém acordar a tela.";

/// Printed by a viewer opened by the interface: its person wants the chat.
pub const VIEWER_CHAT_OPEN: &str = "hext-ui: abrir chat";
/// Printed by a viewer: nobody on the other side read what was sent.
pub const VIEWER_CHAT_NO_ONE: &str = "hext-ui: ninguém no chat";
/// Before each message from the host a viewer prints.
const VIEWER_CHAT: &str = "hext-ui: chat ";

/// A message as one line, for a viewer and its interface to trade.
pub fn chat_json(text: &ChatText) -> String {
    serde_json::to_string(text).expect("a string")
}

/// The message in a line of [`chat_json`].
pub fn chat_of_json(line: &str) -> Option<ChatText> {
    serde_json::from_str::<ChatText>(line)
        .ok()
        .filter(|text| !text.is_empty())
}

/// The line a viewer prints for a message from the host.
pub fn viewer_chat_line(text: &ChatText) -> String {
    format!("{VIEWER_CHAT}{}", chat_json(text))
}

/// The message in a line of [`viewer_chat_line`].
pub fn chat_of_viewer_line(line: &str) -> Option<ChatText> {
    chat_of_json(line.strip_prefix(VIEWER_CHAT)?)
}

/// Printed by a viewer for a session that is over: alone if it ended well,
/// followed by the reason if not.
pub const VIEWER_ENDED: &str = "hext-ui: fim";

/// The reason in a line of [`VIEWER_ENDED`]: `None` if it is not one,
/// `Some(None)` if the session ended well.
pub fn ending_of_viewer_line(line: &str) -> Option<Option<&str>> {
    let rest = line.strip_prefix(VIEWER_ENDED)?;
    match rest.strip_prefix(' ') {
        Some(reason) if !reason.is_empty() => Some(Some(reason)),
        _ if rest.is_empty() => Some(None),
        _ => None,
    }
}

/// One viewer holds every session the interface opens (they share its
/// connection to the network: our relay serves one per key). Each line
/// between them is about one session, by its number.
pub fn session_line(session: u64, line: &str) -> String {
    format!("{session} {line}")
}

/// The session and the rest of a [`session_line`].
pub fn of_session_line(line: &str) -> Option<(u64, &str)> {
    let (session, rest) = line.split_once(' ')?;
    Some((session.parse().ok()?, rest))
}

const OPEN: &str = "open";
/// An open with the password, asking to be trusted from then on.
const OPEN_REMEMBER: &str = "open-remember";
/// An open with the password, asking the computer to bind to the account
/// (the chain's ID in hex, before the password).
const OPEN_ACCOUNT: &str = "open-account";
const FOCUS: &str = "focus";
const CLOSE: &str = "close";

/// What the interface asks of its viewer. (No `Debug`: it may carry a
/// password.)
pub enum ToViewer {
    /// Another computer, as session `session`; with `password`, nobody there
    /// needs to accept, and with `remember` (and the password), it trusts
    /// this technician from then on, without it. With `account` (and the
    /// password), it binds to that account instead (the chain's ID).
    Open {
        session: u64,
        id: DeviceId,
        password: Option<Zeroizing<String>>,
        remember: bool,
        account: Option<[u8; 32]>,
    },
    /// Bring the session's window forward.
    Focus(u64),
    /// End the session.
    Close(u64),
    /// Send this in the session's chat.
    Chat(u64, ChatText),
}

impl ToViewer {
    /// As one line (without its end).
    pub fn line(&self) -> Zeroizing<String> {
        Zeroizing::new(match self {
            Self::Open {
                session,
                id,
                password: None,
                ..
            } => format!("{OPEN} {session} {}", id.get()),
            Self::Open {
                session,
                id,
                password: Some(password),
                account: Some(chain),
                ..
            } => format!(
                "{OPEN_ACCOUNT} {session} {} {} {}",
                id.get(),
                hex::encode(chain),
                password.as_str()
            ),
            Self::Open {
                session,
                id,
                password: Some(password),
                remember,
                account: None,
            } => {
                let verb = if *remember { OPEN_REMEMBER } else { OPEN };
                format!("{verb} {session} {} {}", id.get(), password.as_str())
            }
            Self::Focus(session) => session_line(*session, FOCUS),
            Self::Close(session) => session_line(*session, CLOSE),
            Self::Chat(session, text) => session_line(*session, &chat_json(text)),
        })
    }

    /// A line of [`ToViewer::line`] (its end already gone).
    pub fn parse(line: &str) -> Option<Self> {
        let open = |verb: &str| line.strip_prefix(verb).and_then(|r| r.strip_prefix(' '));
        if let Some(rest) = open(OPEN_ACCOUNT) {
            let mut parts = rest.splitn(4, ' ');
            let session = parts.next()?.parse().ok()?;
            let id = parts.next()?.parse().ok()?;
            let chain = hex::decode(parts.next()?).ok()?.try_into().ok()?;
            let password = parts.next().filter(|p| !p.is_empty())?;
            return Some(Self::Open {
                session,
                id,
                password: Some(Zeroizing::new(password.to_owned())),
                remember: false,
                account: Some(chain),
            });
        }
        let (remember, open) = match open(OPEN_REMEMBER) {
            Some(rest) => (true, Some(rest)),
            None => (false, open(OPEN)),
        };
        if let Some(rest) = open {
            let mut parts = rest.splitn(3, ' ');
            let session = parts.next()?.parse().ok()?;
            let id = parts.next()?.parse().ok()?;
            let password = parts
                .next()
                .filter(|p| !p.is_empty())
                .map(|p| Zeroizing::new(p.to_owned()));
            return Some(Self::Open {
                session,
                id,
                remember: remember && password.is_some(),
                password,
                account: None,
            });
        }
        let (session, rest) = of_session_line(line)?;
        Some(match rest {
            FOCUS => Self::Focus(session),
            CLOSE => Self::Close(session),
            chat => Self::Chat(session, chat_of_json(chat)?),
        })
    }
}

/// Owner of the sessions → interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToUi {
    /// This computer's ID.
    Id(String),
    /// This computer's host key (what the account's list names it by).
    Key(String),
    /// This computer's name, as viewers see it.
    Name(String),
    /// The account bound here, as the home screen says it (`None`: none).
    Owners(Option<String>),
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
    /// How a file is going. `news`: more than the percentage moved on.
    /// `finished`: it got to its end, one way or the other.
    Transfer {
        text: String,
        news: bool,
        finished: bool,
    },
    /// The viewer wrote.
    Chat(ChatText),
    /// The technicians this computer trusts without the password (only
    /// where there is such a list: the service).
    Technicians(Vec<crate::technicians::Technician>),
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
    /// The user chose a file to send to the viewer. It is opened with this
    /// user's rights, not with its owner's.
    SendFile(PathBuf),
    /// The user wrote to the viewer.
    Chat(ChatText),
    /// The user gave this computer another name (empty: back to the
    /// system's). Taken only where the name is this user's, by the app's
    /// own host: the service's is an administrator's to change.
    Rename(String),
    /// Stop trusting this technician (by key) without the password. Anyone
    /// at the computer may: it only takes access away.
    Revoke(String),
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

/// Writes a line in the audit log.
type Audit = fn(&str);

#[derive(Default)]
struct State {
    /// The interface connected now, by connection number.
    ui: Option<(u64, mpsc::UnboundedSender<ToUi>)>,
    next_ui: u64,
    id: Option<String>,
    notice: Option<String>,
    session: Option<(SessionView, Arc<Notify>)>,
    /// Where the files the user sends go, during a session.
    outgoing: Option<mpsc::UnboundedSender<PathBuf>>,
    /// The other end of `outgoing`, until the session takes it.
    to_session: Option<mpsc::UnboundedReceiver<PathBuf>>,
    /// Where what the user writes goes, during a session.
    chat_out: Option<mpsc::UnboundedSender<ChatText>>,
    /// This computer's name, as viewers see it.
    name: Option<String>,
    /// Where [`FromUi::Rename`] writes; `None`: not taken here.
    name_dir: Option<PathBuf>,
    /// The other end of `chat_out`, until the session takes it.
    chat_to_session: Option<mpsc::UnboundedReceiver<ChatText>>,
    /// The technicians trusted without the password, and how a revocation
    /// is written down; `None`: no such list here.
    technicians: Option<(crate::technicians::Technicians, Audit)>,
    /// The list as the interface last got it.
    technicians_shown: Vec<crate::technicians::Technician>,
    /// The host's key, as text.
    key: Option<String>,
    /// The bound account, as said to the interface.
    owners: Option<String>,
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

    /// Shows the technicians as kept now, if that is not what the interface
    /// has already.
    fn show_technicians(&mut self) {
        let Some((technicians, _)) = &self.technicians else {
            return;
        };
        let list = technicians.list();
        if list != self.technicians_shown {
            self.technicians_shown = list.clone();
            self.send(ToUi::Technicians(list));
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
            if let Some(key) = &state.key {
                let _ = tx.send(ToUi::Key(key.clone()));
            }
            if let Some(name) = &state.name {
                let _ = tx.send(ToUi::Name(name.clone()));
            }
            if state.owners.is_some() {
                let _ = tx.send(ToUi::Owners(state.owners.clone()));
            }
            if let Some(notice) = &state.notice {
                let _ = tx.send(ToUi::Notice(notice.clone()));
            }
            if state.technicians.is_some() {
                let _ = tx.send(ToUi::Technicians(state.technicians_shown.clone()));
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
            FromUi::SendFile(path) => {
                if let Some(outgoing) = &state.outgoing {
                    let _ = outgoing.send(path);
                }
            }
            FromUi::Chat(text) => {
                if let Some(chat_out) = &state.chat_out {
                    let _ = chat_out.send(text);
                }
            }
            FromUi::Revoke(key) => {
                let Some((technicians, audit)) = &state.technicians else {
                    return;
                };
                match technicians.revoke(&key) {
                    Ok(gone) => {
                        for t in gone {
                            audit(&format!(
                                "técnico desvinculado na tela: \"{}\" (ID {})",
                                t.name, t.id
                            ));
                        }
                    }
                    Err(e) => warn!("{}: {e}", technicians.path().display()),
                }
                state.show_technicians();
            }
            FromUi::Rename(typed) => {
                let Some(dir) = state.name_dir.clone() else {
                    return;
                };
                let typed = typed.trim();
                let renamed = if typed.is_empty() {
                    crate::name::write(&dir, None).map(|()| crate::name::hostname())
                } else {
                    crate::name::clean(typed)
                        .and_then(|name| crate::name::write(&dir, Some(&name)).map(|()| name))
                };
                match renamed {
                    Ok(name) => {
                        state.name = Some(name.clone());
                        state.send(ToUi::Name(name));
                    }
                    Err(e) => {
                        let notice = format!("Não foi possível mudar o nome: {e:#}");
                        state.notice = Some(notice.clone());
                        state.send(ToUi::Notice(notice));
                    }
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

    /// The host's key, for the interface to put this computer in an
    /// account.
    pub fn set_key(&self, key: &hext_crypto::PublicKey) {
        let mut state = self.0.lock().expect("lock");
        let key = key.to_string();
        state.key = Some(key.clone());
        state.send(ToUi::Key(key));
    }

    /// The account bound here (`None`: none), for the home screen.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn set_owners(&self, text: Option<String>) {
        let mut state = self.0.lock().expect("lock");
        if state.owners != text {
            state.owners = text.clone();
            state.send(ToUi::Owners(text));
        }
    }

    pub fn set_id(&self, id: DeviceId) {
        let mut state = self.0.lock().expect("lock");
        let id = id.to_string();
        state.id = Some(id.clone());
        state.send(ToUi::Id(id));
    }

    /// This computer's name, and where the user may change it (`None`: not
    /// here).
    pub fn set_name(&self, name: String, name_dir: Option<PathBuf>) {
        let mut state = self.0.lock().expect("lock");
        state.name = Some(name.clone());
        state.name_dir = name_dir;
        state.send(ToUi::Name(name));
    }

    /// The technicians trusted here without the password, and how a
    /// revocation on the interface is written down.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn set_technicians(&self, technicians: crate::technicians::Technicians, audit: Audit) {
        let mut state = self.0.lock().expect("lock");
        let list = technicians.list();
        state.send(ToUi::Technicians(list.clone()));
        state.technicians_shown = list;
        state.technicians = Some((technicians, audit));
    }

    /// The list of technicians may have changed (here, or on the command
    /// line): the interface shows it again if it did.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn technicians_changed(&self) {
        self.0.lock().expect("lock").show_technicians();
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
    fn device_name(&self) -> Option<String> {
        let state = self.0.lock().expect("lock");
        state.name_dir.as_ref().and(state.name.clone())
    }

    fn started(&self, session: &ActiveSession) -> Arc<Notify> {
        let end = Arc::new(Notify::new());
        let view = SessionView {
            name: session.name.clone(),
            id: session.id.clone(),
            permissions: session.permissions,
            unattended: session.unattended,
        };
        let (outgoing, to_session) = mpsc::unbounded_channel();
        let (chat_out, chat_to_session) = mpsc::unbounded_channel();
        let mut state = self.0.lock().expect("lock");
        state.session = Some((view.clone(), end.clone()));
        state.outgoing = Some(outgoing);
        state.to_session = Some(to_session);
        state.chat_out = Some(chat_out);
        state.chat_to_session = Some(chat_to_session);
        state.send(ToUi::SessionStarted(view));
        end
    }

    fn ended(&self) {
        let mut state = self.0.lock().expect("lock");
        state.session = None;
        state.outgoing = None;
        state.to_session = None;
        state.chat_out = None;
        state.chat_to_session = None;
        state.send(ToUi::SessionEnded);
    }

    fn outgoing(&self) -> Option<mpsc::UnboundedReceiver<PathBuf>> {
        self.0.lock().expect("lock").to_session.take()
    }

    fn transfer(&self, transfer: &Transfer) {
        self.0.lock().expect("lock").send(ToUi::Transfer {
            text: transfer.to_string(),
            news: transfer.is_news(),
            finished: !matches!(transfer.stage, Stage::Going { .. }),
        });
    }

    fn chat(&self, text: ChatText) -> bool {
        let state = self.0.lock().expect("lock");
        let heard = state.ui.is_some() && state.session.is_some();
        if heard {
            state.send(ToUi::Chat(text));
        }
        heard
    }

    fn chat_out(&self) -> Option<mpsc::UnboundedReceiver<ChatText>> {
        self.0.lock().expect("lock").chat_to_session.take()
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
        crate::service::remember_id(&id.to_string());
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

    #[test]
    fn what_the_interface_asks_of_its_viewer_goes_as_lines() {
        let open = ToViewer::Open {
            session: 3,
            id: id(),
            password: Some(Zeroizing::new("uma senha  com espaços ".to_owned())),
            remember: false,
            account: None,
        };
        let line = open.line();
        assert_eq!(line.as_str(), "open 3 482913557 uma senha  com espaços ");
        let Some(ToViewer::Open {
            session: 3,
            id: back,
            password: Some(password),
            remember: false,
            account: None,
        }) = ToViewer::parse(&line)
        else {
            panic!("an open with a password");
        };
        assert_eq!((back, password.as_str()), (id(), "uma senha  com espaços "));
        assert!(matches!(
            ToViewer::parse("open 4 482913557"),
            Some(ToViewer::Open {
                session: 4,
                password: None,
                ..
            })
        ));
        let remembered = ToViewer::Open {
            session: 6,
            id: id(),
            password: Some(Zeroizing::new("senha".to_owned())),
            remember: true,
            account: None,
        };
        let line = remembered.line();
        assert_eq!(line.as_str(), "open-remember 6 482913557 senha");
        let bound = ToViewer::Open {
            session: 8,
            id: id(),
            password: Some(Zeroizing::new("a senha".to_owned())),
            remember: false,
            account: Some([0xab; 32]),
        };
        let bound_line = bound.line();
        assert_eq!(
            bound_line.as_str(),
            format!("open-account 8 482913557 {} a senha", "ab".repeat(32))
        );
        let Some(ToViewer::Open {
            session: 8,
            password: Some(password),
            account: Some(chain),
            remember: false,
            ..
        }) = ToViewer::parse(&bound_line)
        else {
            panic!("an open binding to an account");
        };
        assert_eq!((password.as_str(), chain), ("a senha", [0xab; 32]));
        // Without the password, or with a broken ID, it is no open.
        assert!(
            ToViewer::parse(&format!("open-account 8 482913557 {}", "ab".repeat(32))).is_none()
        );
        assert!(ToViewer::parse("open-account 8 482913557 abcd senha").is_none());
        assert!(matches!(
            ToViewer::parse(&line),
            Some(ToViewer::Open {
                session: 6,
                remember: true,
                password: Some(_),
                ..
            })
        ));
        // Without the password there is nothing to remember it by.
        assert!(matches!(
            ToViewer::parse("open-remember 7 482913557"),
            Some(ToViewer::Open {
                remember: false,
                password: None,
                ..
            })
        ));
        assert!(matches!(
            ToViewer::parse("2 focus"),
            Some(ToViewer::Focus(2))
        ));
        assert!(matches!(
            ToViewer::parse("2 close"),
            Some(ToViewer::Close(2))
        ));
        let text = ChatText::new("oi").unwrap();
        let chat = ToViewer::Chat(5, text.clone()).line();
        assert!(matches!(ToViewer::parse(&chat), Some(ToViewer::Chat(5, t)) if t == text));
        assert!(ToViewer::parse("open x 1").is_none());
        assert!(ToViewer::parse("focus").is_none());
        assert!(ToViewer::parse("2 nonsense").is_none());
    }

    #[test]
    fn a_viewer_says_which_session_each_line_is_about() {
        let line = session_line(7, VIEWER_CONNECTED);
        assert_eq!(of_session_line(&line), Some((7, VIEWER_CONNECTED)));
        assert_eq!(of_session_line("Pedindo acesso a 482913557..."), None);
        assert_eq!(ending_of_viewer_line(VIEWER_ENDED), Some(None));
        let failed = format!("{VIEWER_ENDED} senha errada");
        assert_eq!(ending_of_viewer_line(&failed), Some(Some("senha errada")));
        assert_eq!(ending_of_viewer_line("hext-ui: fimx"), None);
        assert_eq!(ending_of_viewer_line(VIEWER_CHAT_OPEN), None);
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
    async fn the_technicians_on_screen_follow_the_file_and_revoke() {
        let dir = std::env::temp_dir().join(format!("hext-hub-technicians-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = crate::technicians::Technicians::new(dir.join("technicians.json"));
        let key = hext_crypto::PublicKey::from_bytes([7; 32]);
        store.bind(&key, "482 913 557", "Técnico").unwrap();

        let hub = UiHub::default();
        hub.set_technicians(store.clone(), |_| {});
        let (mut ui, _attached) = connect(&hub);
        let ToUi::Technicians(list) = next(&mut ui).await else {
            panic!("the list first");
        };
        assert_eq!(list.len(), 1);

        // Revoked on the command line: shown once the check sees it, and
        // nothing is sent while it stays the same.
        store.revoke("482913557").unwrap();
        hub.technicians_changed();
        hub.technicians_changed();
        assert_eq!(next(&mut ui).await, ToUi::Technicians(Vec::new()));

        // Revogar on the screen.
        store.bind(&key, "482 913 557", "Técnico").unwrap();
        hub.technicians_changed();
        let ToUi::Technicians(list) = next(&mut ui).await else {
            panic!("the list again");
        };
        write_msg(&mut ui, &FromUi::Revoke(list[0].key.clone()))
            .await
            .unwrap();
        assert_eq!(next(&mut ui).await, ToUi::Technicians(Vec::new()));
        assert!(!store.bound(&key));
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
    async fn the_files_chosen_go_to_the_session_on_now() {
        let hub = UiHub::default();
        let (mut ui, _attached) = connect(&hub);
        settled(&hub).await;
        // Chosen with no session: goes nowhere.
        let chosen = FromUi::SendFile("antes.txt".into());
        write_msg(&mut ui, &chosen).await.unwrap();
        hub.set_id(id());
        assert!(matches!(next(&mut ui).await, ToUi::Id(_)));

        let _end = hub.started(&session());
        let mut files = hub.outgoing().expect("this session's");
        assert!(hub.outgoing().is_none(), "once");
        let chosen = FromUi::SendFile("durante.txt".into());
        write_msg(&mut ui, &chosen).await.unwrap();
        assert_eq!(files.recv().await, Some("durante.txt".into()));
        hub.ended();
        assert_eq!(files.recv().await, None, "over with the session");
    }

    #[tokio::test]
    async fn the_chat_goes_to_the_session_on_now_and_comes_from_it() {
        let hub = UiHub::default();
        let (mut ui, service_end) = tokio::io::duplex(1 << 16);
        let attached = hub.clone();
        tokio::spawn(async move { attached.attach(service_end).await });
        settled(&hub).await;
        let text = ChatText::new("Oi").unwrap();
        assert!(!hub.chat(text.clone()), "no session, nobody to read it");

        let _end = hub.started(&session());
        assert!(matches!(next(&mut ui).await, ToUi::SessionStarted(_)));
        assert!(hub.chat(text.clone()));
        assert_eq!(next(&mut ui).await, ToUi::Chat(text.clone()));

        let mut out = hub.chat_out().expect("once for each session");
        assert!(hub.chat_out().is_none());
        hub.received(FromUi::Chat(text.clone()));
        assert_eq!(out.recv().await, Some(text.clone()));

        hub.ended();
        assert_eq!(out.recv().await, None, "nothing goes to the next one");
    }

    #[test]
    fn with_no_interface_nobody_reads_the_chat() {
        let hub = UiHub::default();
        let _end = hub.started(&session());
        assert!(!hub.chat(ChatText::new("Oi").unwrap()));
    }

    #[test]
    fn a_message_fits_a_line_whatever_it_says() {
        let text = ChatText::new("duas\nlinhas \"e\" aspas").unwrap();
        let line = viewer_chat_line(&text);
        assert!(!line.contains('\n'));
        assert_eq!(chat_of_viewer_line(&line), Some(text));
        assert_eq!(chat_of_viewer_line("hext-ui: conectado"), None);
        assert_eq!(chat_of_json("\"\u{202e}\""), None, "nothing left of it");
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
