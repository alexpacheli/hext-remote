//! The screen and the input on Wayland (F7, milestone 5), through the
//! desktop's portal (`xdg-desktop-portal`, over D-Bus): one RemoteDesktop
//! session with the screens of a ScreenCast attached, so a single question
//! to the person at the computer covers both.
//!
//! The session opens when a capture first needs it. The first time, the
//! desktop asks the person at the computer which screens to share and
//! whether to allow control; the answer comes with a restore token, kept in
//! this user's folder, which later sessions present to skip the question
//! (where the desktop honors it: RemoteDesktop version 2). The session
//! closes a little after the last capture ends, and with it the desktop's
//! "sharing the screen" indicator.
//!
//! Frames come by PipeWire ([`crate::pipewire`]); input goes back through
//! the same session ([`send_input`]), in evdev codes and in each stream's
//! own coordinates.

use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use ashpd::desktop::remote_desktop::{
    Axis, DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions,
};
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use ashpd::desktop::{PersistMode, Session};
use ashpd::enumflags2::BitFlags;
use futures_util::StreamExt;
use tokio::sync::mpsc as channel;

use crate::linux::{Monitor, layout};
use crate::pipewire::{PipeWireCapturer, Remote};
use crate::{CaptureError, Capturer, Display, Frame, Rect};

/// How long the person at the computer has to answer the desktop's question.
const ANSWER_WAIT: Duration = Duration::from_secs(120);
/// How long the session stays open with no capture (a monitor switch reopens
/// one at once; the end of a session does not).
const IDLE_CLOSE: Duration = Duration::from_secs(10);
/// The restore token's file, in this user's folder.
const TOKEN_FILE: &str = "portal-token";

/// One shared screen: its PipeWire node and where it sits among the displays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SharedScreen {
    pub node: u32,
    pub display: Display,
}

/// Input for the portal, in its own terms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PortalInput {
    /// An evdev key code (`KEY_*`).
    Key { code: u16, pressed: bool },
    /// An evdev button code (`BTN_LEFT`...).
    Button { code: u16, pressed: bool },
    /// The pointer, at a place of the displays (relative to the main one).
    MoveTo { x: i32, y: i32 },
    /// Wheel notches: positive `dy` up, positive `dx` right.
    Scroll { dx: i32, dy: i32 },
}

enum Command {
    OpenRemote(mpsc::Sender<Result<OwnedFd, String>>),
    Input(PortalInput),
}

/// The open session, as the rest of the process sees it.
struct Portal {
    commands: channel::UnboundedSender<Command>,
    screens: Vec<SharedScreen>,
    alive: Arc<AtomicBool>,
}

static PORTAL: Mutex<Option<Portal>> = Mutex::new(None);
/// Captures open now: the session closes when none is left for a while.
static CAPTURES: AtomicUsize = AtomicUsize::new(0);
/// The person at the computer stopped the sharing while captures were open:
/// no new question until they are all gone (the session that was stopped
/// ends), or the monitor checks would ask again every few seconds.
static STOPPED: AtomicBool = AtomicBool::new(false);

/// The screens the person at the computer shared, as displays; opens the
/// session first if none is open (which may mean waiting for their answer).
pub(crate) fn displays() -> Result<Vec<Display>, CaptureError> {
    with_portal(|portal| portal.screens.iter().map(|s| s.display.clone()).collect())
}

/// Opens the capture of one of the [`displays`].
pub(crate) fn open(display: &Display, max_fps: u32) -> Result<Box<dyn Capturer>, CaptureError> {
    let (screen, opened) = with_portal(|portal| {
        let screen = portal
            .screens
            .iter()
            .find(|s| s.display.id == display.id)
            .cloned();
        let (reply, answer) = mpsc::channel();
        let sent = portal.commands.send(Command::OpenRemote(reply)).is_ok();
        (screen, sent.then_some(answer))
    })?;
    let screen = screen.ok_or_else(|| CaptureError::DisplayGone(display.id.clone()))?;
    let fd = opened
        .and_then(|answer| answer.recv_timeout(Duration::from_secs(10)).ok())
        .ok_or_else(|| CaptureError::Platform("o portal não respondeu".into()))?
        .map_err(|e| CaptureError::Platform(format!("PipeWire do portal: {e}")))?;
    // Counted before the stream starts, so the session stays for it.
    let user = CaptureUser::new();
    let capturer =
        PipeWireCapturer::new(Remote::Fd(fd), screen.node, screen.display.rect, max_fps)?;
    Ok(Box::new(PortalCapturer {
        capturer,
        _user: user,
    }))
}

/// Sends input through the open session; dropped (with a note in the log)
/// when there is none: input never opens one.
pub fn send_input(input: PortalInput) {
    let guard = PORTAL.lock().expect("lock");
    match guard.as_ref() {
        Some(portal) if portal.alive.load(Ordering::Acquire) => {
            let _ = portal.commands.send(Command::Input(input));
        }
        _ => tracing::debug!("mouse e teclado: sem sessão do portal"),
    }
}

/// Runs `f` on the open session, opening one if needed.
fn with_portal<T>(f: impl FnOnce(&Portal) -> T) -> Result<T, CaptureError> {
    let mut guard = PORTAL.lock().expect("lock");
    if !guard
        .as_ref()
        .is_some_and(|p| p.alive.load(Ordering::Acquire))
    {
        if STOPPED.load(Ordering::Acquire) && CAPTURES.load(Ordering::Acquire) > 0 {
            return Err(CaptureError::PermissionDenied(
                "o compartilhamento da tela foi encerrado neste computador".into(),
            ));
        }
        STOPPED.store(false, Ordering::Release);
        *guard = Some(start()?);
    }
    Ok(f(guard.as_ref().expect("just opened")))
}

/// Opens a session on a thread of its own and waits for its screens.
fn start() -> Result<Portal, CaptureError> {
    let (commands, received) = channel::unbounded_channel();
    let (ready, answer) = mpsc::channel();
    let alive = Arc::new(AtomicBool::new(true));
    let flag = alive.clone();
    std::thread::Builder::new()
        .name("hext-portal".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    let _ = ready.send(Err(CaptureError::Platform(e.to_string())));
                    return;
                }
            };
            runtime.block_on(run(ready, received, &flag));
            flag.store(false, Ordering::Release);
        })
        .map_err(|e| CaptureError::Platform(e.to_string()))?;
    let screens = answer
        .recv_timeout(ANSWER_WAIT + Duration::from_secs(5))
        .map_err(|_| {
            CaptureError::PermissionDenied(
                "ninguém respondeu ao pedido de compartilhar a tela neste computador".into(),
            )
        })??;
    Ok(Portal {
        commands,
        screens,
        alive,
    })
}

/// The session's life: open it, answer for it, close it.
async fn run(
    ready: mpsc::Sender<Result<Vec<SharedScreen>, CaptureError>>,
    mut commands: channel::UnboundedReceiver<Command>,
    alive: &AtomicBool,
) {
    let opened = tokio::time::timeout(ANSWER_WAIT, open_session()).await;
    let (remote, screencast, session, screens) = match opened {
        Ok(Ok(opened)) => opened,
        Ok(Err(e)) => {
            let _ = ready.send(Err(e));
            return;
        }
        Err(_) => {
            let _ = ready.send(Err(CaptureError::PermissionDenied(
                "ninguém respondeu ao pedido de compartilhar a tela neste computador".into(),
            )));
            return;
        }
    };
    let _ = ready.send(Ok(screens.clone()));
    let mut closed = match session.receive_closed().await {
        Ok(closed) => Some(closed),
        Err(e) => {
            tracing::debug!("portal: sem o aviso de sessão encerrada: {e}");
            None
        }
    };
    let mut idle_since: Option<Instant> = None;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::OpenRemote(reply)) => {
                    let fd = screencast
                        .open_pipe_wire_remote(&session, Default::default())
                        .await
                        .map_err(|e| e.to_string());
                    let _ = reply.send(fd);
                }
                Some(Command::Input(input)) => {
                    if let Err(e) = notify(&remote, &session, &screens, input).await {
                        tracing::debug!("mouse e teclado pelo portal: {e}");
                    }
                }
                None => break,
            },
            _ = async {
                match closed.as_mut() {
                    Some(closed) => { closed.next().await; }
                    None => std::future::pending::<()>().await,
                }
            } => {
                tracing::info!("o compartilhamento da tela foi encerrado neste computador");
                // Stopped by the person at the computer: the next session
                // asks again, never comes back on its own with the token.
                forget_token();
                STOPPED.store(true, Ordering::Release);
                alive.store(false, Ordering::Release);
                return;
            }
            _ = tick.tick() => {
                if CAPTURES.load(Ordering::Acquire) > 0 {
                    idle_since = None;
                } else if idle_since.get_or_insert_with(Instant::now).elapsed() >= IDLE_CLOSE {
                    break;
                }
            }
        }
    }
    // (Marked first, so nobody sends to a session on its way out.)
    alive.store(false, Ordering::Release);
    let _ = session.close().await;
    tracing::info!("sessão do portal encerrada");
}

type Opened = (
    RemoteDesktop,
    Screencast,
    Session<RemoteDesktop>,
    Vec<SharedScreen>,
);

/// Asks the desktop for the screens and the input (or presents the token of
/// an earlier answer).
async fn open_session() -> Result<Opened, CaptureError> {
    let portal = |e: ashpd::Error| match e {
        ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => {
            CaptureError::PermissionDenied(
                "o compartilhamento da tela foi recusado neste computador".into(),
            )
        }
        ashpd::Error::PortalNotFound(_) => CaptureError::Platform(
            "este computador não tem o portal de compartilhamento de tela (xdg-desktop-portal)"
                .into(),
        ),
        e => CaptureError::Platform(format!("portal: {e}")),
    };
    let remote = RemoteDesktop::new().await.map_err(portal)?;
    let screencast = Screencast::new().await.map_err(portal)?;
    let session = remote
        .create_session(Default::default())
        .await
        .map_err(portal)?;
    // Persisting needs RemoteDesktop version 2; older ones ask every time.
    let token = (remote.version() >= 2).then(read_token).flatten();
    remote
        .select_devices(
            &session,
            SelectDevicesOptions::default()
                .set_devices(DeviceType::Keyboard | DeviceType::Pointer)
                .set_persist_mode(PersistMode::ExplicitlyRevoked)
                .set_restore_token(token.as_deref()),
        )
        .await
        .map_err(portal)?;
    // The pointer stays out of the image, as on the other systems (the
    // viewer draws its own), where the desktop can.
    let cursor = match screencast.available_cursor_modes().await {
        Ok(modes) if !modes.contains(CursorMode::Hidden) => CursorMode::Embedded,
        _ => CursorMode::Hidden,
    };
    screencast
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(cursor)
                .set_sources(BitFlags::from(SourceType::Monitor))
                .set_multiple(true),
        )
        .await
        .map_err(portal)?;
    let started = remote
        .start(&session, None, Default::default())
        .await
        .map_err(portal)?
        .response()
        .map_err(portal)?;
    if let Some(token) = started.restore_token() {
        write_token(token);
    }
    let streams: Vec<StreamInfo> = started
        .streams()
        .iter()
        .map(|s| StreamInfo {
            node: s.pipe_wire_node_id(),
            id: s.id().map(str::to_owned),
            position: s.position(),
            size: s.size(),
        })
        .collect();
    let screens = screens(&streams);
    if screens.is_empty() {
        return Err(CaptureError::PermissionDenied(
            "nenhuma tela foi compartilhada neste computador".into(),
        ));
    }
    tracing::info!(
        "portal: {} tela(s) compartilhada(s), controle {}",
        screens.len(),
        if started
            .devices()
            .contains(DeviceType::Keyboard | DeviceType::Pointer)
        {
            "permitido"
        } else {
            "não permitido"
        }
    );
    Ok((remote, screencast, session, screens))
}

/// What the portal says about one stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StreamInfo {
    pub node: u32,
    pub id: Option<String>,
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
}

/// The streams as displays, the first one the main (the portal has no
/// "primary"): placed where the desktop says, or side by side when it
/// does not say.
pub(crate) fn screens(streams: &[StreamInfo]) -> Vec<SharedScreen> {
    let mut next_x = 0;
    let monitors: Vec<(u32, Monitor)> = streams
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let (width, height) = s.size.unwrap_or((1920, 1080));
            let (x, y) = s.position.unwrap_or((next_x, 0));
            next_x = next_x.max(x + width);
            let monitor = Monitor {
                name: s.id.clone().unwrap_or_default(),
                x,
                y,
                width: width.max(0) as u32,
                height: height.max(0) as u32,
                primary: i == 0,
            };
            (s.node, monitor)
        })
        .collect();
    let list: Vec<Monitor> = monitors.iter().map(|(_, m)| m.clone()).collect();
    let (displays, origin) = layout(&list, (0, 0));
    // `layout` keeps the order of the monitors it kept (those of some size).
    monitors
        .iter()
        .filter(|(_, m)| m.width >= 2 && m.height >= 2)
        .zip(displays)
        .map(|((node, m), display)| {
            debug_assert_eq!(
                (m.x - origin.0, m.y - origin.1),
                (display.rect.x, display.rect.y)
            );
            SharedScreen {
                node: *node,
                display,
            }
        })
        .collect()
}

/// The screen under a place of the displays, and the place in its own
/// coordinates (the nearest screen when between them).
pub(crate) fn locate(screens: &[SharedScreen], x: i32, y: i32) -> Option<(u32, f64, f64)> {
    let distance = |r: &Rect| {
        let dx = (r.x - x).max(x - (r.x + r.width as i32 - 1)).max(0);
        let dy = (r.y - y).max(y - (r.y + r.height as i32 - 1)).max(0);
        i64::from(dx) * i64::from(dx) + i64::from(dy) * i64::from(dy)
    };
    let screen = screens.iter().min_by_key(|s| distance(&s.display.rect))?;
    let r = screen.display.rect;
    let local_x = (x - r.x).clamp(0, r.width.saturating_sub(1) as i32);
    let local_y = (y - r.y).clamp(0, r.height.saturating_sub(1) as i32);
    Some((screen.node, f64::from(local_x), f64::from(local_y)))
}

async fn notify(
    remote: &RemoteDesktop,
    session: &Session<RemoteDesktop>,
    screens: &[SharedScreen],
    input: PortalInput,
) -> ashpd::Result<()> {
    let state = |pressed| {
        if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        }
    };
    match input {
        PortalInput::Key { code, pressed } => {
            remote
                .notify_keyboard_keycode(session, code.into(), state(pressed), Default::default())
                .await
        }
        PortalInput::Button { code, pressed } => {
            remote
                .notify_pointer_button(session, code.into(), state(pressed), Default::default())
                .await
        }
        PortalInput::MoveTo { x, y } => {
            let Some((node, x, y)) = locate(screens, x, y) else {
                return Ok(());
            };
            remote
                .notify_pointer_motion_absolute(session, node, x, y, Default::default())
                .await
        }
        PortalInput::Scroll { dx, dy } => {
            // The portal counts like Wayland: positive scrolls down and right.
            if dy != 0 {
                remote
                    .notify_pointer_axis_discrete(session, Axis::Vertical, -dy, Default::default())
                    .await?;
            }
            if dx != 0 {
                remote
                    .notify_pointer_axis_discrete(session, Axis::Horizontal, dx, Default::default())
                    .await?;
            }
            Ok(())
        }
    }
}

/// This user's folder for the app (the same as the rest of the app's).
fn token_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("hext-remote").join(TOKEN_FILE))
}

fn read_token() -> Option<String> {
    let token = std::fs::read_to_string(token_path()?).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_owned())
}

/// Keeps the token for next time, readable by this user only. (Each token
/// works once: every session brings a new one.)
fn write_token(token: &str) {
    use std::os::unix::fs::OpenOptionsExt;
    let Some(path) = token_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, token.as_bytes()));
    if let Err(e) = written {
        tracing::warn!("portal: o token de restauração não foi salvo: {e}");
    }
}

fn forget_token() {
    if let Some(path) = token_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// Counts a capture as open for as long as it lives.
struct CaptureUser;

impl CaptureUser {
    fn new() -> Self {
        CAPTURES.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for CaptureUser {
    fn drop(&mut self) {
        CAPTURES.fetch_sub(1, Ordering::AcqRel);
    }
}

struct PortalCapturer {
    capturer: PipeWireCapturer,
    _user: CaptureUser,
}

impl Capturer for PortalCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<bool, CaptureError> {
        self.capturer.next_frame(timeout)
    }

    fn frame(&self) -> Option<&Frame> {
        self.capturer.frame()
    }

    fn rect(&self) -> Rect {
        self.capturer.rect()
    }

    fn copy_time(&self) -> Duration {
        self.capturer.copy_time()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(node: u32, position: Option<(i32, i32)>, size: (i32, i32)) -> StreamInfo {
        StreamInfo {
            node,
            id: None,
            position,
            size: Some(size),
        }
    }

    #[test]
    fn the_first_stream_is_the_main_display() {
        let screens = screens(&[
            stream(40, Some((1920, 0)), (1280, 1024)),
            stream(41, Some((0, 0)), (1920, 1080)),
        ]);
        let rects: Vec<(u32, i32, i32)> = screens
            .iter()
            .map(|s| (s.node, s.display.rect.x, s.display.rect.y))
            .collect();
        assert_eq!(rects, [(40, 0, 0), (41, -1920, 0)]);
        // Unnamed streams still get names apart.
        assert_ne!(screens[0].display.id, screens[1].display.id);
    }

    #[test]
    fn streams_without_a_place_go_side_by_side() {
        let screens = screens(&[stream(7, None, (800, 600)), stream(8, None, (1024, 768))]);
        assert_eq!(screens[1].display.rect.x, 800);
        assert_eq!(screens[1].display.rect.width, 1024);
    }

    #[test]
    fn the_pointer_lands_on_the_screen_under_it() {
        let screens = screens(&[
            stream(40, Some((0, 0)), (1920, 1080)),
            stream(41, Some((1920, 0)), (1280, 1024)),
        ]);
        assert_eq!(locate(&screens, 100, 200), Some((40, 100.0, 200.0)));
        assert_eq!(locate(&screens, 2000, 10), Some((41, 80.0, 10.0)));
        // Below the shorter screen: the nearest one, at its edge.
        assert_eq!(locate(&screens, 2000, 1050), Some((41, 80.0, 1023.0)));
        assert_eq!(locate(&[], 0, 0), None);
    }
}
