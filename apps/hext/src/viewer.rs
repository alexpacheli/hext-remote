//! The viewer: a window showing the host's screen, forwarding input.
//!
//! winit owns the main thread (required on macOS); networking runs on a
//! tokio runtime and decoding on its own thread. Each decoded frame is
//! handed over through `latest` and the event loop is woken to draw it.

use std::io::{IsTerminal, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use anyhow::{Context, Result, bail};
use hext_clipboard::Clipboard;
use hext_codec::{DecoderPreference, YuvFrame};
use hext_input::keymap::{self, CommandKey};
use hext_net::{Connection, Endpoint, EndpointConfig, NetError, PasswordLogin};
use hext_proto::Os;
use hext_proto::{ChatText, Displays, Hello, InputEvent, MouseButton, PeerInfo, RejectReason};
use hext_render::{
    ButtonAction, DisplayBar, Renderer, VideoRect, button_rect, chat_button_rect, handle_rect,
};
use hext_session::client::{self, ClientHandle};
use hext_session::files::{Confirm, OwnFiles, Stage, Transfer, size_text};
use hext_session::{NETWORK_GRACE, ResumeToken, SessionError};
use hext_signal::{DeviceId, KnownHosts};
use tracing::{info, warn};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Modifiers, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, ModifiersKeyState, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};
use zeroize::Zeroizing;

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

enum UserEvent {
    Frame,
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
}

/// What it takes to reach the host again: only by ID.
struct Route {
    server: String,
    id: DeviceId,
    key: hext_crypto::DeviceKey,
    known: KnownHosts,
}

/// What [`open`] got.
struct Opened {
    endpoint: Endpoint,
    conn: Connection,
    code: String,
    viewer: hext_crypto::PublicKey,
    route: Option<Route>,
}

/// How to open the session again.
struct Back {
    route: Route,
    decoder: DecoderPreference,
    latest: Latest,
    proxy: EventLoopProxy<UserEvent>,
}

/// The newest picture, and the host's display it shows.
type Latest = Arc<Mutex<Option<(YuvFrame, u8)>>>;

pub fn run(args: ViewArgs) -> Result<()> {
    let password = read_password(&args)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let Opened {
        endpoint,
        conn,
        code,
        viewer,
        route,
    } = runtime.block_on(open(&args, password.is_some()))?;
    let password = password.map(|password| PasswordLogin { password, viewer });
    let hello = Hello {
        peer: PeerInfo::local(APP_VERSION, device_name()),
        access_code: code,
        resume: None,
    };

    if args.headless {
        return runtime.block_on(headless(endpoint, conn, args, hello, password));
    }

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let latest: Latest = Arc::default();

    let session = runtime.block_on(async {
        let mut session = client::start(
            conn,
            hello,
            password,
            decoder(args.decoder),
            show_frames(latest.clone(), proxy.clone()),
        )
        .await
        .map_err(explain)?;
        tokio::spawn(end_of(&session, proxy.clone()));
        tokio::spawn(share_clipboard(&mut session));
        tokio::spawn(share_files(&mut session, proxy.clone()));
        tokio::spawn(hear_displays(&session, proxy.clone()));
        if args.report_to_ui {
            tokio::spawn(hear_chat(&mut session));
        }
        anyhow::Ok(session)
    })?;
    info!(
        "conectado a {} ({:?}, v{})",
        session.host.device_name, session.host.os, session.host.app_version
    );
    if !session.permissions.control {
        info!("o host permitiu só ver a tela");
    }
    if args.report_to_ui {
        println!(
            "{} {}",
            crate::ui_link::VIEWER_CONNECTED,
            crate::signaling::shown_name(&session.host.device_name)
        );
        listen_to_ui(proxy.clone());
    }

    let host_is_mac = session.host.os == Os::MacOs;
    let back = route.map(|route| {
        Arc::new(Back {
            route,
            decoder: decoder(args.decoder),
            latest: latest.clone(),
            proxy: proxy.clone(),
        })
    });
    let mut app = Viewer {
        runtime,
        session: Some(session),
        endpoint: Some(endpoint),
        latest,
        back,
        coming_back: None,
        restart_confirm: None,
        restart_sent: None,
        transfer: None,
        failure: None,
        proxy,
        window: None,
        renderer: None,
        command_key: match args.cmd_key {
            // A Mac host understands Command itself.
            _ if host_is_mac => CommandKey::Win,
            CmdKey::Ctrl => CommandKey::Ctrl,
            CmdKey::Win => CommandKey::Win,
        },
        swap_ctrl_and_super: host_is_mac
            && !cfg!(target_os = "macos")
            && args.ctrl_key == CtrlKey::Cmd,
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
        chat_link: args.report_to_ui,
        chat_button: None,
        last_title: Instant::now(),
    };
    event_loop.run_app(&mut app)?;
    match app.failure.take() {
        Some(e) => Err(e.context("não foi possível voltar à sessão")),
        None => Ok(()),
    }
}

struct Viewer {
    runtime: tokio::runtime::Runtime,
    /// `None` while coming back.
    session: Option<ClientHandle>,
    /// Closed (gracefully, relay included) when the window closes.
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
    /// Why the session could not be opened again.
    failure: Option<anyhow::Error>,
    proxy: EventLoopProxy<UserEvent>,
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
            let _ = back.proxy.send_event(UserEvent::Back(Box::new(result)));
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
    ///
    /// With `only_if_new`, does nothing when no new frame arrived: several
    /// frame events can find a single frame, and presenting the same picture
    /// again could block on vsync and back up the event queue.
    fn draw(&mut self, only_if_new: bool) {
        // Check the renderer first: taking the frame before the window exists
        // would discard it, and a still host screen sends no replacement.
        let Some(renderer) = &mut self.renderer else {
            return;
        };
        let frame = self.latest.lock().expect("lock").take();
        let mut arrived = false;
        match frame {
            Some((frame, display)) => {
                renderer.upload(&frame);
                arrived = self.picture.replace(display) != Some(display);
            }
            None if only_if_new => return,
            None => {}
        }
        if let Err(e) = renderer.render() {
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
            self.draw(false);
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

impl ApplicationHandler<UserEvent> for Viewer {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
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
                warn!("janela: {e}");
                event_loop.exit();
                return;
            }
        };
        match Renderer::new(window.clone()) {
            Ok(renderer) => self.renderer = Some(renderer),
            Err(e) => {
                warn!("{e}");
                event_loop.exit();
                return;
            }
        }
        self.window = Some(window);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            // Draw right away instead of waiting for a redraw request: lower latency.
            UserEvent::Frame => self.draw(true),
            UserEvent::SessionEnded => {
                let ending = self.session.as_ref().and_then(ClientHandle::comes_back);
                match (ending, self.back.clone(), self.endpoint.clone()) {
                    (Some((token, time)), Some(back), Some(endpoint)) => {
                        self.come_back(back, endpoint, token, time);
                    }
                    _ => {
                        info!("sessão encerrada pelo host");
                        event_loop.exit();
                    }
                }
            }
            UserEvent::Back(result) => match *result {
                Ok(mut session) => {
                    info!("sessão retomada");
                    // (On the window's thread, outside the runtime.)
                    self.runtime.spawn(end_of(&session, self.proxy.clone()));
                    self.runtime.spawn(share_clipboard(&mut session));
                    self.runtime
                        .spawn(share_files(&mut session, self.proxy.clone()));
                    self.runtime
                        .spawn(hear_displays(&session, self.proxy.clone()));
                    if self.chat_link {
                        self.runtime.spawn(hear_chat(&mut session));
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
                    self.failure = Some(e);
                    event_loop.exit();
                }
            },
            // (Taken right after this, as after any event.)
            UserEvent::Displays => {}
            UserEvent::ChatOut(text) => match &self.session {
                Some(session) => session.send_chat(text),
                None => tell_ui(crate::ui_link::VIEWER_CHAT_NO_ONE),
            },
            UserEvent::Transfer(transfer) => {
                let until = (!matches!(transfer.stage, Stage::Going { .. }))
                    .then(|| Instant::now() + TRANSFER_SHOWN);
                self.show_transfer(transfer.to_string(), until);
            }
            UserEvent::AskFile { text, answer } => {
                if let Some(window) = &self.window {
                    self.runtime.spawn(ask_file(window, text, answer));
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height);
                }
                self.update_button();
                self.draw(false);
            }
            WindowEvent::RedrawRequested => self.draw(false),
            WindowEvent::DroppedFile(path) => self.send_file(path),
            WindowEvent::Focused(false) => {
                self.local_key = None;
                self.send(InputEvent::ReleaseAll);
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
                        tell_ui(crate::ui_link::VIEWER_CHAT_OPEN);
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
                    tell_ui(crate::ui_link::VIEWER_CHAT_OPEN);
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

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
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
        let wake = [self.button_hint, self.restart_confirm, answer, shown_until]
            .into_iter()
            .flatten()
            .min();
        event_loop.set_control_flow(match wake {
            Some(until) => ControlFlow::WaitUntil(until),
            None => ControlFlow::Wait,
        });
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(session) = self.session.take()
            && let Err(e) = self.runtime.block_on(session.close())
        {
            warn!("encerrando: {e}");
        }
        if let Some(endpoint) = self.endpoint.take() {
            self.runtime.block_on(endpoint.close());
        }
    }
}

/// Hands decoded frames to the window.
fn show_frames(
    latest: Latest,
    proxy: EventLoopProxy<UserEvent>,
) -> impl FnMut(client::DecodedFrame) + Send + 'static {
    move |frame| {
        *latest.lock().expect("lock") = Some((frame.yuv, frame.display));
        let _ = proxy.send_event(UserEvent::Frame);
    }
}

/// Wakes the window at each news of the host's displays: a still screen
/// brings no event of its own.
fn hear_displays(
    session: &ClientHandle,
    proxy: EventLoopProxy<UserEvent>,
) -> impl Future<Output = ()> + Send + 'static {
    let mut displays = session.displays_changed();
    async move {
        while displays.changed().await.is_ok() {
            let _ = proxy.send_event(UserEvent::Displays);
        }
    }
}

/// Hands the interface what the chat brings, for as long as the session lasts.
fn hear_chat(session: &mut ClientHandle) -> impl Future<Output = ()> + Send + 'static {
    let chat = session.chat();
    async move {
        let Some(mut chat) = chat else { return };
        while let Some(news) = chat.recv().await {
            match news {
                client::ChatNews::Text(text) => {
                    tell_ui(&crate::ui_link::viewer_chat_line(&text));
                }
                client::ChatNews::NoOne => tell_ui(crate::ui_link::VIEWER_CHAT_NO_ONE),
            }
        }
    }
}

/// A line for the interface that opened this viewer. If it is gone, nobody
/// is waiting: the session goes on. (`println!` would stop it right there.)
fn tell_ui(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}").and_then(|()| out.flush());
}

/// Takes what the person writes in the interface's chat window, on a thread
/// that may wait for as long as the interface stays quiet.
fn listen_to_ui(proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        for line in std::io::stdin().lines().map_while(std::result::Result::ok) {
            if let Some(text) = crate::ui_link::chat_of_json(&line) {
                let _ = proxy.send_event(UserEvent::ChatOut(text));
            }
        }
    });
}

/// Tells the window when this session's connection ends.
fn end_of(
    session: &ClientHandle,
    proxy: EventLoopProxy<UserEvent>,
) -> impl Future<Output = ()> + Send + 'static {
    let closed = session.closed();
    async move {
        closed.await;
        let _ = proxy.send_event(UserEvent::SessionEnded);
    }
}

/// Keeps this computer's clipboard and the host's in step, for as long as
/// the session lasts and if it may.
fn share_clipboard(session: &mut ClientHandle) -> impl Future<Output = ()> + Send + 'static {
    let link = session
        .clipboard()
        .filter(|_| session.permissions.clipboard);
    async move {
        let Some((outgoing, mut incoming)) = link else {
            return;
        };
        let watched = Clipboard::watch(move |text| {
            let _ = outgoing.send(text);
        });
        match watched {
            Ok(clipboard) => {
                while let Some(text) = incoming.recv().await {
                    clipboard.set(text);
                }
            }
            Err(e) => warn!("área de transferência: {e}"),
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
    proxy: EventLoopProxy<UserEvent>,
) -> impl Future<Output = ()> + Send + 'static {
    let host = session.host.device_name.clone();
    let asking = proxy.clone();
    let confirm: Confirm = Arc::new(move |name, size| {
        let text = format!(
            "{host} quer enviar um arquivo para este computador:\n\n{name} ({})\n\n\
             Receba só se esperava por ele. Ele vai para a pasta Downloads.",
            size_text(size)
        );
        let (answer, said) = oneshot::channel();
        let _ = asking.send_event(UserEvent::AskFile { text, answer });
        Box::pin(async move { said.await.unwrap_or(false) })
    });
    session.files(
        Arc::new(OwnFiles(crate::downloads())),
        Arc::new(move |transfer| {
            let _ = proxy.send_event(UserEvent::Transfer(transfer));
        }),
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
    let route = &back.route;
    let client = crate::signaling::login(&route.server, &route.key, &device_name()).await?;
    let (conn, code) =
        crate::signaling::connect(client, route.id, &route.known, endpoint, true).await?;
    let hello = Hello {
        peer: PeerInfo::local(APP_VERSION, device_name()),
        access_code: code,
        resume: Some(token),
    };
    let frames = show_frames(back.latest.clone(), back.proxy.clone());
    Ok(client::start(conn, hello, None, back.decoder, frames).await?)
}

/// Whether the host said no: trying again would not change that.
fn is_final(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<SessionError>(),
        Some(SessionError::Net(
            NetError::Rejected(_)
                | NetError::PasswordNeeded
                | NetError::HostNotConfirmed
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
fn read_password(args: &ViewArgs) -> Result<Option<Zeroizing<String>>> {
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
    Ok(Some(password))
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

/// Connects to the host: by ID through the server (`--server`), or by IP
/// with the access code and the host's key. `unattended`: with the password.
async fn open(args: &ViewArgs, unattended: bool) -> Result<Opened> {
    // The arguments first, before any network.
    let target = target(args)?;
    if unattended && !matches!(target, Target::Id { .. }) {
        bail!("a senha de acesso não assistido só vale conectando pelo ID");
    }
    let key = crate::device_key(args.device_key.as_deref())?;
    let viewer = key.public_key();
    let (endpoint, connected, route) = match target {
        Target::Id { id, server } => {
            // Logged in (and so registered, which the default relay
            // requires) before the endpoint reaches the relay.
            let client = crate::signaling::login(server, &key, &device_name()).await?;
            let endpoint = bind(&key, crate::relay_for(&args.relay, Some(server))?).await?;
            // Next to the key, so each key (profile) has its own.
            let known = match &args.device_key {
                Some(path) => KnownHosts::new(path.with_file_name("known_hosts")),
                None => KnownHosts::default_location()?,
            };
            let connected =
                crate::signaling::connect(client, id, &known, &endpoint, unattended).await;
            let route = Route {
                server: server.to_owned(),
                id,
                key,
                known,
            };
            (endpoint, connected, Some(route))
        }
        Target::Ip { addr, host, code } => {
            // Not registered through a server: no default relay.
            let endpoint = bind(&key, crate::relay_for(&args.relay, None)?).await?;
            let connected = hext_net::connect(&endpoint, host)
                .await
                .with_context(|| format!("falha ao conectar em {addr}"))
                .map(|conn| (conn, code));
            (endpoint, connected, None)
        }
    };
    match connected {
        Ok((conn, code)) => Ok(Opened {
            endpoint,
            conn,
            code,
            viewer,
            route,
        }),
        Err(e) => {
            endpoint.close().await;
            Err(e)
        }
    }
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
enum Target<'a> {
    /// By ID, through the signaling server.
    Id { id: DeviceId, server: &'a str },
    /// By IP, with the access code and the host's key.
    Ip {
        addr: std::net::SocketAddr,
        host: hext_net::EndpointAddr,
        code: String,
    },
}

fn target(args: &ViewArgs) -> Result<Target<'_>> {
    // An address first: "192.168.0.10" must never be taken for an ID.
    let addr = parse_addr(&args.target);
    if let (Err(_), Ok(id)) = (&addr, args.target.parse::<DeviceId>()) {
        let Some(server) = crate::server(&args.server) else {
            bail!("para conectar pelo ID é preciso o servidor de sinalização (--server)");
        };
        if args.code.is_some() || args.key.is_some() {
            warn!(
                "pelo ID, --code e --key não são usados: o host aceita o pedido e a chave dele vem do servidor"
            );
        }
        return Ok(Target::Id { id, server });
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
    endpoint: Endpoint,
    conn: Connection,
    args: ViewArgs,
    hello: Hello,
    password: Option<PasswordLogin>,
) -> Result<()> {
    let session = client::start(conn, hello, password, decoder(args.decoder), |_| {})
        .await
        .map_err(explain)?;
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
    endpoint.close().await;
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
