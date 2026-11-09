//! The viewer: a window showing the host's screen, forwarding input.
//!
//! winit owns the main thread (required on macOS); networking runs on a
//! tokio runtime and decoding on its own thread. Each decoded frame is
//! handed over through `latest` and the event loop is woken to draw it.

use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use anyhow::{Context, Result, bail};
use hext_clipboard::Clipboard;
use hext_codec::{DecoderPreference, YuvFrame};
use hext_input::keymap::{self, CommandKey};
use hext_net::{Connection, Endpoint, EndpointConfig, NetError, PasswordLogin};
use hext_proto::Os;
use hext_proto::{
    ChatText, Displays, Hello, HelloAsks, InputEvent, MouseButton, PeerInfo, RejectReason,
};
use hext_render::{
    ButtonAction, DisplayBar, Renderer, VideoRect, button_rect, chat_button_rect, handle_rect,
};
use hext_session::client::{self, ClientHandle};
use hext_session::files::{Confirm, OwnFiles, Stage, Transfer, size_text};
use hext_session::{NETWORK_GRACE, ResumeToken, SessionError};
use hext_signal::chain::BindAsk;
use hext_signal::{DeviceId, KnownHosts};
use tracing::{info, warn};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Modifiers, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, ModifiersKeyState, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};
use zeroize::Zeroizing;

use crate::loop_watch::timed;
use crate::signaling::Pin;
use crate::ui_link::ToViewer;
use crate::{APP_VERSION, CmdKey, CodecChoice, CtrlKey, ViewArgs, device_name, parse_addr};

/// Trackpad scrolling arrives in pixels; this many make one wheel notch.
const PIXELS_PER_NOTCH: f64 = 50.0;
/// How long the button stays up after entering full screen, so the way out
/// is seen once.
const BUTTON_HINT: Duration = Duration::from_secs(4);
/// The gap between the handle and the buttons, in logical pixels.
const BUTTON_GAP: f64 = 8.0;
/// How long the buttons stay after the mouse leaves them: one passing by
/// does not make them blink.
const BUTTON_LINGER: Duration = Duration::from_millis(800);
/// How long the second Ctrl+Alt+R has to come.
const RESTART_CONFIRM: Duration = Duration::from_secs(5);
/// How long the title waits for the host's answer about restarting.
const RESTART_ANSWER: Duration = Duration::from_secs(10);
/// Waits between attempts to come back, doubling up to the last. (The server
/// takes 10 requests a minute from a device.)
const BACK_FIRST: Duration = Duration::from_secs(2);
const BACK_MAX: Duration = Duration::from_secs(10);
/// How long the title says how a file ended.
const TRANSFER_SHOWN: Duration = Duration::from_secs(8);
/// How long the mouse waits for the host to change displays.
const SWITCH_WAIT: Duration = Duration::from_secs(2);
/// How long a viewer of the interface stays with no window, for the next
/// computer asked for: one leaving just as it is asked would lose it.
const IDLE_STAY: Duration = Duration::from_secs(60);

/// News of one session, for the window loop.
enum SessionEvent {
    Frame(Instant),
    SessionEnded,
    /// The attempt to come back ended.
    Back(Box<Result<ClientHandle>>),
    /// News of the host's displays.
    Displays,
    /// The person wrote in the chat window.
    ChatOut(ChatText),
    /// News of a file.
    Transfer(Transfer),
    /// The host wants to send this file: receive it?
    AskFile {
        text: String,
        answer: oneshot::Sender<bool>,
    },
    /// (While opening) the host's screen is locked: someone must wake it.
    Locked,
}

enum UserEvent {
    /// About a session, by its number.
    Session(u64, SessionEvent),
    /// That session is open, or could not be.
    Opened(u64, Box<Result<Started>>),
    /// The interface asks for something.
    Ui(ToViewer),
    /// The interface is gone: nobody will ask for more.
    UiGone,
}

/// Hands one session's news to the window loop.
#[derive(Clone)]
struct Tell {
    session: u64,
    proxy: EventLoopProxy<UserEvent>,
}

impl Tell {
    fn send(&self, event: SessionEvent) {
        let _ = self
            .proxy
            .send_event(UserEvent::Session(self.session, event));
    }
}

/// What every session of this viewer shares: one key, one endpoint. Our
/// relay serves one connection per key, so two endpoints (two viewers)
/// would keep knocking each other off.
struct Net {
    key: hext_crypto::DeviceKey,
    /// The signaling server, to connect by ID.
    server: Option<String>,
    /// The hosts' keys, as first seen (by ID only).
    known: Option<KnownHosts>,
    relay: Option<hext_net::RelayUrl>,
    /// The relay the server names at login (the account's), or the build's.
    relays: hext_net::RelaySwitch,
    /// Bound on first use: by ID, only once logged in (and so registered,
    /// which the default relay requires).
    endpoint: tokio::sync::OnceCell<Endpoint>,
}

impl Net {
    fn new(args: &ViewArgs, target: &Target) -> Result<Self> {
        let key = crate::device_key(args.device_key.as_deref())?;
        let server = crate::server(&args.server).map(str::to_owned);
        let (known, relay) = match target {
            Target::Id(_) => {
                // Next to the key, so each key (profile) has its own.
                let known = match &args.device_key {
                    Some(path) => KnownHosts::new(path.with_file_name("known_hosts")),
                    None => KnownHosts::default_location()?,
                };
                (
                    Some(known),
                    crate::relay_for(&args.relay, server.as_deref())?,
                )
            }
            // Not registered through a server: no default relay.
            Target::Ip { .. } => (None, crate::relay_for(&args.relay, None)?),
        };
        Ok(Self {
            key,
            server,
            known,
            relays: hext_net::RelaySwitch::new(relay.clone()),
            relay,
            endpoint: tokio::sync::OnceCell::new(),
        })
    }

    async fn endpoint(&self) -> Result<&Endpoint> {
        self.endpoint
            .get_or_try_init(|| bind(&self.key, self.relay.clone()))
            .await
    }
}

/// What it takes to reach the host again: only by ID.
struct Route {
    net: Arc<Net>,
    id: DeviceId,
}

/// A session just opened.
struct Started {
    session: ClientHandle,
    route: Option<Route>,
}

/// How to open the session again.
struct Back {
    route: Route,
    decoder: DecoderPreference,
    latest: Latest,
    tell: Tell,
}

/// Where the decoder leaves pictures for the window.
type Latest = Arc<Pictures>;

/// The newest picture, and the host's display it shows; older ones are
/// dropped unseen. The window hears of it once until it takes it: on
/// Windows, each word to the window is a message ahead of the mouse in its
/// queue, and one per frame, faster than the window draws them (a burst
/// after the network stalls, a GPU waiting for the screen), held clicks and
/// drags for as long as the queue took to drain: a window that would not
/// move or maximize.
#[derive(Default)]
struct Pictures {
    newest: Mutex<Option<(YuvFrame, u8)>>,
    /// The window was told of a picture it has not taken yet.
    told: AtomicBool,
}

impl Pictures {
    /// Leaves `frame`; whether the window must hear of it.
    fn put(&self, frame: YuvFrame, display: u8) -> bool {
        *self.newest.lock().expect("lock") = Some((frame, display));
        !self.told.swap(true, Ordering::AcqRel)
    }

    /// The newest picture, if one came since the last time.
    fn take(&self) -> Option<(YuvFrame, u8)> {
        // Before taking: a picture left meanwhile is told again.
        self.told.store(false, Ordering::Release);
        self.newest.lock().expect("lock").take()
    }
}

/// How a session's window ends.
enum Ending {
    /// Closed, or the host ended it.
    Over,
    Failed(anyhow::Error),
}

pub fn run(args: ViewArgs) -> Result<()> {
    let password = read_password(&args)?;
    // The arguments first, before any network.
    let target = target(&args)?;
    if password.is_some() && !matches!(target, Target::Id(_)) {
        bail!("a senha de acesso não assistido só vale conectando pelo ID");
    }
    let net = Arc::new(Net::new(&args, &target)?);
    let runtime = tokio::runtime::Runtime::new()?;
    if args.headless {
        return runtime.block_on(headless(net, target, &args, password));
    }

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    if args.report_to_ui {
        listen_to_ui(proxy.clone());
    }
    let mut viewers = Viewers {
        runtime,
        net,
        decoder: decoder(args.decoder),
        cmd_key: args.cmd_key,
        ctrl_key: args.ctrl_key,
        ui: args.report_to_ui,
        proxy,
        sessions: HashMap::new(),
        windows: HashMap::new(),
        opening: HashMap::new(),
        focus: Focus::default(),
        resumed: false,
        idle_since: None,
        ui_gone: false,
        closing: Vec::new(),
        failure: None,
    };
    viewers.open(args.session, target, password);
    let ran = event_loop.run_app(&mut viewers);
    finish_ui_out();
    ran?;
    match viewers.failure.take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Every session of this viewer, each in its window.
struct Viewers {
    runtime: tokio::runtime::Runtime,
    net: Arc<Net>,
    decoder: DecoderPreference,
    cmd_key: CmdKey,
    ctrl_key: CtrlKey,
    /// Opened by the interface: it takes a line about each session, and may
    /// ask for more.
    ui: bool,
    proxy: EventLoopProxy<UserEvent>,
    sessions: HashMap<u64, Viewer>,
    windows: HashMap<WindowId, u64>,
    /// Sessions on their way: where their pictures will go, and how to give
    /// up on them.
    opening: HashMap<u64, (Latest, tokio::task::AbortHandle)>,
    /// The session whose window has the focus now: the only one that trades
    /// clipboards, or one customer's text could reach another (or what is
    /// copied here, while working elsewhere, reach the host).
    focus: Focus,
    /// Windows can be made (not before the loop starts).
    resumed: bool,
    /// With nothing open, since when.
    idle_since: Option<Instant>,
    /// The interface that opened this is gone.
    ui_gone: bool,
    /// Sessions closing, waited for before leaving.
    closing: Vec<tokio::task::JoinHandle<()>>,
    /// (Without the interface) why the last session could not open, or come
    /// back: what this ends with.
    failure: Option<anyhow::Error>,
}

impl Viewers {
    /// Opens session `n` in the background.
    fn open(&mut self, n: u64, target: Target, password: Option<Login>) {
        if self.sessions.contains_key(&n) || self.opening.contains_key(&n) {
            return;
        }
        self.idle_since = None;
        let latest = Latest::default();
        let tell = Tell {
            session: n,
            proxy: self.proxy.clone(),
        };
        let (net, decoder, proxy) = (self.net.clone(), self.decoder, self.proxy.clone());
        let pictures = latest.clone();
        let task = self.runtime.spawn(async move {
            let opened = open_session(net, target, password, decoder, pictures, tell).await;
            let _ = proxy.send_event(UserEvent::Opened(n, Box::new(opened)));
        });
        self.opening.insert(n, (latest, task.abort_handle()));
    }

    /// Session `n` opened: its window comes up.
    fn opened(&mut self, event_loop: &ActiveEventLoop, n: u64, started: Started) {
        // (Given up on meanwhile: it closes as it drops.)
        let Some((latest, _)) = self.opening.remove(&n) else {
            return;
        };
        let Started { mut session, route } = started;
        info!(
            "conectado a {} ({:?}, v{})",
            session.host.device_name, session.host.os, session.host.app_version
        );
        if !session.permissions.control {
            info!("o host permitiu só ver a tela");
        }
        let tell = Tell {
            session: n,
            proxy: self.proxy.clone(),
        };
        let runtime = self.runtime.handle().clone();
        runtime.spawn(end_of(&session, tell.clone()));
        let clipboard_tell = Arc::new(tokio::sync::Notify::new());
        runtime.spawn(share_clipboard(
            &mut session,
            n,
            self.focus.clone(),
            clipboard_tell.clone(),
        ));
        runtime.spawn(share_files(&mut session, tell.clone()));
        runtime.spawn(hear_displays(&session, tell.clone()));
        if self.ui {
            runtime.spawn(hear_chat(&mut session, n));
            // (Made safe to show when the session opened.)
            let name = &session.host.device_name;
            tell_ui(n, &format!("{} {name}", crate::ui_link::VIEWER_CONNECTED));
        }
        let host_is_mac = session.host.os == Os::MacOs;
        let back = route.map(|route| {
            Arc::new(Back {
                route,
                decoder: self.decoder,
                latest: latest.clone(),
                tell: tell.clone(),
            })
        });
        let viewer = Viewer {
            n,
            runtime,
            session: Some(session),
            endpoint: self.net.endpoint.get().cloned(),
            latest,
            back,
            coming_back: None,
            restart_confirm: None,
            restart_sent: None,
            transfer: None,
            closing: None,
            focus: WindowFocus::new(n, self.focus.clone()),
            clipboard_tell,
            tell,
            window: None,
            renderer: None,
            command_key: match self.cmd_key {
                // A Mac host understands Command itself.
                _ if host_is_mac => CommandKey::Win,
                CmdKey::Ctrl => CommandKey::Ctrl,
                CmdKey::Win => CommandKey::Win,
            },
            swap_ctrl_and_super: host_is_mac
                && !cfg!(target_os = "macos")
                && self.ctrl_key == CtrlKey::Cmd,
            modifiers: Modifiers::default(),
            local_key: None,
            cursor: None,
            button: None,
            button_hint: None,
            button_pressed: false,
            button_near: false,
            handle: None,
            displays: Displays::default(),
            display_bar: None,
            switching: None,
            picture: None,
            chat_link: self.ui,
            chat_button: None,
            last_title: Instant::now(),
            redraw_asked: None,
        };
        // In use once its window has the focus (it tells).
        self.sessions.insert(n, viewer);
        if self.resumed {
            self.make_window(event_loop, n);
        }
    }

    fn make_window(&mut self, event_loop: &ActiveEventLoop, n: u64) {
        if let Some(viewer) = self.sessions.get_mut(&n)
            && let Some(id) = viewer.open_window(event_loop)
        {
            self.windows.insert(id, n);
        }
        self.settle(event_loop, n);
    }

    /// Session `n` could not open: says why.
    fn failed(&mut self, event_loop: &ActiveEventLoop, n: u64, e: anyhow::Error) {
        if self.opening.remove(&n).is_none() {
            return;
        }
        self.report(n, Ending::Failed(e));
        self.exit_if_done(event_loop);
    }

    /// Says how session `n` ended: to the interface, or as this viewer's
    /// result.
    fn report(&mut self, n: u64, ending: Ending) {
        match (ending, self.ui) {
            (Ending::Over, true) => tell_ui(n, crate::ui_link::VIEWER_ENDED),
            (Ending::Failed(e), true) => {
                let reason = format!("{e:#}").replace(['\r', '\n'], " ");
                tell_ui(n, &format!("{} {reason}", crate::ui_link::VIEWER_ENDED));
            }
            (Ending::Over, false) => {}
            (Ending::Failed(e), false) => self.failure = Some(e),
        }
    }

    /// A session that is over goes, with its window.
    fn settle(&mut self, event_loop: &ActiveEventLoop, n: u64) {
        let Some(ending) = self.sessions.get_mut(&n).and_then(|v| v.closing.take()) else {
            return;
        };
        let Some(mut viewer) = self.sessions.remove(&n) else {
            return;
        };
        if let Some(window) = viewer.window.take() {
            self.windows.remove(&window.id());
        }
        if let Some(session) = viewer.session.take() {
            self.closing.push(self.runtime.spawn(async move {
                if let Err(e) = session.close().await {
                    warn!("encerrando: {e}");
                }
            }));
        }
        self.report(n, ending);
        self.exit_if_done(event_loop);
    }

    /// With nothing open or opening, the viewer is done: at once, unless the
    /// interface may still ask for more ([`IDLE_STAY`]).
    fn exit_if_done(&mut self, event_loop: &ActiveEventLoop) {
        if !(self.sessions.is_empty() && self.opening.is_empty()) {
            return;
        }
        let stays = self.ui && !self.ui_gone;
        let since = *self.idle_since.get_or_insert_with(Instant::now);
        if !stays || since.elapsed() >= IDLE_STAY {
            event_loop.exit();
        }
    }

    /// What the interface asks for.
    fn asked(&mut self, event_loop: &ActiveEventLoop, asked: ToViewer) {
        match asked {
            ToViewer::Open {
                session,
                id,
                password,
                remember,
                account,
            } => {
                let login = password.map(|password| Login {
                    password,
                    remember,
                    account,
                });
                self.open(session, Target::Id(id), login);
            }
            ToViewer::Focus(n) => {
                if let Some(window) = self.sessions.get(&n).and_then(|v| v.window.as_ref()) {
                    window.set_minimized(false);
                    window.focus_window();
                }
            }
            ToViewer::Close(n) => {
                if let Some(viewer) = self.sessions.get_mut(&n) {
                    viewer.closing = Some(Ending::Over);
                    self.settle(event_loop, n);
                } else if let Some((_, task)) = self.opening.remove(&n) {
                    // Leaving the server takes the request back.
                    task.abort();
                    self.report(n, Ending::Over);
                    self.exit_if_done(event_loop);
                }
            }
            ToViewer::Chat(n, text) => match self.sessions.get_mut(&n) {
                Some(viewer) => viewer.on_event(SessionEvent::ChatOut(text)),
                None => tell_ui(n, crate::ui_link::VIEWER_CHAT_NO_ONE),
            },
        }
    }
}

struct Viewer {
    /// The session's number (the interface's).
    n: u64,
    runtime: tokio::runtime::Handle,
    /// `None` while coming back.
    session: Option<ClientHandle>,
    /// Shared by every session: to come back by.
    endpoint: Option<Endpoint>,
    latest: Latest,
    /// How to open the session again; `None`: connected by IP.
    back: Option<Arc<Back>>,
    /// What the title says while coming back.
    coming_back: Option<&'static str>,
    /// The first Ctrl+Alt+R counts until then.
    restart_confirm: Option<Instant>,
    /// When the host was asked to restart.
    restart_sent: Option<Instant>,
    /// What the title says of the files, and until when (`None`: until the
    /// next news).
    transfer: Option<(String, Option<Instant>)>,
    /// The window is done, and how.
    closing: Option<Ending>,
    /// Whether this window has the focus, for the clipboard.
    focus: WindowFocus,
    /// Asks the clipboard to tell its content now.
    clipboard_tell: Arc<tokio::sync::Notify>,
    tell: Tell,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    command_key: CommandKey,
    /// Windows/Linux keyboard on a Mac host: Ctrl acts as Command.
    swap_ctrl_and_super: bool,
    /// Ctrl, Alt... held on this keyboard (for Ctrl+Alt+End).
    modifiers: Modifiers,
    /// The key of a shortcut of the viewer: its repeats and release are not
    /// keys for the host.
    local_key: Option<KeyCode>,
    /// Where the mouse is in the window.
    cursor: Option<(f64, f64)>,
    /// The full screen button on screen, if any.
    button: Option<(ButtonAction, VideoRect)>,
    /// The button stays up until then, wherever the mouse is.
    button_hint: Option<Instant>,
    /// The button took a press: its release is not for the host either.
    button_pressed: bool,
    /// The mouse was on the buttons (or on the handle) the last time.
    button_near: bool,
    /// The handle on screen: it stands for the buttons while they are folded.
    handle: Option<VideoRect>,
    /// The host's displays, as last told (kept while coming back).
    displays: Displays,
    /// The numbered buttons on screen, next to the full screen one.
    display_bar: Option<DisplayBar>,
    /// The display asked for, and when.
    switching: Option<(u8, Instant)>,
    /// The display in the picture on screen.
    picture: Option<u8>,
    /// Opened by the interface, whose windows hold the chat.
    chat_link: bool,
    /// The chat button on screen.
    chat_button: Option<VideoRect>,
    last_title: Instant,
    /// Since when a picture waits for the window to paint.
    redraw_asked: Option<Instant>,
}

impl Viewer {
    /// Ctrl and the left Alt held: AltGr (right Alt, which Windows reports
    /// with Ctrl) is a character key on ABNT2 and European keyboards.
    fn ctrl_alt(&self) -> bool {
        let state = self.modifiers.state();
        let pressed = |key| key == ModifiersKeyState::Pressed;
        // An Alt not reported as the right one (some platforms do not say
        // which side) counts as the left.
        let left_alt = pressed(self.modifiers.lalt_state())
            || (state.alt_key() && !pressed(self.modifiers.ralt_state()));
        state.control_key() && left_alt
    }

    /// Ctrl+Alt+R: the first says what it does, the second does it. A window
    /// in full screen shows no title, so it comes out of it.
    fn ask_restart(&mut self) {
        let now = Instant::now();
        let confirmed = self.restart_confirm.take().is_some_and(|until| until > now);
        if confirmed {
            if let Some(session) = &self.session {
                session.request_restart();
                self.restart_sent = Some(now);
            }
        } else {
            self.restart_confirm = Some(now + RESTART_CONFIRM);
            self.leave_fullscreen();
        }
        self.last_title = now - Duration::from_secs(1);
        self.update_title();
    }

    /// Puts `text` on the title, until `until`.
    fn show_transfer(&mut self, text: String, until: Option<Instant>) {
        self.transfer = Some((text, until));
        self.update_title();
    }

    /// A file was dropped on the window.
    fn send_file(&mut self, path: std::path::PathBuf) {
        let until = Some(Instant::now() + TRANSFER_SHOWN);
        match &self.session {
            Some(session) if session.permissions.files => session.send_file(path),
            Some(_) => self.show_transfer("Sem permissão para enviar arquivos".into(), until),
            None => self.show_transfer("Sem conexão: arquivo não enviado".into(), until),
        }
    }

    fn leave_fullscreen(&mut self) {
        if self
            .window
            .as_ref()
            .is_some_and(|w| w.fullscreen().is_some())
        {
            self.toggle_fullscreen();
        }
    }

    /// The session ended and the host expects it back: tries, with the
    /// window as it is, until it opens or `token` runs out.
    fn come_back(
        &mut self,
        back: Arc<Back>,
        endpoint: Endpoint,
        token: ResumeToken,
        time: Duration,
    ) {
        self.coming_back = Some(if time > NETWORK_GRACE {
            "Aguardando a máquina voltar…"
        } else {
            "Conexão perdida, reconectando…"
        });
        self.restart_confirm = None;
        self.restart_sent = None;
        if let Some(session) = self.session.take() {
            self.runtime.spawn(async move {
                let _ = session.close().await;
            });
        }
        self.leave_fullscreen();
        self.last_title = Instant::now() - Duration::from_secs(1);
        self.update_title();
        self.runtime.spawn(async move {
            let result = reconnect(&back, &endpoint, token, time).await;
            back.tell.send(SessionEvent::Back(Box::new(result)));
        });
    }

    fn send(&self, event: InputEvent) {
        if self.switching().is_some() && aimed_at_the_picture(&event) {
            return;
        }
        // (The host refuses it anyway; this saves sending it.)
        if let Some(session) = self.session.as_ref().filter(|s| s.permissions.control) {
            session.send_input(event);
        }
    }

    /// Asks the host for display `index`. It takes no permission: whoever
    /// sees the screen sees them all.
    fn select_display(&mut self, index: u8) {
        let aimed = self.switching().unwrap_or(self.displays.current);
        if let Some(session) = &self.session
            && index < self.displays.count
            && index != aimed
        {
            session.select_display(index);
            self.switching = Some((index, Instant::now()));
        }
    }

    /// The display coming, while the picture on screen is still of another.
    fn switching(&self) -> Option<u8> {
        let (index, asked) = self.switching?;
        (self.picture != Some(index) && asked.elapsed() < SWITCH_WAIT).then_some(index)
    }

    /// The host's mouse stays where it was until it moves: on the display
    /// left behind, where the next click would land, unseen.
    fn aim_mouse(&self) {
        let rect = self.renderer.as_ref().and_then(Renderer::video_rect);
        if let (Some(rect), Some((x, y))) = (rect, self.cursor)
            && let Some((x, y)) = rect.normalize(x, y)
        {
            self.send(InputEvent::MouseMove { x, y });
        }
    }

    /// Takes what the host told of its displays.
    fn update_displays(&mut self) {
        let Some(told) = self.session.as_ref().map(ClientHandle::displays) else {
            return;
        };
        // (Nothing yet: the session just came back.)
        if told == self.displays || told.count == 0 {
            return;
        }
        let first_time = told.count > 1 && self.displays.count <= 1;
        self.displays = told;
        // There is more to see: let it be known once.
        if first_time {
            self.button_hint = Some(Instant::now() + BUTTON_HINT);
        }
        self.update_button();
        self.last_title = Instant::now() - Duration::from_secs(1);
        self.update_title();
    }

    /// Uploads the newest decoded frame, if any, and redraws.
    fn draw(&mut self) {
        // Check the renderer first: taking the frame before the window exists
        // would discard it, and a still host screen sends no replacement.
        let Some(renderer) = &mut self.renderer else {
            return;
        };
        let mut arrived = false;
        if let Some((frame, display)) = self.latest.take() {
            timed("upload", || renderer.upload(&frame));
            arrived = self.picture.replace(display) != Some(display);
        }
        if let Err(e) = timed("render", || renderer.render()) {
            warn!("render: {e}");
        }
        if arrived {
            self.aim_mouse();
        }
    }

    fn toggle_fullscreen(&mut self) {
        let Some(window) = &self.window else { return };
        let entering = window.fullscreen().is_none();
        // The whole screen; the video keeps its proportions (`letterbox`).
        window.set_fullscreen(entering.then_some(Fullscreen::Borderless(None)));
        self.button_hint = entering.then(|| Instant::now() + BUTTON_HINT);
        self.update_button();
    }

    /// Shows the buttons while the mouse is on them or on the handle, or the
    /// hint lasts. Folded, the handle stands for them, always in sight.
    fn update_button(&mut self) {
        let Some(window) = &self.window else { return };
        let size = window.inner_size();
        let scale = window.scale_factor();
        // Laid out from the right edge (each part goes left of the last),
        // then moved beside the handle.
        let mut rect = button_rect((size.width, size.height), scale);
        rect.x = size.width as f32 - rect.width;
        let Displays { count, current } = self.displays;
        // (As many as the shortcut reaches, whatever the host says.)
        let buttons = count.min(9).into();
        let mut bar = DisplayBar::new(rect, buttons, current.into(), scale).filter(|_| count > 1);
        let left = bar.map_or(rect.x, |bar| bar.rect.x);
        let mut chat = chat_button_rect(left, rect, scale).filter(|_| self.chat_link);
        let mut left = chat.map_or(left, |chat| chat.x);
        // The row, beside the handle, halfway down like it.
        let handle = handle_rect((size.width, size.height), scale);
        let dx = (handle.x + handle.width + (BUTTON_GAP * scale) as f32 - left).floor();
        let dy = ((size.height as f32 - rect.height) / 2.0 - rect.y)
            .max(0.0)
            .floor();
        rect.x += dx;
        rect.y += dy;
        left += dx;
        if let Some(bar) = &mut bar {
            bar.rect.x += dx;
            bar.rect.y += dy;
        }
        if let Some(chat) = &mut chat {
            chat.x += dx;
            chat.y += dy;
        }
        // The buttons call for each other: all up, all down.
        let called = VideoRect {
            x: left,
            width: rect.x + rect.width - left,
            ..rect
        };
        let near = self
            .cursor
            .is_some_and(|(x, y)| button_called(called, handle, self.button.is_some(), x, y));
        if std::mem::replace(&mut self.button_near, near) && !near && self.button_hint.is_none() {
            self.button_hint = Some(Instant::now() + BUTTON_LINGER);
        }
        let action = if window.fullscreen().is_some() {
            ButtonAction::Leave
        } else {
            ButtonAction::Enter
        };
        let shown = near || self.button_hint.is_some();
        let button = shown.then_some((action, rect));
        let bar = bar.filter(|_| shown);
        let chat = chat.filter(|_| shown);
        let handle = (!shown).then_some(handle);
        if self.button != button
            || self.display_bar != bar
            || self.chat_button != chat
            || self.handle != handle
        {
            self.button = button;
            self.display_bar = bar;
            self.chat_button = chat;
            self.handle = handle;
            if let Some(renderer) = &mut self.renderer {
                renderer.set_button(button);
                renderer.set_display_bar(bar);
                renderer.set_chat_button(chat);
                renderer.set_handle(handle);
            }
            self.draw();
        }
    }

    /// The display whose numbered button is under the mouse.
    fn on_display_bar(&self) -> Option<u8> {
        let (x, y) = self.cursor?;
        let index = self.display_bar?.display_at(x, y)?;
        u8::try_from(index).ok()
    }

    /// Whether the mouse is on the chat button.
    fn on_chat_button(&self) -> bool {
        match (self.chat_button, self.cursor) {
            (Some(rect), Some((x, y))) => rect.contains(x, y, 0.0),
            _ => false,
        }
    }

    /// Whether the mouse is on the button.
    fn on_button(&self) -> bool {
        match (self.button, self.cursor) {
            (Some((_, rect)), Some((x, y))) => rect.contains(x, y, 0.0),
            _ => false,
        }
    }

    fn update_title(&mut self) {
        if self.last_title.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_title = Instant::now();
        timed("título", || self.set_title());
    }

    fn set_title(&self) {
        let Some(window) = &self.window else { return };
        if let Some(waiting) = self.coming_back {
            window.set_title(&format!("Hext Remote — {waiting}"));
            return;
        }
        let Some(session) = &self.session else { return };
        if self.restart_confirm.is_some() {
            window.set_title("Hext Remote — Aperte Ctrl+Alt+R de novo para reiniciar o computador");
            return;
        }
        if self.restart_sent.is_some() {
            match session.restart_answer() {
                Some(true) => {
                    window.set_title("Hext Remote — Reiniciando o computador…");
                    return;
                }
                Some(false) => {
                    window.set_title("Hext Remote — O computador não reiniciou");
                    return;
                }
                None => {
                    window.set_title("Hext Remote — Pedindo para reiniciar…");
                    return;
                }
            }
        }
        if let Some((text, _)) = &self.transfer {
            window.set_title(&format!("Hext Remote — {text}"));
            return;
        }
        let mut stats = session.stats.lock().expect("lock");
        let path = path_label(session);
        let view_only = if session.permissions.control {
            ""
        } else {
            " (só ver)"
        };
        let mut title = format!(
            "Hext Remote — {}{view_only}{path}",
            session.host.device_name
        );
        let Displays { count, current } = self.displays;
        if count > 1 {
            title += &format!(" — monitor {} de {count}", u16::from(current) + 1);
        }
        if let Some(snapshot) = stats.snapshot() {
            title += &format!(
                " — {}x{} | {snapshot} | RTT {:.1} ms",
                stats.width,
                stats.height,
                session.rtt().as_secs_f64() * 1000.0
            );
        }
        window.set_title(&title);
    }
}

/// The window's side, called by [`Viewers`].
impl Viewer {
    /// Makes the window, the first time; its ID.
    fn open_window(&mut self, event_loop: &ActiveEventLoop) -> Option<WindowId> {
        if self.window.is_some() {
            return None;
        }
        let host = self
            .session
            .as_ref()
            .map_or_else(String::new, |s| s.host.device_name.clone());
        let attributes = Window::default_attributes()
            .with_title(format!("Hext Remote — {host}"))
            .with_inner_size(LogicalSize::new(1280.0, 720.0));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(e) => {
                self.closing = Some(Ending::Failed(anyhow::anyhow!("janela: {e}")));
                return None;
            }
        };
        match Renderer::new(window.clone()) {
            Ok(renderer) => self.renderer = Some(renderer),
            Err(e) => {
                self.closing = Some(Ending::Failed(e.into()));
                return None;
            }
        }
        // A picture told before the window was there waits for this.
        window.request_redraw();
        let id = window.id();
        self.window = Some(window);
        Some(id)
    }

    fn on_event(&mut self, event: SessionEvent) {
        match event {
            // Drawn when the window paints: on Windows, after the mouse and
            // the keyboard, which a picture never holds back. (With nothing
            // waiting, at once.)
            SessionEvent::Frame(sent) => {
                crate::loop_watch::note("fila (quadro)", sent.elapsed());
                if let Some(window) = &self.window {
                    window.request_redraw();
                    self.redraw_asked.get_or_insert_with(Instant::now);
                }
            }
            SessionEvent::SessionEnded => {
                let ending = self.session.as_ref().and_then(ClientHandle::comes_back);
                match (ending, self.back.clone(), self.endpoint.clone()) {
                    (Some((token, time)), Some(back), Some(endpoint)) => {
                        self.come_back(back, endpoint, token, time);
                    }
                    _ => {
                        info!("sessão encerrada pelo host");
                        self.closing = Some(Ending::Over);
                    }
                }
            }
            SessionEvent::Back(result) => match *result {
                Ok(mut session) => {
                    info!("sessão retomada");
                    // (On the window's thread, outside the runtime.)
                    self.runtime.spawn(end_of(&session, self.tell.clone()));
                    self.runtime.spawn(share_clipboard(
                        &mut session,
                        self.n,
                        self.focus.all.clone(),
                        self.clipboard_tell.clone(),
                    ));
                    self.runtime
                        .spawn(share_files(&mut session, self.tell.clone()));
                    self.runtime
                        .spawn(hear_displays(&session, self.tell.clone()));
                    if self.chat_link {
                        self.runtime.spawn(hear_chat(&mut session, self.n));
                    }
                    // The host starts over on a display of its choosing: back
                    // to where this was. (If it is gone, nothing changes.)
                    let index = self.displays.current;
                    session.select_display(index);
                    self.switching = Some((index, Instant::now()));
                    self.picture = None;
                    self.session = Some(session);
                    self.coming_back = None;
                    self.last_title = Instant::now() - Duration::from_secs(1);
                    self.update_title();
                }
                Err(e) => {
                    let e = e.context("não foi possível voltar à sessão");
                    self.closing = Some(Ending::Failed(e));
                }
            },
            // (Taken right after this, as after any event.)
            SessionEvent::Displays => {}
            SessionEvent::ChatOut(text) => match &self.session {
                Some(session) => session.send_chat(text),
                None => tell_ui(self.n, crate::ui_link::VIEWER_CHAT_NO_ONE),
            },
            // (Only while opening: for [`Viewers`].)
            SessionEvent::Locked => {}
            SessionEvent::Transfer(transfer) => {
                let until = (!matches!(transfer.stage, Stage::Going { .. }))
                    .then(|| Instant::now() + TRANSFER_SHOWN);
                self.show_transfer(transfer.to_string(), until);
            }
            SessionEvent::AskFile { text, answer } => {
                if let Some(window) = &self.window {
                    self.runtime.spawn(ask_file(window, text, answer));
                }
            }
        }
    }

    fn on_window(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.closing = Some(Ending::Over),
            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height);
                }
                self.update_button();
                self.draw();
            }
            WindowEvent::RedrawRequested => {
                if let Some(asked) = self.redraw_asked.take() {
                    crate::loop_watch::note("pintura (quadro)", asked.elapsed());
                }
                self.draw();
            }
            WindowEvent::DroppedFile(path) => self.send_file(path),
            WindowEvent::Focused(false) => {
                self.focus.changed(false);
                self.local_key = None;
                self.send(InputEvent::ReleaseAll);
            }
            WindowEvent::Focused(true) => {
                // Back from working elsewhere: what was copied there is for
                // pasting here.
                if self.focus.changed(true) {
                    self.clipboard_tell.notify_one();
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.cursor = None;
                self.update_button();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Some((position.x, position.y));
                self.update_button();
                let rect = self.renderer.as_ref().and_then(Renderer::video_rect);
                if let Some((x, y)) = rect.and_then(|r| r.normalize(position.x, position.y)) {
                    self.send(InputEvent::MouseMove { x, y });
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == winit::event::MouseButton::Left {
                    let pressed = state == ElementState::Pressed;
                    // (Pressed again: its release got lost, as with `local_key`.)
                    let was_ours = std::mem::replace(&mut self.button_pressed, false);
                    if let Some(index) = self.on_display_bar().filter(|_| pressed) {
                        self.button_pressed = true;
                        self.select_display(index);
                        return;
                    }
                    if pressed && self.on_chat_button() {
                        self.button_pressed = true;
                        tell_ui(self.n, crate::ui_link::VIEWER_CHAT_OPEN);
                        return;
                    }
                    if pressed && self.on_button() {
                        self.button_pressed = true;
                        self.toggle_fullscreen();
                        return;
                    }
                    if was_ours && !pressed {
                        return;
                    }
                }
                let button = match button {
                    winit::event::MouseButton::Left => MouseButton::Left,
                    winit::event::MouseButton::Right => MouseButton::Right,
                    winit::event::MouseButton::Middle => MouseButton::Middle,
                    winit::event::MouseButton::Back => MouseButton::Back,
                    winit::event::MouseButton::Forward => MouseButton::Forward,
                    winit::event::MouseButton::Other(_) => return,
                };
                self.send(InputEvent::MouseButton {
                    button,
                    pressed: state == ElementState::Pressed,
                });
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (x, y) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x, y),
                    MouseScrollDelta::PixelDelta(p) => (
                        (p.x / PIXELS_PER_NOTCH) as f32,
                        (p.y / PIXELS_PER_NOTCH) as f32,
                    ),
                };
                // winit: positive x moves the content right, i.e. scrolls left.
                // The protocol (like Windows) uses positive dx = scroll right.
                let (dx, dy) = (-x, y);
                if dx != 0.0 || dy != 0.0 {
                    self.send(InputEvent::Wheel { dx, dy });
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers,
            // Synthetic keys are what this computer believes held as the
            // window gains focus, and let go as it loses it: never typed in
            // the window, so not the host's. (Pressed there, an Esc held to
            // leave another window interrupted the host's program on every
            // return; losing focus already releases everything.)
            WindowEvent::KeyboardInput {
                is_synthetic: true, ..
            } => {}
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                // Ctrl+Alt+End (or Ctrl+Alt+Delete, where this computer
                // does not keep it): Ctrl+Alt+Del on the host.
                let released = event.state == ElementState::Released;
                if belongs_to_shortcut(&mut self.local_key, code, event.repeat, released) {
                    return;
                }
                if event.state == ElementState::Pressed
                    && matches!(code, KeyCode::End | KeyCode::Delete)
                    && self.ctrl_alt()
                {
                    self.local_key = Some(code);
                    self.send(InputEvent::SecureAttention);
                    return;
                }
                // Ctrl+Alt+R: restart the host, if it allows.
                if event.state == ElementState::Pressed
                    && code == KeyCode::KeyR
                    && self.ctrl_alt()
                    && self.session.as_ref().is_some_and(|s| s.permissions.restart)
                {
                    self.local_key = Some(code);
                    self.ask_restart();
                    return;
                }
                // Ctrl+Alt+1..9: another display of the host. (With no such
                // display, the keys are the host's.)
                if event.state == ElementState::Pressed
                    && self.ctrl_alt()
                    && self.displays.count > 1
                    && let Some(index) = display_key(code).filter(|i| *i < self.displays.count)
                {
                    self.local_key = Some(code);
                    self.select_display(index);
                    return;
                }
                // Ctrl+Alt+C: the chat, where the interface can show it.
                if event.state == ElementState::Pressed
                    && code == KeyCode::KeyC
                    && self.chat_link
                    && self.ctrl_alt()
                {
                    self.local_key = Some(code);
                    tell_ui(self.n, crate::ui_link::VIEWER_CHAT_OPEN);
                    return;
                }
                // Ctrl+Alt+Enter: full screen, and back.
                if event.state == ElementState::Pressed
                    && matches!(code, KeyCode::Enter | KeyCode::NumpadEnter)
                    && self.ctrl_alt()
                {
                    self.local_key = Some(code);
                    self.toggle_fullscreen();
                    return;
                }
                // Repeats are forwarded too: injected keys do not auto-repeat.
                if let Some(mut scancode) = keymap::scancode(code, self.command_key) {
                    if self.swap_ctrl_and_super {
                        scancode = keymap::swap_ctrl_and_super(scancode);
                    }
                    self.send(InputEvent::Key {
                        scancode,
                        pressed: event.state == ElementState::Pressed,
                    });
                }
            }
            _ => {}
        }
    }

    /// Brings what changes with time up to date; when to come back for it.
    fn wake_at(&mut self) -> Option<Instant> {
        self.update_displays();
        self.update_title();
        if self
            .button_hint
            .is_some_and(|until| until <= Instant::now())
        {
            self.button_hint = None;
            self.update_button();
        }
        if self
            .restart_confirm
            .is_some_and(|until| until <= Instant::now())
        {
            self.restart_confirm = None;
            self.last_title = Instant::now() - Duration::from_secs(1);
            self.update_title();
        }
        // What the title says of it lasts a while.
        if self
            .restart_sent
            .is_some_and(|sent| sent.elapsed() >= RESTART_ANSWER)
        {
            self.restart_sent = None;
            self.last_title = Instant::now() - Duration::from_secs(1);
            self.update_title();
        }
        let shown_until = self.transfer.as_ref().and_then(|(_, until)| *until);
        if shown_until.is_some_and(|until| until <= Instant::now()) {
            self.transfer = None;
            self.last_title = Instant::now() - Duration::from_secs(1);
            self.update_title();
        }
        // The answer comes with no event of its own, on a still screen.
        let answer = self
            .restart_sent
            .map(|_| Instant::now() + Duration::from_secs(1));
        [self.button_hint, self.restart_confirm, answer, shown_until]
            .into_iter()
            .flatten()
            .min()
    }
}

impl ApplicationHandler<UserEvent> for Viewers {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        timed("resumed", || self.on_resumed(event_loop));
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        timed(user_event_name(&event), || {
            self.on_user_event(event_loop, event);
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        timed(window_event_name(&event), || {
            self.on_window_event(event_loop, id, event);
        });
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        timed("about_to_wait", || self.on_about_to_wait(event_loop));
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.on_exiting(event_loop);
    }
}

/// The loop's calls, each timed by [`timed`]: while one runs, no window of
/// this viewer moves or repaints.
impl Viewers {
    fn on_resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.resumed = true;
        let waiting: Vec<u64> = self
            .sessions
            .iter()
            .filter(|(_, v)| v.window.is_none())
            .map(|(&n, _)| n)
            .collect();
        for n in waiting {
            self.make_window(event_loop, n);
        }
    }

    fn on_user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Session(n, SessionEvent::Locked) => {
                if self.ui {
                    tell_ui(n, crate::ui_link::HOST_LOCKED);
                } else {
                    println!("{}", crate::ui_link::HOST_LOCKED);
                }
            }
            UserEvent::Session(n, event) => {
                if let Some(viewer) = self.sessions.get_mut(&n) {
                    viewer.on_event(event);
                    self.settle(event_loop, n);
                }
            }
            UserEvent::Opened(n, opened) => match *opened {
                Ok(started) => self.opened(event_loop, n, started),
                Err(e) => self.failed(event_loop, n, e),
            },
            UserEvent::Ui(asked) => self.asked(event_loop, asked),
            UserEvent::UiGone => {
                self.ui_gone = true;
                self.exit_if_done(event_loop);
            }
        }
    }

    fn on_window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(&n) = self.windows.get(&id) else {
            return;
        };
        if let Some(viewer) = self.sessions.get_mut(&n) {
            viewer.on_window(event);
            self.settle(event_loop, n);
        }
    }

    fn on_about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.idle_since.is_some() {
            self.exit_if_done(event_loop);
        }
        let leave = self.idle_since.map(|since| since + IDLE_STAY);
        let wake = self
            .sessions
            .values_mut()
            .filter_map(Viewer::wake_at)
            .chain(leave)
            .min();
        event_loop.set_control_flow(match wake {
            Some(until) => ControlFlow::WaitUntil(until),
            None => ControlFlow::Wait,
        });
    }

    fn on_exiting(&mut self, _event_loop: &ActiveEventLoop) {
        for (_, mut viewer) in self.sessions.drain() {
            if let Some(session) = viewer.session.take() {
                self.closing.push(self.runtime.spawn(async move {
                    if let Err(e) = session.close().await {
                        warn!("encerrando: {e}");
                    }
                }));
            }
        }
        let closing = std::mem::take(&mut self.closing);
        let net = self.net.clone();
        self.runtime.block_on(async move {
            for task in closing {
                let _ = task.await;
            }
            // Gracefully, relay included.
            if let Some(endpoint) = net.endpoint.get() {
                endpoint.close().await;
            }
        });
    }
}

/// What a user event is, for [`timed`].
fn user_event_name(event: &UserEvent) -> &'static str {
    match event {
        UserEvent::Session(_, SessionEvent::Frame(_)) => "quadro",
        UserEvent::Session(_, SessionEvent::SessionEnded) => "sessão encerrada",
        UserEvent::Session(_, SessionEvent::Back(_)) => "volta",
        UserEvent::Session(_, SessionEvent::Displays) => "monitores",
        UserEvent::Session(_, SessionEvent::ChatOut(_)) => "chat",
        UserEvent::Session(_, SessionEvent::Transfer(_)) => "arquivo",
        UserEvent::Session(_, SessionEvent::AskFile { .. }) => "pergunta de arquivo",
        UserEvent::Session(_, SessionEvent::Locked) => "tela bloqueada",
        UserEvent::Opened(..) => "sessão aberta",
        UserEvent::Ui(_) => "pedido da interface",
        UserEvent::UiGone => "interface fechada",
    }
}

/// What a window event is, for [`timed`].
fn window_event_name(event: &WindowEvent) -> &'static str {
    match event {
        WindowEvent::Resized(_) => "Resized",
        WindowEvent::Moved(_) => "Moved",
        WindowEvent::RedrawRequested => "RedrawRequested",
        WindowEvent::CursorMoved { .. } => "CursorMoved",
        WindowEvent::MouseInput { .. } => "MouseInput",
        WindowEvent::KeyboardInput { .. } => "KeyboardInput",
        WindowEvent::Focused(_) => "Focused",
        WindowEvent::ScaleFactorChanged { .. } => "ScaleFactorChanged",
        _ => "outro evento da janela",
    }
}

/// Hands decoded frames to the window.
fn show_frames(latest: Latest, tell: Tell) -> impl FnMut(client::DecodedFrame) + Send + 'static {
    move |frame| {
        if latest.put(frame.yuv, frame.display) {
            tell.send(SessionEvent::Frame(Instant::now()));
        }
    }
}

/// Wakes the window at each news of the host's displays: a still screen
/// brings no event of its own.
fn hear_displays(session: &ClientHandle, tell: Tell) -> impl Future<Output = ()> + Send + 'static {
    let mut displays = session.displays_changed();
    async move {
        while displays.changed().await.is_ok() {
            tell.send(SessionEvent::Displays);
        }
    }
}

/// Hands the interface what the chat of session `n` brings, for as long as
/// it lasts.
fn hear_chat(session: &mut ClientHandle, n: u64) -> impl Future<Output = ()> + Send + 'static {
    let chat = session.chat();
    async move {
        let Some(mut chat) = chat else { return };
        while let Some(news) = chat.recv().await {
            match news {
                client::ChatNews::Text(text) => {
                    tell_ui(n, &crate::ui_link::viewer_chat_line(&text));
                }
                client::ChatNews::NoOne => tell_ui(n, crate::ui_link::VIEWER_CHAT_NO_ONE),
            }
        }
    }
}

/// A line about session `n` for the interface that opened this viewer. If
/// it is gone, nobody is waiting: the session goes on. (`println!` would stop
/// it right there.)
fn tell_ui(n: u64, line: &str) {
    let line = crate::ui_link::session_line(n, line);
    let mut out = UI_OUT.lock().expect("lock");
    let (lines, _) = out.get_or_insert_with(start_ui_out);
    let _ = lines.send(line);
}

/// The lines for the interface, and the thread writing them. The window's
/// thread never writes them itself: a pipe the interface is slow to read
/// would hold every window there.
/// (The second says when all is written.)
type UiOut = (
    std::sync::mpsc::Sender<String>,
    std::sync::mpsc::Receiver<()>,
);

static UI_OUT: Mutex<Option<UiOut>> = Mutex::new(None);

/// How long leaving waits for the last lines: an interface that stopped
/// reading must not keep this process alive.
const UI_OUT_WAIT: Duration = Duration::from_secs(2);

fn start_ui_out() -> UiOut {
    let (lines, queued) = std::sync::mpsc::channel::<String>();
    let (written, done) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = std::io::stdout();
        for line in queued {
            let _ = writeln!(out, "{line}").and_then(|()| out.flush());
        }
        let _ = written.send(());
    });
    (lines, done)
}

/// Waits a little for the lines still on their way to the interface (how
/// the last sessions ended), before leaving.
fn finish_ui_out() {
    let out = UI_OUT.lock().expect("lock").take();
    if let Some((lines, done)) = out {
        drop(lines);
        let _ = done.recv_timeout(UI_OUT_WAIT);
    }
}

/// Takes what the interface asks for (another computer, what the person
/// writes in a chat window...), on a thread that may wait for as long as the
/// interface stays quiet.
fn listen_to_ui(proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        loop {
            // (Zeroed after: it may hold a password.)
            let mut line = Zeroizing::new(String::new());
            match std::io::BufRead::read_line(&mut stdin, &mut line) {
                Ok(0) | Err(_) => {
                    let _ = proxy.send_event(UserEvent::UiGone);
                    break;
                }
                Ok(_) => {}
            }
            let end = line.trim_end_matches(['\r', '\n']).len();
            line.truncate(end);
            if let Some(asked) = ToViewer::parse(&line)
                && proxy.send_event(UserEvent::Ui(asked)).is_err()
            {
                break;
            }
        }
    });
}

/// Tells the window when this session's connection ends.
fn end_of(session: &ClientHandle, tell: Tell) -> impl Future<Output = ()> + Send + 'static {
    let closed = session.closed();
    async move {
        closed.await;
        tell.send(SessionEvent::SessionEnded);
    }
}

/// No session's window has the focus.
const NO_WINDOW: u64 = u64::MAX;

/// The session whose window has the focus now, if any.
#[derive(Clone)]
struct Focus(Arc<AtomicU64>);

impl Default for Focus {
    fn default() -> Self {
        Self(Arc::new(AtomicU64::new(NO_WINDOW)))
    }
}

impl Focus {
    fn is(&self, n: u64) -> bool {
        self.0.load(Ordering::Relaxed) == n
    }
}

/// One window's focus, as the clipboard sees it.
struct WindowFocus {
    n: u64,
    all: Focus,
    /// It lost the focus since it last had it.
    away: bool,
}

impl WindowFocus {
    fn new(n: u64, all: Focus) -> Self {
        Self {
            n,
            all,
            away: false,
        }
    }

    /// The window got (`true`) or lost the focus. Whether to tell the host
    /// what the clipboard holds: on coming back, not when it first opens
    /// (what was copied before the session stays out of it).
    fn changed(&mut self, focused: bool) -> bool {
        if focused {
            self.all.0.store(self.n, Ordering::Relaxed);
            std::mem::take(&mut self.away)
        } else {
            // Unless another window took it already.
            let _ = self.all.0.compare_exchange(
                self.n,
                NO_WINDOW,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
            self.away = true;
            false
        }
    }
}

/// Keeps this computer's clipboard and the host's in step, for as long as
/// session `n` lasts and if it may: only while its window has the focus, or
/// a text from one host could reach another, or what is copied here while
/// working elsewhere reach the host. `tell` asks for the content now (the
/// window got the focus back).
fn share_clipboard(
    session: &mut ClientHandle,
    n: u64,
    focus: Focus,
    tell: Arc<tokio::sync::Notify>,
) -> impl Future<Output = ()> + Send + 'static {
    let link = session
        .clipboard()
        .filter(|_| session.permissions.clipboard);
    async move {
        let Some((outgoing, mut incoming)) = link else {
            return;
        };
        let watching = focus.clone();
        let watched = Clipboard::watch(move |text| {
            if watching.is(n) {
                let _ = outgoing.send(text);
            }
        });
        let clipboard = match watched {
            Ok(clipboard) => clipboard,
            Err(e) => {
                warn!("área de transferência: {e}");
                return;
            }
        };
        loop {
            tokio::select! {
                text = incoming.recv() => match text {
                    Some(text) if focus.is(n) => clipboard.set(text),
                    Some(_) => {}
                    None => return,
                },
                () = tell.notified() => clipboard.tell(),
            }
        }
    }
}

/// Asks over the session's window; a question left unanswered is a no.
#[cfg(any(windows, target_os = "macos"))]
fn ask_file(
    window: &Window,
    text: String,
    answer: oneshot::Sender<bool>,
) -> impl Future<Output = ()> + Send + 'static {
    use rfd::{AsyncMessageDialog, MessageButtons, MessageDialogResult, MessageLevel};
    const RECEIVE: &str = "Receber";
    let shown = AsyncMessageDialog::new()
        .set_title("Hext Remote")
        .set_description(text)
        .set_level(MessageLevel::Warning)
        .set_buttons(MessageButtons::OkCancelCustom(
            RECEIVE.into(),
            "Recusar".into(),
        ))
        .set_parent(window)
        .show();
    async move {
        let yes = match shown.await {
            MessageDialogResult::Ok => true,
            MessageDialogResult::Custom(label) => label == RECEIVE,
            _ => false,
        };
        let _ = answer.send(yes);
    }
}

/// Nothing to ask with: no file is taken.
#[cfg(not(any(windows, target_os = "macos")))]
fn ask_file(
    _window: &Window,
    _text: String,
    answer: oneshot::Sender<bool>,
) -> std::future::Ready<()> {
    let _ = answer.send(false);
    std::future::ready(())
}

/// Trades files with the host, for as long as the session lasts: what it
/// sends, if this user says yes, goes to their Downloads.
fn share_files(
    session: &mut ClientHandle,
    tell: Tell,
) -> impl Future<Output = ()> + Send + 'static {
    let host = session.host.device_name.clone();
    let asking = tell.clone();
    let confirm: Confirm = Arc::new(move |name, size| {
        let text = format!(
            "{host} quer enviar um arquivo para este computador:\n\n{name} ({})\n\n\
             Receba só se esperava por ele. Ele vai para a pasta Downloads.",
            size_text(size)
        );
        let (answer, said) = oneshot::channel();
        asking.send(SessionEvent::AskFile { text, answer });
        Box::pin(async move { said.await.unwrap_or(false) })
    });
    session.files(
        Arc::new(OwnFiles(crate::downloads())),
        Arc::new(move |transfer| tell.send(SessionEvent::Transfer(transfer))),
        confirm,
    )
}

/// Tries to open the session again until `time` runs out: the host may be
/// booting, or the network still down.
async fn reconnect(
    back: &Back,
    endpoint: &Endpoint,
    token: ResumeToken,
    time: Duration,
) -> Result<ClientHandle> {
    let deadline = Instant::now() + time;
    let mut wait = BACK_FIRST;
    loop {
        match try_back(back, endpoint, token).await {
            Ok(session) => return Ok(session),
            Err(e) if is_final(&e) || Instant::now() + wait >= deadline => return Err(e),
            Err(e) => info!("ainda sem voltar: {e:#}"),
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(BACK_MAX);
    }
}

async fn try_back(back: &Back, endpoint: &Endpoint, token: ResumeToken) -> Result<ClientHandle> {
    let (net, id) = (&back.route.net, back.route.id);
    let (Some(server), Some(known)) = (&net.server, &net.known) else {
        bail!("sem o servidor de sinalização");
    };
    let client = crate::signaling::login(server, &net.key, &device_name()).await?;
    let named = client.relay().map(|r| (r.url.as_str(), r.quic_port));
    net.relays.follow(endpoint, named).await;
    let (conn, code, pin) =
        crate::signaling::connect(client, id, known, endpoint, (true, false, None), || {}).await?;
    let hello = Hello {
        peer: PeerInfo::local(APP_VERSION, device_name()),
        access_code: code,
        resume: Some(token),
        asks: Default::default(),
    };
    let frames = show_frames(back.latest.clone(), back.tell.clone());
    let session = client::start(conn, hello, None, back.decoder, frames).await?;
    keep_pin(Some(pin));
    Ok(clean_host(session))
}

/// The host's name and version as the session shows them everywhere (log,
/// title, questions): as the host sent them, they could move a terminal's
/// cursor or turn text around.
fn clean_host(mut session: ClientHandle) -> ClientHandle {
    let host = &mut session.host;
    host.device_name = crate::signaling::shown_name(&host.device_name);
    host.app_version = crate::signaling::shown_name(&host.app_version);
    session
}

/// The session got in: the host's key is kept for its ID (a failure to
/// write it only costs asking again next time).
fn keep_pin(pin: Option<Pin>) {
    if let Some(Err(e)) = pin.map(Pin::keep) {
        warn!("não foi possível guardar a chave do computador: {e:#}");
    }
}

/// Whether the host said no: trying again would not change that.
fn is_final(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<SessionError>(),
        Some(SessionError::Net(
            NetError::Rejected(_)
                | NetError::PasswordNeeded
                | NetError::HostNotConfirmed
                | NetError::PasswordNotAsked
                | NetError::IncompatibleVersion { .. }
        ))
    )
}

/// Whether a key event is the rest of a shortcut of the viewer (`held`): its
/// repeats and release, not keys for the host. A new press means the release
/// got lost, as can happen while the window changes: it is a key like any
/// other, and so is what follows it.
fn belongs_to_shortcut(
    held: &mut Option<KeyCode>,
    code: KeyCode,
    repeat: bool,
    released: bool,
) -> bool {
    if *held != Some(code) {
        return false;
    }
    if repeat && !released {
        return true;
    }
    *held = None;
    released
}

/// The display of a Ctrl+Alt+1..9 key, from 0.
fn display_key(code: KeyCode) -> Option<u8> {
    const KEYS: [KeyCode; 9] = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
    ];
    KEYS.iter().position(|key| *key == code).map(|i| i as u8)
}

/// Whether input takes its meaning from the picture on screen. A release is
/// not such a thing: the button it lets go of would stay down on the host.
fn aimed_at_the_picture(event: &InputEvent) -> bool {
    matches!(
        event,
        InputEvent::MouseMove { .. }
            | InputEvent::Wheel { .. }
            | InputEvent::MouseButton { pressed: true, .. }
    )
}

/// Whether the mouse at (`x`, `y`) calls for the buttons (their row at
/// `rect`): it comes up from the handle, always in sight, and stays while
/// the mouse is around the row or the handle. A mouse merely passing over
/// the row's place does not bring it, or what is under it on the host could
/// never be clicked.
fn button_called(rect: VideoRect, handle: VideoRect, shown: bool, x: f64, y: f64) -> bool {
    let on_handle = handle.contains(x, y, handle.width);
    on_handle || (shown && rect.contains(x, y, rect.height))
}

/// The unattended password, if asked for: from the terminal, unseen, or from
/// standard input (never an argument: others could read it in the process
/// list).
fn read_password(args: &ViewArgs) -> Result<Option<Login>> {
    if !args.password_stdin {
        return Ok(None);
    }
    let password = if std::io::stdin().is_terminal() {
        Zeroizing::new(rpassword::prompt_password("Senha do outro computador: ")?)
    } else {
        let mut line = Zeroizing::new(String::new());
        std::io::stdin().read_line(&mut line)?;
        // (In place: a copy would stay behind in memory.)
        let end = line.trim_end_matches(['\r', '\n']).len();
        line.truncate(end);
        line
    };
    if password.is_empty() {
        bail!("a senha está vazia");
    }
    let account = match &args.account {
        Some(text) => Some(crate::owners::parse_id(text)?),
        None => None,
    };
    Ok(Some(Login {
        password,
        remember: args.remember,
        account,
    }))
}

/// The unattended password, and whether to ask to be trusted from then on,
/// without it, or that the computer bind to an account (the chain's ID).
struct Login {
    password: Zeroizing<String>,
    remember: bool,
    account: Option<[u8; 32]>,
}

/// What a request asks for: with the password, to be remembered, and that
/// the computer bind to an account.
type Asks = (bool, bool, Option<[u8; 32]>);

impl Login {
    /// What to ask the server for: with the password, and whether to be
    /// remembered or bound to.
    fn asks(login: Option<&Login>) -> Asks {
        login.map_or((false, false, None), |login| {
            (true, login.remember, login.account)
        })
    }
}

/// What to tell the user when a session does not open.
fn explain(e: SessionError) -> anyhow::Error {
    let reason = match &e {
        SessionError::Net(NetError::Rejected(RejectReason::WrongPassword)) => "senha errada".into(),
        SessionError::Net(NetError::Rejected(RejectReason::LockedOut { seconds })) => format!(
            "senhas erradas demais: tente de novo em {} min",
            seconds.div_ceil(60)
        ),
        SessionError::Net(NetError::Rejected(RejectReason::Denied)) => {
            "esse computador não está aceitando a senha agora".into()
        }
        _ => return e.into(),
    };
    anyhow::anyhow!(reason)
}

/// Reaches the host: by ID through the server, or by IP with the access code
/// and the host's key. The connection, the code to present, how to come
/// back (by ID), and the host's key to keep once in (by ID). `asks`: with the password, and asking to be remembered
/// ([`Login::asks`]). `on_waiting` runs if the host says its screen is
/// locked.
async fn reach(
    net: &Arc<Net>,
    target: Target,
    (unattended, remember, account): Asks,
    on_waiting: impl FnMut() + Send,
) -> Result<(Connection, String, Option<Route>, Option<Pin>)> {
    match target {
        Target::Id(id) => {
            let (Some(server), Some(known)) = (&net.server, &net.known) else {
                bail!("para conectar pelo ID é preciso o servidor de sinalização (--server)");
            };
            // Logged in (and so registered, which the default relay requires)
            // before the endpoint reaches the relay.
            let client = crate::signaling::login(server, &net.key, &device_name()).await?;
            let endpoint = net.endpoint().await?;
            let named = client.relay().map(|r| (r.url.as_str(), r.quic_port));
            net.relays.follow(endpoint, named).await;
            // Signed here, for this computer: the server cannot point it at
            // another account.
            let account = account.map(|chain| BindAsk::new(&net.key, chain, id));
            let asks = (unattended, remember, account);
            let (conn, code, pin) =
                crate::signaling::connect(client, id, known, endpoint, asks, on_waiting).await?;
            let route = Route {
                net: net.clone(),
                id,
            };
            Ok((conn, code, Some(route), Some(pin)))
        }
        Target::Ip { addr, host, code } => {
            let endpoint = net.endpoint().await?;
            let conn = hext_net::connect(endpoint, host)
                .await
                .with_context(|| format!("falha ao conectar em {addr}"))?;
            hext_net::log_paths(&conn, "visualizador");
            Ok((conn, code, None, None))
        }
    }
}

/// Opens a session: reaches the host and gets in. Its pictures go to
/// `latest`, and its news to `tell`.
async fn open_session(
    net: Arc<Net>,
    target: Target,
    login: Option<Login>,
    decoder: DecoderPreference,
    latest: Latest,
    tell: Tell,
) -> Result<Started> {
    let waiting = tell.clone();
    let on_waiting = move || waiting.send(SessionEvent::Locked);
    let asks = Login::asks(login.as_ref());
    let (conn, code, route, pin) = reach(&net, target, asks, on_waiting).await?;
    let (hello, password) = hello_of(&net, code, login);
    let session = client::start(conn, hello, password, decoder, show_frames(latest, tell))
        .await
        .map_err(explain)?;
    keep_pin(pin);
    let session = clean_host(session);
    Ok(Started { session, route })
}

/// The introduction to the host, and the password to answer it with. What
/// the viewer asks for (to be remembered, the account) goes end to end, too:
/// the host does not take the server's word alone.
fn hello_of(net: &Net, code: String, login: Option<Login>) -> (Hello, Option<PasswordLogin>) {
    let asks = login
        .as_ref()
        .map_or_else(HelloAsks::default, |login| HelloAsks {
            remember: login.remember,
            account: login.account,
        });
    let hello = Hello {
        peer: PeerInfo::local(APP_VERSION, device_name()),
        access_code: code,
        resume: None,
        asks,
    };
    let viewer = net.key.public_key();
    let password = login.map(|login| PasswordLogin {
        password: login.password,
        viewer,
    });
    (hello, password)
}

/// The viewer's endpoint (it connects; it does not accept).
async fn bind(key: &hext_crypto::DeviceKey, relay: Option<hext_net::RelayUrl>) -> Result<Endpoint> {
    let config = EndpointConfig {
        relay,
        ..Default::default()
    };
    Ok(hext_net::bind(key, &config).await?)
}

/// Who to connect to, and how.
enum Target {
    /// By ID, through the signaling server.
    Id(DeviceId),
    /// By IP, with the access code and the host's key.
    Ip {
        addr: std::net::SocketAddr,
        host: hext_net::EndpointAddr,
        code: String,
    },
}

fn target(args: &ViewArgs) -> Result<Target> {
    // An address first: "192.168.0.10" must never be taken for an ID.
    let addr = parse_addr(&args.target);
    if let (Err(_), Ok(id)) = (&addr, args.target.parse::<DeviceId>()) {
        if crate::server(&args.server).is_none() {
            bail!("para conectar pelo ID é preciso o servidor de sinalização (--server)");
        }
        if args.code.is_some() || args.key.is_some() {
            warn!(
                "pelo ID, --code e --key não são usados: o host aceita o pedido e a chave dele vem do servidor"
            );
        }
        return Ok(Target::Id(id));
    }
    let addr = addr?;
    let (Some(code), Some(host_key)) = (args.code.clone(), args.key) else {
        bail!(
            "para conectar pelo IP, informe o código de acesso (--code) e a chave do host (--key), mostrados por ele"
        );
    };
    let host = hext_net::EndpointAddr::new(hext_net::endpoint_id(&host_key)?).with_ip_addr(addr);
    Ok(Target::Ip { addr, host, code })
}

/// Receives and decodes without a window, printing stats every second.
async fn headless(
    net: Arc<Net>,
    target: Target,
    args: &ViewArgs,
    login: Option<Login>,
) -> Result<()> {
    let on_waiting = || println!("{}", crate::ui_link::HOST_LOCKED);
    let asks = Login::asks(login.as_ref());
    let (conn, code, _, pin) = reach(&net, target, asks, on_waiting).await?;
    let (hello, password) = hello_of(&net, code, login);
    let session = client::start(conn, hello, password, decoder(args.decoder), |_| {})
        .await
        .map_err(explain)?;
    keep_pin(pin);
    let session = clean_host(session);
    println!(
        "Conectado a {} ({:?}, v{})",
        session.host.device_name, session.host.os, session.host.app_version
    );
    for _ in 0..args.seconds {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if session.is_finished() {
            println!("Sessão encerrada pelo host");
            break;
        }
        let mut stats = session.stats.lock().expect("lock");
        let (w, h) = (stats.width, stats.height);
        if let Some(snapshot) = stats.snapshot() {
            println!(
                "{w}x{h} | {snapshot} | RTT {:.1} ms{}",
                session.rtt().as_secs_f64() * 1000.0,
                path_label(&session)
            );
        }
    }
    session.close().await?;
    if let Some(endpoint) = net.endpoint.get() {
        endpoint.close().await;
    }
    Ok(())
}

/// " (P2P)" or " (relay)": the path the session is using now.
fn path_label(session: &ClientHandle) -> &'static str {
    match session.path() {
        Some(p) if p.relay => " (relay)",
        Some(_) => " (P2P)",
        None => "",
    }
}

fn decoder(choice: CodecChoice) -> DecoderPreference {
    match choice {
        CodecChoice::Auto => DecoderPreference::Auto,
        CodecChoice::Software => DecoderPreference::Software,
        CodecChoice::Hardware => DecoderPreference::Hardware,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(width: u32) -> YuvFrame {
        YuvFrame {
            width,
            height: 2,
            y: vec![0; 4],
            u: vec![0; 1],
            v: vec![0; 1],
        }
    }

    #[test]
    fn the_clipboard_goes_only_to_the_window_with_the_focus() {
        let all = Focus::default();
        let mut one = WindowFocus::new(1, all.clone());
        let mut two = WindowFocus::new(2, all.clone());
        // Before any window has the focus: none.
        assert!(!all.is(1) && !all.is(2));
        // Opening: in use, but what was copied before stays here.
        assert!(!one.changed(true));
        assert!(all.is(1));
        // Working elsewhere: nobody's.
        assert!(!one.changed(false));
        assert!(!all.is(1));
        // Back: what was copied meanwhile is told, once.
        assert!(one.changed(true));
        assert!(!one.changed(true));
        // To the other window, whichever order the news come in.
        assert!(!two.changed(true));
        one.changed(false);
        assert!(all.is(2) && !all.is(1));
        two.changed(false);
        assert!(!all.is(2));
    }

    #[test]
    fn a_burst_of_pictures_wakes_the_window_once() {
        let pictures = Pictures::default();
        // Only the first of many is told; the window finds the newest.
        assert!(pictures.put(picture(1), 0));
        for width in 2..=100 {
            assert!(!pictures.put(picture(width), 1));
        }
        let (newest, display) = pictures.take().unwrap();
        assert_eq!((newest.width, display), (100, 1));
        assert!(pictures.take().is_none());
        // Taken, the next is told again.
        assert!(pictures.put(picture(101), 1));
    }

    #[test]
    fn a_picture_left_while_drawing_is_told_again() {
        let pictures = Pictures::default();
        assert!(pictures.put(picture(1), 0));
        // The window, told, takes nothing yet (no window), then takes it.
        assert!(!pictures.put(picture(2), 0));
        assert_eq!(pictures.take().unwrap().0.width, 2);
        // Left right after the take: a new word for the window.
        assert!(pictures.put(picture(3), 0));
        assert!(!pictures.put(picture(4), 0));
    }

    #[test]
    fn a_shortcut_keeps_its_repeats_and_release() {
        let mut held = Some(KeyCode::Enter);
        assert!(belongs_to_shortcut(&mut held, KeyCode::Enter, true, false));
        assert!(!belongs_to_shortcut(&mut held, KeyCode::KeyA, false, false));
        assert!(belongs_to_shortcut(&mut held, KeyCode::Enter, false, true));
        assert_eq!(held, None);
    }

    #[test]
    fn a_key_after_a_lost_release_is_the_hosts_up_and_down() {
        let mut held = Some(KeyCode::Enter);
        assert!(!belongs_to_shortcut(
            &mut held,
            KeyCode::Enter,
            false,
            false
        ));
        assert!(!belongs_to_shortcut(&mut held, KeyCode::Enter, false, true));
    }

    #[test]
    fn the_shortcut_numbers_displays_from_the_first() {
        assert_eq!(display_key(KeyCode::Digit1), Some(0));
        assert_eq!(display_key(KeyCode::Digit9), Some(8));
        assert_eq!(display_key(KeyCode::Digit0), None);
        assert_eq!(display_key(KeyCode::KeyR), None);
    }

    #[test]
    fn a_held_button_can_always_be_let_go() {
        let button = |pressed| InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed,
        };
        assert!(aimed_at_the_picture(&button(true)));
        assert!(!aimed_at_the_picture(&button(false)));
        assert!(aimed_at_the_picture(&InputEvent::MouseMove {
            x: 0.5,
            y: 0.5
        }));
        assert!(!aimed_at_the_picture(&InputEvent::ReleaseAll));
    }

    /// On the left edge, halfway down a window 800 high; the row beside it.
    const HANDLE: VideoRect = VideoRect {
        x: 2.0,
        y: 368.0,
        width: 8.0,
        height: 64.0,
    };
    const ROW: VideoRect = VideoRect {
        x: 18.0,
        y: 386.0,
        width: 152.0,
        height: 28.0,
    };

    #[test]
    fn the_buttons_come_from_the_handle_only() {
        assert!(
            button_called(ROW, HANDLE, false, 0.0, 400.0),
            "the very edge"
        );
        assert!(
            button_called(ROW, HANDLE, false, 6.0, 370.0),
            "on the handle"
        );
        assert!(
            !button_called(ROW, HANDLE, false, 60.0, 400.0),
            "where the row would be"
        );
        assert!(
            !button_called(ROW, HANDLE, false, 0.0, 100.0),
            "elsewhere on the edge"
        );
    }

    #[test]
    fn the_buttons_stay_while_the_mouse_is_around() {
        assert!(button_called(ROW, HANDLE, true, 60.0, 400.0));
        assert!(button_called(ROW, HANDLE, true, 190.0, 430.0));
        assert!(
            button_called(ROW, HANDLE, true, 4.0, 360.0),
            "back on the handle"
        );
        assert!(!button_called(ROW, HANDLE, true, 300.0, 400.0));
    }
}
