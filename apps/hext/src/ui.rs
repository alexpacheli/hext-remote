//! The interface (Tauri 2): the home screen with the ID, the window that
//! asks about each request and the bar shown during a session.
//!
//! It always runs as the user, never as SYSTEM: a webview is far too much
//! to expose to a user's session with the system's rights. So the service
//! starts this in the user's session ([`Link::Service`]) and only trades
//! messages with it (see [`crate::ui_link`]). Portable, the host runs in
//! this same process ([`Link::Portable`]), with the same messages. Installed
//! on macOS, it is that same host, started at login with no window
//! ([`Link::Login`]).
//!
//! Windows are made when needed: this stays running in every session of
//! an installed computer, and an idle webview is not cheap.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use hext_proto::{ChatText, Permissions};
use hext_signal::DeviceId;
use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, RunEvent, State, WebviewUrl,
    WebviewWindow, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_dialog::DialogExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tracing::warn;
use zeroize::Zeroizing;

use crate::agent::{read_msg, write_msg};
use crate::chat::{Author, Chat, HOST_CHAT};
use crate::recent::{Computer, Recent};
use crate::ui_link::{
    FromUi, HOST_LOCKED, SessionView, ToUi, UiHub, VIEWER_CHAT_NO_ONE, VIEWER_CHAT_OPEN,
    VIEWER_CONNECTED, chat_json, chat_of_viewer_line,
};

/// Tells the pages to read [`View`] again.
const CHANGED: &str = "changed";
/// How long the bar says how a file ended, before going back to saying what
/// the session may do.
const TRANSFER_SHOWN: Duration = Duration::from_secs(8);
/// The session bar: width and height, in logical pixels.
const BAR_OPEN: (f64, f64) = (540.0, 56.0);
/// Folded: a strip on the top edge, with the dot. Nothing in it ends the
/// session, so a click meant for what is below does no harm.
const BAR_FOLDED: (f64, f64) = (96.0, 14.0);
/// How far from the top edge the open bar stands, in physical pixels.
const BAR_TOP: i32 = 8;

/// Who owns the sessions.
pub enum Link {
    /// This process (the portable app).
    Portable,
    /// The service, through this pipe.
    #[cfg(windows)]
    Service(String),
    /// This process, started at login (macOS, installed).
    #[cfg(target_os = "macos")]
    Login,
}

/// A request waiting for the user.
#[derive(Debug, Clone, Serialize)]
struct AskView {
    ask: u64,
    name: String,
    id: String,
    seconds: u64,
    /// This computer can restart at a viewer's request (installed only).
    can_restart: bool,
}

/// The viewer opened from the home screen.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
enum ViewerState {
    #[default]
    Idle,
    Asking {
        locked: bool,
    },
    Connected,
    Failed {
        message: String,
    },
}

/// What the pages show.
#[derive(Debug, Clone, Default, Serialize)]
struct View {
    id: Option<String>,
    notice: Option<String>,
    ask: Option<AskView>,
    session: Option<SessionView>,
    viewer: ViewerState,
    /// The computers this one accessed, the latest first.
    recent: Vec<Computer>,
    /// How the last file of the session went.
    transfer: Option<String>,
    /// Counts what the bar should unfold for.
    transfer_news: u64,
    /// Counts the sessions: a file is chosen for the one that was on.
    sessions: u64,
}

struct Shared {
    view: Mutex<View>,
    to_host: mpsc::UnboundedSender<FromUi>,
    /// This keeps running with no window: the sessions are the service's,
    /// or this is the installed app of a Mac. (On a Mac, an app opened by
    /// hand becomes it when the one from the login finds it running.)
    managed: AtomicBool,
    /// This computer can restart at a viewer's request.
    can_restart: bool,
    /// The conversations, by the label of their window.
    chats: Mutex<HashMap<String, Chat>>,
    /// Where each viewer running takes its messages, by that same label.
    viewers: Mutex<HashMap<String, ChildStdin>>,
    next_viewer: AtomicU64,
}

impl Shared {
    fn send(&self, msg: FromUi) {
        let _ = self.to_host.send(msg);
    }
}

fn managed(app: &AppHandle) -> bool {
    app.state::<Shared>().managed.load(Ordering::Relaxed)
}

fn changed(app: &AppHandle) {
    let _ = app.emit(CHANGED, ());
}

/// Whether what is going on calls for window `label`. Windows take a while
/// to make and to go, and things change meanwhile: this is checked once a
/// window is made and once one is gone.
fn wanted(app: &AppHandle, label: &str) -> bool {
    let shared = app.state::<Shared>();
    let view = shared.view.lock().expect("lock");
    match label {
        "ask" => view.ask.is_some(),
        "bar" => view.session.is_some(),
        _ => false,
    }
}

/// A window of the app, made or brought back. Never on the main thread
/// (making one there waits for itself).
fn window(
    app: &AppHandle,
    label: &str,
    page: &str,
    style: impl FnOnce(
        WebviewWindowBuilder<'_, tauri::Wry, AppHandle>,
    ) -> WebviewWindowBuilder<'_, tauri::Wry, AppHandle>,
) -> Option<WebviewWindow> {
    if let Some(window) = app.get_webview_window(label) {
        return Some(window);
    }
    let builder = WebviewWindowBuilder::new(app, label, WebviewUrl::App(page.into()));
    match style(builder).build() {
        Ok(window) => Some(window),
        Err(e) => {
            warn!("janela {label}: {e}");
            None
        }
    }
}

fn open_home(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let home = window(&app, "main", "index.html", |builder| {
            builder
                .title("Hext Remote")
                .inner_size(420.0, 640.0)
                .resizable(false)
                .maximizable(false)
                .center()
        });
        match home {
            Some(home) => {
                let _ = home.unminimize();
                let _ = home.show();
                let _ = home.set_focus();
            }
            // Portable, this window is the only way in: an app nobody can
            // see must not stay online.
            None if !managed(&app) && app.webview_windows().is_empty() => {
                #[cfg(windows)]
                show_error("A janela do Hext Remote não abriu. O WebView2 está instalado?");
                app.exit(1);
            }
            None => {}
        }
    });
}

/// For when there is no window to say it in.
#[cfg(windows)]
#[allow(unsafe_code)] // Win32 call
fn show_error(text: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::HSTRING;
    let (text, title) = (HSTRING::from(text), HSTRING::from("Hext Remote"));
    unsafe { MessageBoxW(None, &text, &title, MB_OK | MB_ICONERROR) };
}

fn open_ask(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let ask = window(&app, "ask", "ask.html", |builder| {
            builder
                .title("Hext Remote: pedido de acesso")
                .inner_size(420.0, 470.0)
                .resizable(false)
                .maximizable(false)
                .minimizable(false)
                .always_on_top(true)
                .center()
                .focused(true)
        });
        match ask {
            Some(ask) if wanted(&app, "ask") => {
                let _ = ask.show();
                let _ = ask.set_focus();
            }
            Some(ask) => {
                let _ = ask.destroy();
            }
            // Nobody can answer: a no now, not when the time runs out.
            None => refuse(&app),
        }
    });
}

/// The bar: on top of everything for as long as the session lasts, at the
/// top of the main screen. It takes no focus from what the user is doing.
fn open_bar(app: &AppHandle) {
    const WIDTH: f64 = BAR_OPEN.0;
    let app = app.clone();
    std::thread::spawn(move || {
        let bar = window(&app, "bar", "bar.html", |builder| {
            // (On the Mac a transparent window takes a private API: there the
            // strip is solid.)
            #[cfg(not(target_os = "macos"))]
            let builder = builder.transparent(true);
            builder
                .title("Hext Remote: sessão ativa")
                .shadow(false)
                .inner_size(WIDTH, BAR_OPEN.1)
                .decorations(false)
                .resizable(false)
                .maximizable(false)
                .minimizable(false)
                .always_on_top(true)
                .skip_taskbar(true)
                .focused(false)
        });
        let Some(bar) = bar else {
            // A session must not go on without its indicator.
            app.state::<Shared>().send(FromUi::EndSession);
            return;
        };
        if !wanted(&app, "bar") {
            let _ = bar.destroy();
            return;
        }
        if let Ok(Some(monitor)) = bar.primary_monitor() {
            let width = (WIDTH * monitor.scale_factor()) as i32;
            let x = monitor.position().x + (monitor.size().width as i32 - width) / 2;
            let y = monitor.position().y + BAR_TOP;
            let _ = bar.set_position(PhysicalPosition::new(x, y));
        }
    });
}

/// The chat window `label`. A message must not take the keyboard from what
/// the person is typing, or a text could land in the conversation, or the
/// other way around: only asking for the window gives it `focus`.
fn open_chat(app: &AppHandle, label: String, focus: bool) {
    let app = app.clone();
    std::thread::spawn(move || {
        let chat = window(&app, &label, "chat.html", |builder| {
            builder
                .title("Hext Remote: chat")
                .inner_size(340.0, 420.0)
                .min_inner_size(260.0, 260.0)
                .always_on_top(true)
                .focused(focus)
        });
        let Some(chat) = chat else { return };
        if focus {
            let _ = chat.show();
            let _ = chat.unminimize();
            let _ = chat.set_focus();
        } else if !chat.is_focused().unwrap_or(true) {
            let _ = chat.request_user_attention(Some(tauri::UserAttentionType::Informational));
        }
    });
}

/// Who a session on this computer is with.
fn peer_of(session: &SessionView) -> String {
    match &session.id {
        Some(id) => format!("{} (ID {id})", session.name),
        None => session.name.clone(),
    }
}

/// Closes a window without passing through its close button.
fn dismiss(app: &AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.destroy();
    }
}

/// Window `label` is gone: back, if it went while on its way out and a new
/// request or session had already taken it for its own.
fn reopen(app: &AppHandle, label: &str) {
    if wanted(app, label) {
        match label {
            "ask" => open_ask(app),
            "bar" => open_bar(app),
            _ => {}
        }
    }
}

/// Takes a message from the owner of the sessions.
fn received(app: &AppHandle, msg: ToUi) {
    let shared = app.state::<Shared>();
    let mut view = shared.view.lock().expect("lock");
    match msg {
        ToUi::Id(id) => view.id = Some(id),
        ToUi::Notice(notice) => {
            view.notice = Some(notice);
            // Started at login with no window: something to be seen (a
            // permission missing, say).
            #[cfg(target_os = "macos")]
            if app.get_webview_window("main").is_none() {
                open_home(app);
            }
        }
        ToUi::Ask {
            ask,
            name,
            id,
            seconds,
        } => {
            view.ask = Some(AskView {
                ask,
                name,
                id,
                seconds,
                can_restart: shared.can_restart,
            });
            open_ask(app);
        }
        ToUi::AskOver { ask } => {
            if view.ask.as_ref().is_some_and(|a| a.ask == ask) {
                view.ask = None;
                dismiss(app, "ask");
            }
        }
        ToUi::SessionStarted(session) => {
            let mut chats = shared.chats.lock().expect("lock");
            if let Some(chat) = chats.get_mut(HOST_CHAT)
                && !chat.stays_for(&peer_of(&session), session.id.is_some())
            {
                chats.remove(HOST_CHAT);
                dismiss(app, HOST_CHAT);
            }
            drop(chats);
            view.session = Some(session);
            view.sessions += 1;
            open_bar(app);
        }
        ToUi::SessionEnded => {
            view.session = None;
            view.transfer = None;
            dismiss(app, "bar");
            if let Some(chat) = shared.chats.lock().expect("lock").get_mut(HOST_CHAT) {
                chat.live = false;
            }
        }
        ToUi::Transfer {
            text,
            news,
            finished,
        } => {
            view.transfer = Some(text);
            view.transfer_news += u64::from(news);
            if finished {
                forget_transfer(app.clone(), view.transfer_news);
            }
        }
        ToUi::Chat(text) => {
            if let Some(session) = &view.session {
                shared
                    .chats
                    .lock()
                    .expect("lock")
                    .entry(HOST_CHAT.into())
                    .or_insert_with(|| Chat::new(peer_of(session)))
                    .push(Author::Them, text.into());
                open_chat(app, HOST_CHAT.into(), false);
            }
        }
    }
    drop(view);
    changed(app);
}

/// After a while, unless there is news since: what the session may do is what
/// the bar is there to say.
fn forget_transfer(app: AppHandle, news: u64) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(TRANSFER_SHOWN).await;
        let shared = app.state::<Shared>();
        let mut view = shared.view.lock().expect("lock");
        if view.transfer_news == news {
            view.transfer = None;
            drop(view);
            changed(&app);
        }
    });
}

/// Trades messages until the other end goes.
async fn talk<P: AsyncRead + AsyncWrite>(
    app: &AppHandle,
    pipe: P,
    mut outgoing: mpsc::UnboundedReceiver<FromUi>,
) {
    let (mut reader, mut writer) = tokio::io::split(pipe);
    let writing = async {
        while let Some(msg) = outgoing.recv().await {
            if write_msg(&mut writer, &msg).await.is_err() {
                break;
            }
        }
    };
    let reading = async {
        loop {
            match read_msg::<_, ToUi>(&mut reader).await {
                Ok(Some(msg)) => received(app, msg),
                Ok(None) => break,
                Err(e) => {
                    warn!("ligação com o host: {e}");
                    break;
                }
            }
        }
    };
    tokio::select! {
        () = writing => {}
        () = reading => {}
    }
}

/// The app's connection to the owner of the sessions, on a thread of its own.
fn connect_to_host(app: AppHandle, link: Link, outgoing: mpsc::UnboundedReceiver<FromUi>) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime,
            Err(e) => {
                warn!("{e}");
                return;
            }
        };
        runtime.block_on(async move {
            match link {
                Link::Portable => own_host(&app, outgoing, false).await,
                #[cfg(target_os = "macos")]
                Link::Login => own_host(&app, outgoing, true).await,
                #[cfg(windows)]
                Link::Service(pipe) => {
                    match tokio::net::windows::named_pipe::ClientOptions::new().open(&pipe) {
                        Ok(client) => talk(&app, client, outgoing).await,
                        Err(e) => warn!("abrir {pipe}: {e}"),
                    }
                    // Without the service, this has nothing to show. (It
                    // starts another when it comes back.)
                    app.exit(0);
                }
            }
        });
    });
}

/// The host in this process. Started at login (`login`), a host that fails
/// takes the app with it, for the system to start it again: nobody may be
/// there to.
async fn own_host(app: &AppHandle, outgoing: mpsc::UnboundedReceiver<FromUi>, login: bool) {
    let hub = UiHub::default();
    let (ours, theirs) = tokio::io::duplex(1 << 16);
    let attached = hub.clone();
    tokio::spawn(async move { attached.attach(theirs).await });
    let failed = app.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::host::portable(hub.clone()).await {
            warn!("{e:#}");
            if login {
                failed.exit(1);
            }
            hub.set_notice(format!("Não foi possível receber conexões: {e:#}"));
        }
    });
    talk(app, ours, outgoing).await;
}

#[tauri::command]
fn view(shared: State<'_, Shared>) -> View {
    shared.view.lock().expect("lock").clone()
}

/// What the user chooses in the window: viewing goes without saying.
#[derive(Debug, Clone, Copy, Deserialize)]
struct Choice {
    control: bool,
    clipboard: bool,
    files: bool,
    restart: bool,
}

/// The user's answer to request `ask`; `None`: refused.
#[tauri::command(async)]
fn answer(
    app: AppHandle,
    window: WebviewWindow,
    shared: State<'_, Shared>,
    ask: u64,
    choice: Option<Choice>,
) {
    if window.label() != "ask" {
        return; // only the window that asked answers
    }
    let permissions = choice.map(|choice| Permissions {
        control: choice.control,
        clipboard: choice.clipboard,
        files: choice.files,
        restart: choice.restart,
    });
    settle(&app, &shared, ask, permissions);
}

/// Gives the answer to request `ask`, if it is the one waiting.
fn settle(app: &AppHandle, shared: &Shared, ask: u64, permissions: Option<Permissions>) {
    {
        let mut view = shared.view.lock().expect("lock");
        if view.ask.as_ref().is_none_or(|a| a.ask != ask) {
            return;
        }
        view.ask = None;
    }
    shared.send(FromUi::Answer { ask, permissions });
    dismiss(app, "ask");
    changed(app);
}

/// Says no to the request waiting, if any, and takes it off the screen.
fn refuse(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let ask = shared
        .view
        .lock()
        .expect("lock")
        .ask
        .as_ref()
        .map(|a| a.ask);
    match ask {
        Some(ask) => settle(app, &shared, ask, None),
        None => dismiss(app, "ask"),
    }
}

/// The bar's size, open and folded: folded it goes to the top edge of its
/// screen and shows the dot. It stays centered where it is (it can be
/// dragged).
#[tauri::command]
fn fold_bar(window: WebviewWindow, folded: bool) {
    if window.label() != "bar" {
        return;
    }
    let (width, height) = if folded { BAR_FOLDED } else { BAR_OPEN };
    let (Ok(position), Ok(old), Ok(scale)) = (
        window.outer_position(),
        window.outer_size(),
        window.scale_factor(),
    ) else {
        return;
    };
    let new = (width * scale) as i32;
    let x = position.x + (old.width as i32 - new) / 2;
    let top = |monitor: tauri::Monitor| monitor.position().y + if folded { 0 } else { BAR_TOP };
    let y = window
        .current_monitor()
        .ok()
        .flatten()
        .map_or(position.y, top);
    let _ = window.set_size(LogicalSize::new(width, height));
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

/// The conversation of this chat window.
#[tauri::command]
fn chat_view(window: WebviewWindow, shared: State<'_, Shared>) -> Option<Chat> {
    let chats = shared.chats.lock().expect("lock");
    chats.get(window.label()).cloned()
}

/// Sends what the person wrote in this chat window.
#[tauri::command(async)]
fn chat_send(app: AppHandle, window: WebviewWindow, shared: State<'_, Shared>, text: String) {
    let Some(text) = ChatText::new(&text) else {
        return;
    };
    let label = window.label();
    let live = |chats: &HashMap<String, Chat>| chats.get(label).is_some_and(|chat| chat.live);
    if !live(&shared.chats.lock().expect("lock")) {
        return;
    }
    // (With no lock held: a viewer that stopped reading holds only this up.)
    let sent = if label == HOST_CHAT {
        shared.send(FromUi::Chat(text.clone()));
        true
    } else {
        let mut viewers = shared.viewers.lock().expect("lock");
        viewers
            .get_mut(label)
            .is_some_and(|viewer| writeln!(viewer, "{}", chat_json(&text)).is_ok())
    };
    if let Some(chat) = shared.chats.lock().expect("lock").get_mut(label) {
        if sent {
            chat.push(Author::Me, text.into());
        } else {
            chat.push(Author::App, "Não foi possível enviar.".into());
        }
    }
    changed(&app);
}

/// The Chat button of the bar.
#[tauri::command(async)]
fn open_host_chat(app: AppHandle, window: WebviewWindow, shared: State<'_, Shared>) {
    if window.label() != "bar" {
        return;
    }
    let session = shared.view.lock().expect("lock").session.clone();
    if let Some(session) = session {
        let mut chats = shared.chats.lock().expect("lock");
        chats
            .entry(HOST_CHAT.into())
            .or_insert_with(|| Chat::new(peer_of(&session)));
        drop(chats);
        open_chat(&app, HOST_CHAT.into(), true);
    }
}

#[tauri::command]
fn end_session(window: WebviewWindow, shared: State<'_, Shared>) {
    if window.label() == "bar" {
        shared.send(FromUi::EndSession);
    }
}

/// Lets the user choose files to send to the viewer. (On a thread that may
/// wait: the window stays open for as long as the user takes.)
#[tauri::command(async)]
fn send_files(app: AppHandle, window: WebviewWindow, shared: State<'_, Shared>) {
    if window.label() != "bar" {
        return;
    }
    let session = shared.view.lock().expect("lock").sessions;
    let chosen = app
        .dialog()
        .file()
        .set_title("Enviar arquivos para o técnico")
        .set_parent(&window)
        .blocking_pick_files();
    // Chosen for that technician, not for one who came in the meantime.
    if shared.view.lock().expect("lock").sessions != session {
        return;
    }
    for path in chosen.into_iter().flatten() {
        match path.into_path() {
            Ok(path) => shared.send(FromUi::SendFile(path)),
            Err(e) => warn!("arquivo escolhido: {e}"),
        }
    }
}

/// Connects to computer `id`; with `password`, nobody there needs to accept.
#[tauri::command]
fn connect(
    app: AppHandle,
    shared: State<'_, Shared>,
    id: String,
    password: Option<String>,
) -> Result<(), String> {
    let password = password.filter(|p| !p.is_empty()).map(Zeroizing::new);
    let id: DeviceId = id.parse().map_err(|e| format!("{e}"))?;
    {
        let mut view = shared.view.lock().expect("lock");
        if matches!(
            view.viewer,
            ViewerState::Asking { .. } | ViewerState::Connected
        ) {
            return Err("Já existe uma conexão aberta.".into());
        }
        if view.id.as_deref() == Some(id.to_string().as_str()) {
            return Err("Esse é o ID deste computador.".into());
        }
        view.viewer = ViewerState::Asking { locked: false };
    }
    if let Err(e) = open_viewer(app.clone(), id, password) {
        let message = format!("Não foi possível abrir a conexão: {e:#}");
        shared.view.lock().expect("lock").viewer = ViewerState::Failed {
            message: message.clone(),
        };
        return Err(message);
    }
    changed(&app);
    Ok(())
}

/// Takes a computer off the list.
#[tauri::command]
fn forget(app: AppHandle, shared: State<'_, Shared>, id: String) {
    let mut recent = Recent::load();
    recent.forget(&id);
    recent.save();
    shared.view.lock().expect("lock").recent = recent.into_list();
    changed(&app);
}

/// A viewer connected to computer `id`, called `name`: on the list it goes.
fn remember(app: &AppHandle, id: &str, name: &str) {
    let mut recent = Recent::load();
    recent.visited(id, name);
    recent.save();
    let shared = app.state::<Shared>();
    let mut view = shared.view.lock().expect("lock");
    view.viewer = ViewerState::Connected;
    view.recent = recent.into_list();
}

/// Opens the viewer, a process of its own (ARCHITECTURE 3.5), and follows it.
fn open_viewer(app: AppHandle, id: DeviceId, password: Option<Zeroizing<String>>) -> Result<()> {
    // Not the key the portable host is using: our relay serves one
    // connection per key, so the two would keep knocking each other off.
    let key = hext_crypto::config_dir()?.join("viewer.key");
    let number = app
        .state::<Shared>()
        .next_viewer
        .fetch_add(1, Ordering::Relaxed);
    let label = format!("chat-viewer-{number}");
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("view")
        .arg(id.get().to_string())
        .arg("--device-key")
        .arg(key)
        .arg("--report-to-ui")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    if password.is_some() {
        // Through its input: an argument could be read in the process list.
        command.arg("--password-stdin");
    }
    let mut child = command.spawn().context("iniciar o visualizador")?;
    let mut stdin = child.stdin.take().context("entrada do visualizador")?;
    if let Some(password) = &password {
        writeln!(stdin, "{}", password.as_str()).context("passar a senha ao visualizador")?;
    }
    let shared = app.state::<Shared>();
    let mut viewers = shared.viewers.lock().expect("lock");
    viewers.insert(label.clone(), stdin);
    drop(viewers);
    let stdout = child.stdout.take().context("saída do visualizador")?;
    let stderr = child.stderr.take().context("erros do visualizador")?;

    let connected = app.clone();
    let chat_label = label.clone();
    std::thread::spawn(move || {
        let in_chat = |author, text: String| {
            let shared = connected.state::<Shared>();
            if let Some(chat) = shared.chats.lock().expect("lock").get_mut(&chat_label) {
                chat.push(author, text);
            }
            changed(&connected);
        };
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(name) = line.strip_prefix(VIEWER_CONNECTED) {
                let chat = Chat::new(name.trim().to_owned());
                let shared = connected.state::<Shared>();
                let mut chats = shared.chats.lock().expect("lock");
                chats.insert(chat_label.clone(), chat);
                drop(chats);
                remember(&connected, &id.to_string(), name.trim());
                changed(&connected);
            } else if line == VIEWER_CHAT_OPEN {
                open_chat(&connected, chat_label.clone(), true);
            } else if line == VIEWER_CHAT_NO_ONE {
                let text = "Ninguém recebeu: não há usuário no computador, ou a conexão caiu.";
                in_chat(Author::App, text.into());
                open_chat(&connected, chat_label.clone(), false);
            } else if let Some(text) = chat_of_viewer_line(&line) {
                in_chat(Author::Them, text.into());
                open_chat(&connected, chat_label.clone(), false);
            } else if line == HOST_LOCKED {
                connected
                    .state::<Shared>()
                    .view
                    .lock()
                    .expect("lock")
                    .viewer = ViewerState::Asking { locked: true };
                changed(&connected);
            }
        }
    });
    std::thread::spawn(move || {
        // What `main` prints when it fails: `Error: <the reason>`.
        let failure = BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
            .find_map(|line| line.strip_prefix("Error: ").map(str::to_owned));
        let status = child.wait();
        let state = match (status, failure) {
            (Ok(status), _) if status.success() => ViewerState::Idle,
            (_, Some(message)) => ViewerState::Failed { message },
            _ => ViewerState::Failed {
                message: "A conexão terminou com um erro.".into(),
            },
        };
        let shared = app.state::<Shared>();
        shared.view.lock().expect("lock").viewer = state;
        shared.viewers.lock().expect("lock").remove(&label);
        if let Some(chat) = shared.chats.lock().expect("lock").get_mut(&label) {
            chat.live = false;
        }
        changed(&app);
    });
    Ok(())
}

/// Double-clicked, a console app gets a console of its own: not wanted
/// behind a window. Started from a terminal, it stays attached to it.
#[cfg(windows)]
#[allow(unsafe_code)] // Win32 calls
fn drop_own_console() {
    use windows::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
    let mut processes = [0u32; 2];
    if unsafe { GetConsoleProcessList(&mut processes) } == 1 {
        let _ = unsafe { FreeConsole() };
    }
}

pub fn run(link: Link) -> Result<()> {
    #[cfg(windows)]
    drop_own_console();
    // Started with no window; stays running with none; restarts the computer.
    #[cfg(windows)]
    let (hidden, stays, can_restart) = {
        let service = matches!(link, Link::Service(_));
        (service, service, service)
    };
    #[cfg(target_os = "macos")]
    let (hidden, stays, can_restart) = {
        let installed = crate::service::installed();
        let login = matches!(link, Link::Login);
        (login, login || installed, installed)
    };
    let (to_host, outgoing) = mpsc::unbounded_channel();

    let app = tauri::Builder::default()
        // A second start brings this one forward (it would be a second host),
        // unless it is the service's interface finding the portable app here:
        // it tries again while the app stays open, with the service asking
        // through its message box meanwhile.
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            if !args.iter().any(|arg| arg == "ui") {
                open_home(app);
            } else if cfg!(target_os = "macos") {
                // The login's: this one takes its place, and stays.
                app.state::<Shared>().managed.store(true, Ordering::Relaxed);
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .manage(Shared {
            view: Mutex::new(View {
                recent: Recent::load().into_list(),
                ..View::default()
            }),
            to_host,
            managed: AtomicBool::new(stays),
            can_restart,
            chats: Mutex::default(),
            viewers: Mutex::default(),
            next_viewer: AtomicU64::new(1),
        })
        .invoke_handler(tauri::generate_handler![
            view,
            answer,
            end_session,
            fold_bar,
            send_files,
            chat_view,
            chat_send,
            open_host_chat,
            connect,
            forget
        ])
        .on_window_event(move |window, event| {
            let WindowEvent::CloseRequested { api, .. } = event else {
                return;
            };
            let app = window.app_handle();
            match window.label() {
                // The indicator stays for as long as there is a session.
                "bar" => api.prevent_close(),
                // Closing the question is a no.
                "ask" => {
                    api.prevent_close();
                    let app = app.clone();
                    std::thread::spawn(move || refuse(&app));
                }
                // Portable, the last window takes the host with it: not in
                // the middle of a request or a session.
                "main" if !managed(app) && (wanted(app, "ask") || wanted(app, "bar")) => {
                    api.prevent_close();
                    let _ = window.minimize();
                }
                _ => {}
            }
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            // In the background, like the system's own: no Dock icon.
            #[cfg(target_os = "macos")]
            if hidden {
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }
            if !hidden {
                open_home(&handle);
            }
            connect_to_host(handle, link, outgoing);
            Ok(())
        })
        .build(tauri::generate_context!())
        .context("abrir a interface")?;
    app.run(move |app, event| match event {
        // With no window open: the service's interface waits for the next
        // request; the portable one is done.
        RunEvent::ExitRequested {
            code: None, api, ..
        } if managed(app) => api.prevent_exit(),
        // The Dock icon of an app with no window.
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => open_home(app),
        // (Here and not on the window: only by now can the label be reused.)
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::Destroyed,
            ..
        } => {
            reopen(app, &label);
            // Closed for good: what was said goes with it.
            let shared = app.state::<Shared>();
            let mut chats = shared.chats.lock().expect("lock");
            if chats.get(&label).is_some_and(|chat| !chat.live) {
                chats.remove(&label);
            }
        }
        _ => {}
    });
    Ok(())
}
