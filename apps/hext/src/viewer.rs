//! The viewer: a window showing the host's screen, forwarding input.
//!
//! winit owns the main thread (required on macOS); networking runs on a
//! tokio runtime and decoding on its own thread. Each decoded frame is
//! handed over through `latest` and the event loop is woken to draw it.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use hext_codec::{DecoderPreference, YuvFrame};
use hext_input::keymap::{self, CommandKey};
use hext_net::{Connection, Endpoint, EndpointConfig};
use hext_proto::Os;
use hext_proto::{Hello, InputEvent, MouseButton, PeerInfo};
use hext_render::{ButtonAction, Renderer, VideoRect, button_rect};
use hext_session::client::{self, ClientHandle};
use hext_signal::{DeviceId, KnownHosts};
use tracing::{info, warn};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Modifiers, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, ModifiersKeyState, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};

use crate::{APP_VERSION, CmdKey, CodecChoice, CtrlKey, ViewArgs, device_name, parse_addr};

/// Trackpad scrolling arrives in pixels; this many make one wheel notch.
const PIXELS_PER_NOTCH: f64 = 50.0;
/// How long the button stays up after entering full screen, so the way out
/// is seen once.
const BUTTON_HINT: Duration = Duration::from_secs(4);
/// How close to the top of the window the mouse must go to call the button.
const BUTTON_EDGE: f64 = 2.0;

enum UserEvent {
    Frame,
    SessionEnded,
}

pub fn run(args: ViewArgs) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let (endpoint, conn, code) = runtime.block_on(open(&args))?;
    let hello = Hello {
        peer: PeerInfo::local(APP_VERSION, device_name()),
        access_code: code,
    };

    if args.headless {
        return runtime.block_on(headless(endpoint, conn, args, hello));
    }

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let latest: Arc<Mutex<Option<YuvFrame>>> = Arc::default();

    let (handle, closed_proxy) = (latest.clone(), proxy.clone());
    let session = runtime.block_on(async {
        let watch = conn.clone();
        let session = client::start(conn, hello, decoder(args.decoder), move |frame| {
            *handle.lock().expect("lock") = Some(frame.yuv);
            let _ = proxy.send_event(UserEvent::Frame);
        })
        .await?;
        tokio::spawn(async move {
            watch.closed().await;
            let _ = closed_proxy.send_event(UserEvent::SessionEnded);
        });
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
        println!("{}", crate::ui_link::VIEWER_CONNECTED);
    }

    let host_is_mac = session.host.os == Os::MacOs;
    let mut app = Viewer {
        runtime,
        session: Some(session),
        endpoint: Some(endpoint),
        latest,
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
        last_title: Instant::now(),
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

struct Viewer {
    runtime: tokio::runtime::Runtime,
    session: Option<ClientHandle>,
    /// Closed (gracefully, relay included) when the window closes.
    endpoint: Option<Endpoint>,
    latest: Arc<Mutex<Option<YuvFrame>>>,
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

    fn send(&self, event: InputEvent) {
        // (The host refuses it anyway; this saves sending it.)
        if let Some(session) = self.session.as_ref().filter(|s| s.permissions.control) {
            session.send_input(event);
        }
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
        match frame {
            Some(frame) => renderer.upload(&frame),
            None if only_if_new => return,
            None => {}
        }
        if let Err(e) = renderer.render() {
            warn!("render: {e}");
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

    /// Shows the button while the mouse calls for it or the hint lasts.
    fn update_button(&mut self) {
        let Some(window) = &self.window else { return };
        let size = window.inner_size();
        let rect = button_rect((size.width, size.height), window.scale_factor());
        let near = self
            .cursor
            .is_some_and(|(x, y)| button_called(rect, self.button.is_some(), x, y));
        let action = if window.fullscreen().is_some() {
            ButtonAction::Leave
        } else {
            ButtonAction::Enter
        };
        let button = (near || self.button_hint.is_some()).then_some((action, rect));
        if self.button != button {
            self.button = button;
            if let Some(renderer) = &mut self.renderer {
                renderer.set_button(button);
            }
            self.draw(false);
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
        let (Some(window), Some(session)) = (&self.window, &self.session) else {
            return;
        };
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
                info!("sessão encerrada pelo host");
                event_loop.exit();
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
        self.update_title();
        if self
            .button_hint
            .is_some_and(|until| until <= Instant::now())
        {
            self.button_hint = None;
            self.update_button();
        }
        event_loop.set_control_flow(match self.button_hint {
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

/// Whether the mouse at (`x`, `y`) calls for the button at `rect`: it comes up
/// at the very top of the window, and stays while the mouse is around. A mouse
/// merely passing there does not bring it, or what is under it on the host
/// could never be clicked.
fn button_called(rect: VideoRect, shown: bool, x: f64, y: f64) -> bool {
    if shown {
        rect.contains(x, y, rect.height)
    } else {
        y <= BUTTON_EDGE && rect.contains(x, f64::from(rect.y), rect.height)
    }
}

/// Connects to the host: by ID through the server (`--server`), or by IP
/// with the access code and the host's key. Returns the code to present to
/// the host.
async fn open(args: &ViewArgs) -> Result<(Endpoint, Connection, String)> {
    // The arguments first, before any network.
    let target = target(args)?;
    let key = crate::device_key(args.device_key.as_deref())?;
    let (endpoint, connected) = match target {
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
            let connected = crate::signaling::connect(client, id, &known, &endpoint).await;
            (endpoint, connected)
        }
        Target::Ip { addr, host, code } => {
            // Not registered through a server: no default relay.
            let endpoint = bind(&key, crate::relay_for(&args.relay, None)?).await?;
            let connected = hext_net::connect(&endpoint, host)
                .await
                .with_context(|| format!("falha ao conectar em {addr}"))
                .map(|conn| (conn, code));
            (endpoint, connected)
        }
    };
    match connected {
        Ok((conn, code)) => Ok((endpoint, conn, code)),
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
) -> Result<()> {
    let session = client::start(conn, hello, decoder(args.decoder), |_| {}).await?;
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

    const RECT: VideoRect = VideoRect {
        x: 472.0,
        y: 6.0,
        width: 56.0,
        height: 28.0,
    };

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
    fn the_button_comes_from_the_top_edge_only() {
        assert!(button_called(RECT, false, 500.0, 0.0));
        assert!(!button_called(RECT, false, 500.0, 20.0));
        assert!(!button_called(RECT, false, 100.0, 0.0));
    }

    #[test]
    fn the_button_stays_while_the_mouse_is_around() {
        assert!(button_called(RECT, true, 500.0, 20.0));
        assert!(button_called(RECT, true, 500.0, 60.0));
        assert!(!button_called(RECT, true, 500.0, 90.0));
    }
}
