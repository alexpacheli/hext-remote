//! Host side of a session.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hext_clipboard::Clipboard;
use hext_codec::EncoderSettings;
use hext_crypto::PublicKey;
use hext_crypto::pake::Verifier;
use hext_input::Area;
use hext_net::video::{LOG_TARGET, ResendRequest, VideoSender};
use hext_net::{Admission, Connection, ConnectionError, NetError};
use hext_proto::{
    Control, ENDED_ON_HOST, Hello, InputEvent, MAX_CLIPBOARD_TEXT, PeerInfo, Permissions,
    RejectReason, StreamKind, VideoFrame,
};
use tokio::sync::{Notify, mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::files::{self, Direction, FileAccess, FileEnd, Stage, Tell, Transfer};
use crate::{KeptResume, Lockout, Result, ResumeTokens, SessionError, codes_match};

/// How long the viewer has to hear that the host ended the session.
const BYE_WAIT: Duration = Duration::from_secs(1);
/// How often a still screen repeats the number of its last frame.
const IDLE_NOTICE: Duration = Duration::from_millis(250);

pub struct HostConfig {
    pub peer: PeerInfo,
    /// The code shown to the host's user, good for any number of sessions;
    /// `None`: sessions only through the server (accepted one by one).
    pub access_code: Option<String>,
    /// One-time codes given to viewers the host's user accepted (through
    /// the server); each also opens one session.
    pub session_codes: crate::SessionCodes,
    /// Where the screen comes from and the input goes.
    pub desktop: Arc<dyn Desktop>,
    /// Keeps the host's user aware of the session, with a way to end it.
    pub presence: Option<Arc<dyn Presence>>,
    /// Sessions without anyone accepting, with the password; `None`: this
    /// host has no such thing (it only exists installed).
    pub unattended: Option<UnattendedAccess>,
    /// The tokens viewers come back with.
    pub resumes: ResumeTokens,
    /// Restarts this computer; `None`: this host cannot (it takes the
    /// service, to be back afterwards).
    pub restart: Option<Arc<dyn Restart>>,
    /// Where files from viewers go; `None`: this host takes none.
    pub files: Option<Arc<dyn FileAccess>>,
}

/// What a restart takes on the host.
pub trait Restart: Send + Sync {
    /// Keeps `kept` for after the restart, and restarts the computer.
    /// `who`: the viewer who asked, as the audit names it.
    fn restart(&self, kept: &KeptResume, who: &str) -> std::result::Result<(), String>;
    /// The host's user ended the session: what [`Self::restart`] kept is no
    /// longer good, even if the computer does restart.
    fn revoke(&self);
}

/// What the unattended password takes on the host.
pub struct UnattendedAccess {
    /// This computer's key: the exchange is tied to it.
    pub key: PublicKey,
    pub lockout: Lockout,
    pub password: Arc<dyn UnattendedPassword>,
    /// Where the outcomes go, on their way to the server.
    pub attempts: mpsc::UnboundedSender<Attempt>,
}

/// How a session with nobody accepting went. The server cannot see that, and
/// limits requests by it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    /// The viewer's ID, as in [`crate::Grant`].
    pub viewer: String,
    /// With the password or a token; `false`: the password was wrong.
    pub entered: bool,
}

/// Where the password is kept and what is said about its use.
pub trait UnattendedPassword: Send + Sync {
    /// What is kept of the password now; `None`: the access is off. Read on
    /// each request, so a change needs no restart.
    fn verifier(&self) -> Option<Verifier>;
    /// Writes down something that happened, for the owner of the computer.
    fn audit(&self, entry: &str);
}

/// The session going on, as the host's user is shown it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSession {
    pub name: String,
    /// `None`: connected by IP, with the access code.
    pub id: Option<String>,
    pub permissions: Permissions,
    /// Came in with the password; nobody accepted.
    pub unattended: bool,
}

/// The host's indicator of a session (ARCHITECTURE 7.3).
pub trait Presence: Send + Sync {
    /// A session starts (before anything is shared). The session ends
    /// when the returned [`Notify`], new for each session, is notified.
    fn started(&self, session: &ActiveSession) -> Arc<Notify>;
    /// The session ended (however it did).
    fn ended(&self);
    /// The files the host's user wants to send during the session started
    /// last. Once for each session.
    fn outgoing(&self) -> Option<mpsc::UnboundedReceiver<PathBuf>> {
        None
    }
    /// How a file is going.
    fn transfer(&self, _transfer: &Transfer) {}
}

/// The session on the indicator, until dropped.
struct Shown(Arc<dyn Presence>);

impl Drop for Shown {
    fn drop(&mut self) {
        self.0.ended();
    }
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Where a session's screen comes from and its input goes: this process
/// ([`LocalDesktop`]), or another one (the agent of a Windows service).
pub trait Desktop: Send + Sync {
    /// Starts streaming the screen, encoded, into `frames`. With `clipboard`,
    /// also tells it each new text copied on this computer.
    fn open(
        &self,
        frames: mpsc::Sender<VideoFrame>,
        clipboard: Option<mpsc::Sender<String>>,
    ) -> BoxFuture<'_, Result<Box<dyn Screen>>>;

    /// The video's bit rate, for pacing it out.
    fn bitrate_bps(&self) -> u32;
}

/// A screen being streamed.
pub trait Screen: Send {
    /// The viewer lost video: the next frame must be a keyframe.
    fn request_keyframe(&self);
    /// Replays the viewer's input.
    fn inject(&mut self, event: &InputEvent);
    /// Puts text from the viewer on this computer's clipboard.
    fn set_clipboard(&mut self, text: String);
    /// Stops streaming, releasing any key or button still held.
    fn close(self: Box<Self>) -> BoxFuture<'static, ()>;
}

/// The screen of this process's desktop: capture and encoding on a thread
/// of their own, input injected here.
pub struct LocalDesktop {
    /// Display index (0 = primary).
    pub display: usize,
    /// Stream a synthetic pattern instead of the real screen.
    pub test_pattern: bool,
    pub encoder: EncoderSettings,
}

impl Desktop for LocalDesktop {
    fn open(
        &self,
        frames: mpsc::Sender<VideoFrame>,
        clipboard: Option<mpsc::Sender<String>>,
    ) -> BoxFuture<'_, Result<Box<dyn Screen>>> {
        let settings = CaptureSettings {
            display: self.display,
            test_pattern: self.test_pattern,
            encoder: self.encoder,
        };
        Box::pin(async move {
            let shared = Arc::new(Shared {
                stop: AtomicBool::new(false),
                keyframe: AtomicBool::new(false),
                area: Mutex::new(Area::default()),
            });
            // Capture and encoding run on their own thread: both are
            // blocking, and capture objects (COM on Windows) must stay on
            // the thread that made them. A one-slot channel gives
            // backpressure: while the network is busy the thread waits, and
            // the next capture picks up the latest screen.
            let (started_tx, started_rx) = oneshot::channel();
            let thread = {
                let shared = shared.clone();
                std::thread::Builder::new()
                    .name("hext-capture".into())
                    .spawn(move || capture_loop(&settings, &shared, &frames, started_tx))
                    .map_err(|e| SessionError::Protocol(format!("capture thread: {e}")))?
            };
            let started = started_rx
                .await
                .map_err(|_| SessionError::Protocol("capture thread died".into()))?;
            started?;
            let (input, input_thread) = spawn_input(shared.clone())?;
            // A session goes on without it.
            let clipboard = clipboard.and_then(|changed| {
                Clipboard::watch(move |text| {
                    let _ = changed.try_send(text);
                })
                .inspect_err(|e| warn!("área de transferência: {e}"))
                .ok()
            });
            Ok(Box::new(LocalScreen {
                shared,
                clipboard,
                thread: Some(thread),
                input: Some(input),
                input_thread: Some(input_thread),
                input_failed: false,
            }) as Box<dyn Screen>)
        })
    }

    fn bitrate_bps(&self) -> u32 {
        self.encoder.bitrate_bps
    }
}

#[derive(Clone, Copy)]
struct CaptureSettings {
    display: usize,
    test_pattern: bool,
    encoder: EncoderSettings,
}

struct LocalScreen {
    shared: Arc<Shared>,
    /// Only with the permission.
    clipboard: Option<Clipboard>,
    /// Taken by `close`, which waits for it.
    thread: Option<std::thread::JoinHandle<()>>,
    /// The input thread; it releases held keys when this closes.
    input: Option<std::sync::mpsc::Sender<InputEvent>>,
    /// Taken by `close`, which waits for it (keys released).
    input_thread: Option<std::thread::JoinHandle<()>>,
    /// The input thread is gone (logged once).
    input_failed: bool,
}

/// Also when the session is dropped without `close` (a task cancelled):
/// never leave keys stuck down on the host (the input thread releases them
/// when its channel closes), nor the capture running.
impl Drop for LocalScreen {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

/// Injects input on a thread of its own: on Windows it follows the input
/// desktop (the sign-in screen and UAC prompts, for a SYSTEM agent), which
/// only works per thread.
fn spawn_input(
    shared: Arc<Shared>,
) -> Result<(
    std::sync::mpsc::Sender<InputEvent>,
    std::thread::JoinHandle<()>,
)> {
    /// How often to check whether the input desktop changed (not on every
    /// mouse move).
    const FOLLOW_EVERY: Duration = Duration::from_millis(200);
    let (tx, rx) = std::sync::mpsc::channel::<InputEvent>();
    let thread = std::thread::Builder::new()
        .name("hext-input".into())
        .spawn(move || {
            let mut injector = hext_input::injector();
            let mut followed: Option<Instant> = None;
            while let Ok(event) = rx.recv() {
                if followed.is_none_or(|t| t.elapsed() >= FOLLOW_EVERY) {
                    hext_capture::follow_input_desktop();
                    followed = Some(Instant::now());
                }
                let area = *shared.area.lock().expect("lock");
                injector.inject(&event, area);
            }
            injector.release_all();
        })
        .map_err(|e| SessionError::Protocol(format!("input thread: {e}")))?;
    Ok((tx, thread))
}

impl Screen for LocalScreen {
    fn request_keyframe(&self) {
        self.shared.keyframe.store(true, Ordering::Relaxed);
    }

    fn inject(&mut self, event: &InputEvent) {
        if *event == InputEvent::SecureAttention {
            // Takes the Windows service (this process is not it).
            debug!("Ctrl+Alt+Del: só o serviço pode enviar");
            return;
        }
        let sent = self
            .input
            .as_ref()
            .is_some_and(|input| input.send(*event).is_ok());
        if !sent && !std::mem::replace(&mut self.input_failed, true) {
            warn!("input: a thread de injeção parou; mouse e teclado não chegam mais");
        }
    }

    fn set_clipboard(&mut self, text: String) {
        if let Some(clipboard) = &self.clipboard {
            clipboard.set(text);
        }
    }

    fn close(mut self: Box<Self>) -> BoxFuture<'static, ()> {
        // Releases the keys and stops the capture (`Drop`), then waits for
        // the capture to let go of the screen and the encoder.
        let thread = self.thread.take();
        let input_thread = self.input_thread.take();
        // Closing its channel makes the input thread release held keys.
        self.input = None;
        drop(self);
        Box::pin(async move {
            let _ = tokio::task::spawn_blocking(move || {
                if let Some(input) = input_thread {
                    let _ = input.join();
                }
                if let Some(thread) = thread {
                    let _ = thread.join();
                }
            })
            .await;
        })
    }
}

/// State shared between a [`LocalScreen`] and its capture thread.
struct Shared {
    stop: AtomicBool,
    keyframe: AtomicBool,
    area: Mutex<Area>,
}

/// How a session with the password is told apart in the audit.
fn who(grant: &crate::Grant) -> String {
    format!("\"{}\" (ID {})", grant.name, grant.id)
}

/// Writes down the end of a session with the password, when dropped.
struct AuditEnd(Arc<dyn UnattendedPassword>, String);

impl Drop for AuditEnd {
    fn drop(&mut self) {
        self.0.audit(&format!("sessão encerrada: \"{}\"", self.1));
    }
}

/// Tells the server how a session with nobody accepting went.
fn report(access: &UnattendedAccess, grant: Option<&crate::Grant>, entered: bool) {
    if let Some(grant) = grant {
        let viewer = grant.id.clone();
        // (Gone when the host is on its way out: nothing to tell anymore.)
        let _ = access.attempts.send(Attempt { viewer, entered });
    }
}

/// Whether the password may be tried by `viewer` now, and against what.
fn admit_with_password(
    access: Option<&UnattendedAccess>,
    viewer: &PublicKey,
    grant: &crate::Grant,
) -> std::result::Result<Admission, RejectReason> {
    let access = access.ok_or(RejectReason::Denied)?;
    if let Some(wait) = access.lockout.check(viewer) {
        return Err(RejectReason::LockedOut {
            seconds: wait.as_secs().max(1).try_into().unwrap_or(u32::MAX),
        });
    }
    let verifier = access.password.verifier().ok_or(RejectReason::Denied)?;
    Ok(Admission::Password {
        permissions: grant.permissions,
        verifier,
        host: access.key,
    })
}

/// Runs one session on an accepted connection until the client leaves.
///
/// Returns the client's identity once the session ends normally.
pub async fn serve(conn: Connection, config: Arc<HostConfig>) -> Result<PeerInfo> {
    // The viewer proved it holds this key; a session code is only good from
    // the viewer it was issued to.
    let viewer = hext_net::remote_key(&conn);
    let mut grant: Option<crate::Grant> = None;
    let mut resumed = false;
    let handshake = hext_net::host_handshake(&conn, config.peer.clone(), |hello: &Hello| {
        let code = config.access_code.as_deref();
        if code.is_some_and(|code| codes_match(code, &hello.access_code)) {
            return Ok(Admission::Allow(Permissions::CONTROL));
        }
        grant = config
            .session_codes
            .redeem(&hello.access_code, &viewer)
            .map(|mut grant| {
                grant.permissions.restart &= config.restart.is_some();
                grant.permissions.files &= config.files.is_some();
                grant
            });
        let granted = grant.as_ref().ok_or(RejectReason::WrongCode)?;
        if !granted.needs_password {
            return Ok(Admission::Allow(granted.permissions));
        }
        // Nobody accepted: the token of a session it had, or the password.
        let back = hello
            .resume
            .and_then(|token| config.resumes.take(&token, &viewer));
        if let Some(back) = back {
            let permissions = back.permissions;
            grant = Some(back);
            resumed = true;
            return Ok(Admission::Allow(permissions));
        }
        admit_with_password(config.unattended.as_ref(), &viewer, granted)
    })
    .await;
    // What counts as a guess is known only now.
    let unattended = config
        .unattended
        .as_ref()
        .filter(|_| grant.as_ref().is_some_and(|grant| grant.needs_password));
    let (mut control, hello) = match (handshake, unattended) {
        (Ok(done), Some(access)) => {
            access.lockout.succeeded(&viewer);
            report(access, grant.as_ref(), true);
            done
        }
        (Err(e), Some(access)) => {
            if matches!(e, NetError::Rejected(RejectReason::WrongPassword)) {
                access.lockout.failed(&viewer);
                report(access, grant.as_ref(), false);
            }
            if let (NetError::Rejected(reason), Some(grant)) = (&e, &grant) {
                access
                    .password
                    .audit(&format!("recusado: {reason:?}, {}", who(grant)));
            }
            return Err(e.into());
        }
        (handshake, None) => handshake?,
    };
    let client = hello.peer;
    let audit = config
        .unattended
        .as_ref()
        .filter(|_| resumed)
        .or(unattended)
        .map(|access| access.password.clone());
    if let (true, Some(access), Some(grant)) = (resumed, &config.unattended, &grant) {
        access
            .password
            .audit(&format!("sessão retomada: {}", who(grant)));
    }
    let returnable = grant.clone();
    let session = match grant {
        Some(grant) => {
            if let Some(audit) = audit.as_ref().filter(|_| !resumed) {
                audit.audit(&format!("sessão iniciada: {}", who(&grant)));
            }
            ActiveSession {
                unattended: grant.needs_password,
                name: grant.name,
                id: Some(grant.id),
                permissions: grant.permissions,
            }
        }
        None => ActiveSession {
            name: client.device_name.clone(),
            id: None,
            permissions: Permissions::CONTROL,
            unattended: false,
        },
    };
    // Written however the session ends.
    let _audit_end = audit
        .clone()
        .map(|audit| AuditEnd(audit, session.name.clone()));
    // (Not the path: right after the handshake it is usually the relay,
    // and a direct one follows; the viewer's title shows it live.)
    info!(
        "conectado: {} ({:?}, v{})",
        client.device_name, client.os, client.app_version,
    );

    // On the indicator before the first frame leaves.
    let end = config
        .presence
        .as_ref()
        .map_or_else(Default::default, |presence| presence.started(&session));
    let _shown = config.presence.clone().map(Shown);

    // One slot: backpressure from the network back to the capture.
    let (frames_tx, mut frames_rx) = mpsc::channel::<VideoFrame>(1);
    // A few slots: a burst of copies loses the middle ones, which is fine.
    let (clipboard_tx, mut clipboard_rx) = mpsc::channel::<String>(4);
    let clipboard = session.permissions.clipboard.then_some(clipboard_tx);
    let screen = match config.desktop.open(frames_tx, clipboard).await {
        Ok(screen) => Arc::new(Mutex::new(Some(screen))),
        Err(e) => {
            conn.close(1u32.into(), b"capture unavailable");
            return Err(e);
        }
    };

    // Video goes out as datagrams with FEC (see `hext_net::video`).
    let video = conn.clone();
    let bitrate = config.desktop.bitrate_bps();
    // A few slots: a viewer asking for more than this is past saving.
    let (resend_tx, mut resend_rx) = mpsc::channel::<ResendRequest>(32);
    let mut writer = tokio::spawn(async move {
        let mut sender = VideoSender::new(bitrate);
        loop {
            tokio::select! {
                biased;
                Some(request) = resend_rx.recv() => sender.resend(&video, &request).await?,
                frame = tokio::time::timeout(IDLE_NOTICE, frames_rx.recv()) => match frame {
                    Ok(Some(frame)) => sender.send(&video, &frame, &mut resend_rx).await?,
                    Ok(None) => return Ok::<_, NetError>(()),
                    // A still screen: let the viewer check it has the last frame.
                    Err(_) => sender.send_idle(&video).await?,
                },
            }
        }
    });
    // A file cut off with the session is told about after it ended.
    let session_on = Arc::new(AtomicBool::new(true));
    let (file_answers, answers) = mpsc::unbounded_channel();
    let (outcomes, mut file_outcomes) = mpsc::unbounded_channel();
    let file_end = FileEnd {
        conn: conn.clone(),
        allowed: session.permissions.files,
        access: config.files.clone(),
        paths: config
            .presence
            .as_ref()
            .and_then(|presence| presence.outgoing())
            .filter(|_| session.permissions.files)
            .unwrap_or_else(|| mpsc::unbounded_channel().1),
        answers,
        outcomes,
        tell: tell_about_files(
            config.presence.clone(),
            audit,
            session.name.clone(),
            session_on.clone(),
        ),
        confirm: None,
    };
    let mut input = tokio::spawn(receive_input(
        conn.clone(),
        screen.clone(),
        session.permissions,
        file_end,
    ));

    // Through the server: this viewer can come back. (A failure to tell it
    // is the connection failing, which the loop below sees.)
    let (token_hash, displaced) = match returnable {
        Some(grant) => {
            let (token, hash, displaced) = config.resumes.open(viewer, grant);
            let _ = control.send(&Control::ResumeToken(token)).await;
            (Some(hash), displaced)
        }
        None => (None, Arc::default()),
    };
    let mut lost = false;
    // Once: the system refuses a second restart while one is under way.
    let mut restarting = false;

    let result = loop {
        tokio::select! {
            msg = control.recv() => match msg {
                Ok(Control::RequestKeyframe) => {
                    if let Some(screen) = screen.lock().expect("lock").as_ref() {
                        screen.request_keyframe();
                    }
                }
                Ok(Control::ResendVideo { frame, packets }) => {
                    let _ = resend_tx.try_send(ResendRequest { frame, packets });
                }
                Ok(Control::Bye) => {
                    // Closing here is what lets the client's `bye()` return.
                    conn.close(0u32.into(), b"bye");
                    break Ok(());
                }
                Ok(Control::Restart) => {
                    restarting = restarting || restart(&config, &session, token_hash.as_ref());
                    let ok = restarting;
                    if let Err(e) = control.send(&Control::RestartAnswer { ok }).await {
                        break Err(e.into());
                    }
                }
                // Refused here: a modified viewer gets nowhere.
                Ok(Control::Clipboard { text }) => {
                    if session.permissions.clipboard && text.len() <= MAX_CLIPBOARD_TEXT
                        && let Some(screen) = screen.lock().expect("lock").as_mut()
                    {
                        screen.set_clipboard(text);
                    }
                }
                Ok(Control::FileAnswer { id, outcome }) => {
                    let _ = file_answers.send((id, outcome));
                }
                Ok(other) => debug!("mensagem inesperada: {other:?}"),
                // A peer that left is a clean end; anything else is an error.
                Err(e) if e.is_peer_closed() => break Ok(()),
                Err(e) => break Err(e.into()),
            },
            Some(text) = clipboard_rx.recv() => {
                if let Err(e) = control.send(&Control::Clipboard { text }).await {
                    break Err(e.into());
                }
            }
            Some((id, outcome)) = file_outcomes.recv() => {
                if let Err(e) = control.send(&Control::FileAnswer { id, outcome }).await {
                    break Err(e.into());
                }
            }
            _ = conn.closed() => break Ok(()),
            _ = displaced.notified() => {
                info!("o técnico voltou por outra conexão");
                lost = true;
                break Ok(());
            }
            _ = end.notified() => {
                info!("sessão encerrada no host");
                if let Some(restart) = &config.restart {
                    restart.revoke();
                }
                // Reliably, unlike the closing itself: a viewer that missed
                // it would wait for a return that will not happen.
                let told = async {
                    let _ = control.send(&Control::Bye).await;
                    conn.closed().await;
                };
                let _ = tokio::time::timeout(BYE_WAIT, told).await;
                conn.close(0u32.into(), ENDED_ON_HOST);
                break Ok(());
            }
            // Without video or input the session is useless: end it.
            done = &mut writer => break task_end("vídeo", done.map(|r| r.map_err(SessionError::from))),
            done = &mut input => break task_end("input", done),
        }
    };

    session_on.store(false, Ordering::Relaxed);
    // (Asked before closing, which would hide how the connection ended.)
    lost |= matches!(
        conn.close_reason(),
        Some(ConnectionError::TimedOut | ConnectionError::Reset)
    );
    if let Some(hash) = &token_hash {
        if lost {
            config.resumes.lost(hash);
        } else {
            config.resumes.forget(hash);
        }
    }

    writer.abort();
    // (A finished task was already awaited by the loop and must not be
    // polled again.)
    if !input.is_finished() {
        input.abort();
        let _ = (&mut input).await;
    }
    conn.close(0u32.into(), b"session ended");
    // Releases any keys still held and stops the capture.
    let screen = screen.lock().expect("lock").take();
    if let Some(screen) = screen {
        screen.close().await;
    }
    info!("desconectado: {}", client.device_name);
    result.map(|()| client)
}

/// Shows how each file goes while the session lasts, and writes down the
/// ones that went, for the owner of a computer nobody was at.
fn tell_about_files(
    presence: Option<Arc<dyn Presence>>,
    audit: Option<Arc<dyn UnattendedPassword>>,
    who: String,
    session_on: Arc<AtomicBool>,
) -> Tell {
    Arc::new(move |transfer: Transfer| {
        if let Some(presence) = presence
            .as_ref()
            .filter(|_| session_on.load(Ordering::Relaxed))
        {
            presence.transfer(&transfer);
        }
        if let (Some(audit), Stage::Done) = (&audit, &transfer.stage) {
            let done = match transfer.direction {
                Direction::Sending => "arquivo enviado",
                Direction::Receiving => "arquivo recebido",
            };
            audit.audit(&format!("{done}: \"{}\", \"{who}\"", transfer.name));
        }
    })
}

/// Restarts the computer for this session's viewer, if it may. The session
/// goes on until the computer goes down, and is not lost if that is called
/// off on the host.
fn restart(config: &HostConfig, session: &ActiveSession, token: Option<&crate::TokenHash>) -> bool {
    let kept = token.and_then(|hash| config.resumes.kept(hash));
    let (true, Some(restart), Some(kept)) = (session.permissions.restart, &config.restart, kept)
    else {
        warn!("pedido para reiniciar recusado: {}", session.name);
        return false;
    };
    let who = format!(
        "\"{}\" (ID {})",
        session.name,
        session.id.as_deref().unwrap_or("")
    );
    match restart.restart(&kept, &who) {
        Ok(()) => true,
        Err(e) => {
            warn!("reiniciar: {e}");
            false
        }
    }
}

/// What the capture did in a second, for the log (`hext_video=debug`): when
/// the frame rate falls and the network was not busy, this says why.
struct CaptureReport {
    since: Instant,
    /// Images that differed from the one before.
    changed: u32,
    /// Images encoded, and those the encoder held back.
    encoded: u32,
    skipped: u32,
    /// The longest encode, and the time waiting for the network to take
    /// frames.
    longest_encode: Duration,
    network: Duration,
}

impl CaptureReport {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            changed: 0,
            encoded: 0,
            skipped: 0,
            longest_encode: Duration::ZERO,
            network: Duration::ZERO,
        }
    }

    fn roll(&mut self) {
        if self.since.elapsed() < Duration::from_secs(1) {
            return;
        }
        debug!(
            target: LOG_TARGET,
            "captura: {} imagens novas, {} codificadas (a mais lenta em {} ms), {} seguradas pelo \
             codificador, {} ms esperando a rede",
            self.changed,
            self.encoded,
            self.longest_encode.as_millis(),
            self.skipped,
            self.network.as_millis(),
        );
        *self = Self::new();
    }
}

fn capture_loop(
    config: &CaptureSettings,
    shared: &Shared,
    frames: &mpsc::Sender<VideoFrame>,
    started: oneshot::Sender<Result<()>>,
) {
    let opened = hext_capture::open(
        config.display,
        config.test_pattern,
        config.encoder.max_fps.round() as u32,
    )
    .map_err(SessionError::from)
    .and_then(|capturer| Ok((capturer, hext_codec::open_encoder(config.encoder)?)));
    let (mut capturer, mut encoder) = match opened {
        Ok(opened) => {
            let _ = started.send(Ok(()));
            opened
        }
        Err(e) => {
            let _ = started.send(Err(e));
            return;
        }
    };
    info!("codificador: {}", encoder.name());
    let mut hardware = encoder.is_hardware();
    let min_interval = Duration::from_secs_f32(1.0 / config.encoder.max_fps.max(1.0));
    let mut last_encode: Option<Instant> = None;
    let mut seq = 0u64;
    // Rate control skipped the last image; it must still be sent.
    let mut skipped = false;
    let mut report = CaptureReport::new();

    while !shared.stop.load(Ordering::Relaxed) {
        report.roll();
        // Respect the frame-rate cap; changes accumulate in the meantime.
        if let Some(last) = last_encode {
            let since = last.elapsed();
            if since < min_interval {
                std::thread::sleep(min_interval - since);
            }
        }

        // The frame-rate cap counts from the start of each frame's work.
        let tick = Instant::now();
        // Owed work (a skipped image, a keyframe) must not wait for a change.
        let owed = skipped || shared.keyframe.load(Ordering::Relaxed);
        let timeout = if owed {
            Duration::ZERO
        } else {
            Duration::from_millis(100)
        };
        let changed = match capturer.next_frame(timeout) {
            Ok(changed) => changed,
            Err(e) => {
                warn!("captura: {e}");
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        let rect = capturer.rect();
        *shared.area.lock().expect("lock") = Area {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        };

        report.changed += u32::from(changed);
        let forced = shared.keyframe.swap(false, Ordering::Relaxed);
        if forced {
            encoder.force_keyframe();
        }
        // A still screen costs nothing, unless the viewer needs a keyframe
        // or the latest image was skipped.
        if !changed && !forced && !skipped {
            if encoder.pending() {
                // The last images are still inside the encoder: collect them
                // without feeding it new input.
                let started = Instant::now();
                match encoder.drain() {
                    Ok(Some(encoded)) => {
                        let Some(frame) = capturer.frame() else {
                            continue;
                        };
                        let waited = Instant::now();
                        let sent = send_frame(
                            frames,
                            &mut seq,
                            frame,
                            encoded,
                            capturer.copy_time(),
                            started.elapsed(),
                            hardware,
                        );
                        report.network += waited.elapsed();
                        if !sent {
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!("encode: {e}");
                        // Encode the latest image again (with the fallback encoder).
                        skipped = true;
                    }
                }
            }
            continue;
        }
        let Some(frame) = capturer.frame() else {
            // Nothing captured yet: keep a pending keyframe request alive.
            if forced {
                shared.keyframe.store(true, Ordering::Relaxed);
            }
            continue;
        };

        let started = Instant::now();
        let encoded = match encoder.encode(&image(frame)) {
            Ok(Some(encoded)) => encoded,
            Ok(None) => {
                skipped = true;
                report.skipped += 1;
                last_encode = Some(tick);
                continue;
            }
            Err(e) => {
                warn!("encode: {e}");
                // Don't drop a keyframe request: retry on the next loop
                // (owed = true keeps the timeout at zero).
                if forced {
                    shared.keyframe.store(true, Ordering::Relaxed);
                }
                continue;
            }
        };
        let encode_time = started.elapsed();
        report.encoded += 1;
        report.longest_encode = report.longest_encode.max(encode_time);
        last_encode = Some(tick);
        skipped = false;
        if hardware != encoder.is_hardware() {
            hardware = encoder.is_hardware();
            warn!("codificador agora: {}", encoder.name());
        }

        let waited = Instant::now();
        let sent = send_frame(
            frames,
            &mut seq,
            frame,
            encoded,
            capturer.copy_time(),
            encode_time,
            hardware,
        );
        report.network += waited.elapsed();
        if !sent {
            break;
        }
    }
}

/// The captured frame as encoder input.
fn image(frame: &hext_capture::Frame) -> hext_codec::Image<'_> {
    hext_codec::Image {
        width: frame.width,
        height: frame.height,
        format: match frame.format {
            hext_capture::PixelFormat::Bgra => hext_codec::PixelFormat::Bgra,
            hext_capture::PixelFormat::Nv12 => hext_codec::PixelFormat::Nv12,
        },
        pixels: gpu_texture(frame).unwrap_or(hext_codec::Pixels::Memory(&frame.data)),
    }
}

/// The frame's GPU texture, where the capture left the image on the GPU.
#[cfg(windows)]
fn gpu_texture(frame: &hext_capture::Frame) -> Option<hext_codec::Pixels<'_>> {
    frame.texture.as_ref().map(hext_codec::Pixels::D3d11)
}

#[cfg(not(windows))]
fn gpu_texture(_frame: &hext_capture::Frame) -> Option<hext_codec::Pixels<'_>> {
    None
}

/// Hands an encoded frame to the network task; `false` once the session is over.
fn send_frame(
    frames: &mpsc::Sender<VideoFrame>,
    seq: &mut u64,
    frame: &hext_capture::Frame,
    encoded: hext_codec::Encoded,
    capture_time: Duration,
    encode_time: Duration,
    hardware: bool,
) -> bool {
    let msg = VideoFrame {
        seq: *seq,
        width: frame.width,
        height: frame.height,
        keyframe: encoded.keyframe,
        capture_micros: micros(capture_time),
        encode_micros: micros(encode_time),
        hardware,
        data: encoded.data,
    };
    *seq += 1;
    frames.blocking_send(msg).is_ok()
}

/// How a video or input task ending affects the session: a peer that left
/// is a normal end, anything else is an error.
fn task_end(
    what: &str,
    done: std::result::Result<Result<()>, tokio::task::JoinError>,
) -> Result<()> {
    match done {
        Ok(Ok(())) => Ok(()),
        Ok(Err(SessionError::Net(e))) if e.is_peer_closed() => Ok(()),
        Ok(Err(e)) => Err(SessionError::Protocol(format!("{what}: {e}"))),
        Err(e) => Err(SessionError::Protocol(format!("{what}: {e}"))),
    }
}

type SharedScreen = Arc<Mutex<Option<Box<dyn Screen>>>>;

/// Injects the client's input until the stream or connection ends; without
/// the permission to control, the input is read and dropped.
async fn receive_input(
    conn: Connection,
    screen: SharedScreen,
    permissions: Permissions,
    file_end: FileEnd,
) -> Result<()> {
    let (kind, mut stream) = hext_net::accept_uni(&conn).await?;
    if kind != StreamKind::Input {
        return Err(SessionError::Protocol(format!(
            "unexpected {kind:?} stream"
        )));
    }
    // Only now: the input stream is the first, and the next ones are these.
    // (Not stopped with the session: the connection ending ends it, and it
    // leaves no temporary file behind.)
    tokio::spawn(files::run(file_end));
    loop {
        let event: InputEvent = stream.recv().await?;
        if !permissions.control {
            continue;
        }
        if let Some(screen) = screen.lock().expect("lock").as_mut() {
            screen.inject(&event);
        }
    }
}

fn micros(d: Duration) -> u32 {
    d.as_micros().min(u32::MAX as u128) as u32
}
