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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use hext_proto::{ChatText, Permissions};
use hext_signal::{AccountCall, AccountReply, AccountView, DeviceId, HostName};
use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, RunEvent, State, WebviewUrl,
    WebviewWindow, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_dialog::DialogExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Notify, mpsc};
use tracing::warn;
use zeroize::Zeroizing;

use crate::account::Account;
use crate::agent::{read_msg, write_msg};
use crate::chat::{Author, Chat, HOST_CHAT};
use crate::recent::{Computer, Recent};
use crate::ui_link::{
    FromUi, HOST_LOCKED, SessionView, ToUi, ToViewer, UiHub, VIEWER_CHAT_NO_ONE, VIEWER_CHAT_OPEN,
    VIEWER_CONNECTED, chat_of_viewer_line, ending_of_viewer_line, of_session_line,
};

/// Tells the pages to read [`View`] again.
const CHANGED: &str = "changed";
/// The viewer's key, next to this computer's: the one the computers on the
/// list know.
const VIEWER_KEY: &str = "viewer.key";
/// Before the session's number, the label of a viewer session's chat window.
const VIEWER_CHAT_PREFIX: &str = "chat-viewer-";
/// Computers open at once, at most: each window decodes its own video.
const MAX_VIEWERS: usize = 8;
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

/// A computer opened from the home screen, each in a window of the viewer.
#[derive(Debug, Clone, Serialize)]
struct ViewerItem {
    /// The viewer's number for it.
    session: u64,
    id: String,
    /// What it is called, once connected.
    name: Option<String>,
    /// Waiting for someone there to accept.
    asking: bool,
    /// Its screen is locked: someone must wake it.
    locked: bool,
    /// Which viewer process holds it.
    #[serde(skip)]
    hub: u64,
    /// Asked, with the password, to bind to the account: once connected,
    /// it goes in the account's list (Meus computadores).
    #[serde(skip)]
    to_account: bool,
}

/// What the pages show.
#[derive(Debug, Clone, Default, Serialize)]
struct View {
    id: Option<String>,
    /// This computer's name, as viewers see it.
    name: Option<String>,
    notice: Option<String>,
    ask: Option<AskView>,
    session: Option<SessionView>,
    /// The computers open, or opening, from here.
    viewers: Vec<ViewerItem>,
    /// Why the last one that failed did.
    viewer_failure: Option<String>,
    /// What the last one to open did besides (put in the account).
    viewer_news: Option<String>,
    /// The computers this one accessed, the latest first.
    recent: Vec<Computer>,
    /// The IDs on that list that are online; `None`: not known.
    online: Option<Vec<String>>,
    /// How the last file of the session went.
    transfer: Option<String>,
    /// Counts what the bar should unfold for.
    transfer_news: u64,
    /// Counts the sessions: a file is chosen for the one that was on.
    sessions: u64,
    /// The technicians this computer trusts without the password.
    technicians: Vec<crate::technicians::Technician>,
    /// Whether the unattended password is on (`None`: not the installed
    /// service, where there is no such password).
    password: Option<bool>,
    /// The account logged in here (F5), once the server said so.
    account: Option<AccountView>,
    /// This computer's host key (to put it in the account).
    #[serde(skip)]
    host_key: Option<String>,
    /// The account's computers (marco 4), as its list says.
    computers: Vec<Computer>,
    /// The names the computers on the lists go by now, by ID, as the server
    /// said with who is online: shown instead of the one on the account's
    /// list (the name a computer had when added).
    #[serde(skip)]
    names: HashMap<String, String>,
    /// The account bound to this computer (marco 5), as said.
    owners: Option<String>,
    /// A newer version (F6), where nothing installs it by itself.
    update: Option<UpdateView>,
    /// The portable app on Windows: it can install itself here.
    installable: bool,
    /// This app's version, shown in the sidebar.
    version: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct UpdateView {
    version: String,
    page: String,
}

/// How often the interface looks for a newer version.
const UPDATE_CHECK: std::time::Duration = std::time::Duration::from_secs(12 * 60 * 60);
/// How often the installed Mac app, which installs it by itself, looks (as
/// the Windows service does).
#[cfg(target_os = "macos")]
const SELF_UPDATE_CHECK: std::time::Duration = std::time::Duration::from_secs(60 * 60);
/// After a failed install, before the next try.
#[cfg(target_os = "macos")]
const SELF_UPDATE_RETRY: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// Looks for a newer version now and then, and shows it. Not with the
/// Windows service: it installs the new one by itself. Neither does the
/// installed Mac app, signed and where it may replace itself: it installs it
/// between sessions and opens again.
fn watch_updates(app: AppHandle) {
    if !crate::update::enabled() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        loop {
            #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
            let mut wait = UPDATE_CHECK;
            if !(cfg!(windows) && managed(&app)) {
                match crate::update::check().await {
                    Ok(found) => {
                        #[cfg(target_os = "macos")]
                        if managed(&app) && crate::service::updatable_app().is_some() {
                            wait = SELF_UPDATE_CHECK;
                            if let Some(manifest) = &found {
                                if !idle(&app) {
                                    tracing::info!("versão {}: depois da sessão", manifest.version);
                                } else {
                                    match crate::service::update_app(manifest).await {
                                        // launchd opens the new one (the agent
                                        // restarts when it exits with a failure).
                                        Ok(()) => {
                                            tracing::info!(
                                                "atualizado para {}: reabrindo",
                                                manifest.version
                                            );
                                            std::process::exit(1);
                                        }
                                        Err(e) => {
                                            warn!("atualização para {}: {e:#}", manifest.version);
                                            wait = SELF_UPDATE_RETRY;
                                        }
                                    }
                                }
                            }
                        }
                        let update = found.map(|m| UpdateView {
                            page: m.page(),
                            version: m.version,
                        });
                        app.state::<Shared>().view.lock().expect("lock").update = update;
                        changed(&app);
                    }
                    Err(e) => warn!("atualização: {e:#}"),
                }
            }
            tokio::time::sleep(wait).await;
        }
    });
}

/// Nothing going on that an update would cut: no session on this computer,
/// no computer open from here.
#[cfg(target_os = "macos")]
fn idle(app: &AppHandle) -> bool {
    let shared = app.state::<Shared>();
    // (One lock at a time.)
    let quiet = {
        let view = shared.view.lock().expect("lock");
        view.session.is_none() && view.viewers.is_empty()
    };
    quiet && shared.hub.lock().expect("lock").is_none()
}

/// Installs the Hext Remote on this computer, from the portable app.
/// Windows asks for an administrator. Done, this app closes: the service
/// opens its own window in its place (with this computer's installed ID).
#[tauri::command]
async fn install_here(app: AppHandle) -> Result<(), String> {
    #[cfg(windows)]
    {
        if managed(&app) || crate::service::installed() {
            return Err("O Hext Remote já está instalado neste computador.".to_owned());
        }
        if app
            .state::<Shared>()
            .view
            .lock()
            .expect("lock")
            .session
            .is_some()
        {
            return Err("Termine a sessão antes de instalar.".to_owned());
        }
        install_this_computer()
            .await
            .map_err(|e| format!("{e:#}"))?;
        let closing = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            closing.exit(0);
        });
        Ok(())
    }
    // The Mac's is the user's own (a LaunchAgent): no administrator. Done
    // after this window closes, since the installed copy gives way to one
    // already open; it then opens in its place.
    #[cfg(target_os = "macos")]
    {
        if crate::service::installed() {
            return Err("O Hext Remote já está instalado neste Mac.".to_owned());
        }
        if app
            .state::<Shared>()
            .view
            .lock()
            .expect("lock")
            .session
            .is_some()
        {
            return Err("Termine a sessão antes de instalar.".to_owned());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        std::process::Command::new("/bin/sh")
            .args(["-c", r#"sleep 2; exec "$0" service install"#])
            .arg(&exe)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("instalar: {e}"))?;
        let closing = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            closing.exit(0);
        });
        Ok(())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = app;
        Err("Só no Windows e no Mac.".to_owned())
    }
}

/// The signed `.msi` of the latest release, when this build knows the
/// release key (it then shows in Apps, and uninstalls there); otherwise
/// this program itself, as the service (`service install`).
#[cfg(windows)]
async fn install_this_computer() -> anyhow::Result<()> {
    if crate::update::enabled() {
        match crate::update::latest().await {
            Ok(manifest) => {
                // A folder nobody could have made ready: the name is new.
                let mut nonce = [0u8; 16];
                ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce)
                    .map_err(|_| anyhow::anyhow!("sem números aleatórios"))?;
                let dir = std::env::temp_dir().join(format!("HextRemote-{}", hex::encode(nonce)));
                std::fs::create_dir(&dir)?;
                let installed = async {
                    let msi = crate::update::download(&manifest, "windows-msi", &dir).await?;
                    let expected = manifest
                        .files
                        .get("windows-msi")
                        .map(|asset| asset.sha256.clone())
                        .unwrap_or_default();
                    tracing::info!("instalar: {}", msi.display());
                    // (The installer asks for the administrator itself.)
                    tokio::task::spawn_blocking(move || run_checked_msi(&msi, &expected)).await?
                }
                .await;
                let _ = std::fs::remove_dir_all(&dir);
                let status = installed?;
                return match status.code() {
                    Some(0 | 3010) => Ok(()),
                    Some(1602) => anyhow::bail!("Instalação cancelada."),
                    code => anyhow::bail!("O instalador terminou com erro ({code:?})."),
                };
            }
            Err(e) => warn!("instalar: sem a versão publicada ({e:#}); vai este programa"),
        }
    }
    let code = tokio::task::spawn_blocking(|| run_elevated_wait("service install")).await??;
    if code != 0 {
        anyhow::bail!("A instalação falhou ({code}).");
    }
    Ok(())
}

/// Runs the installer at `msi` once it is what the list says (`sha256`, hex),
/// held so that nothing changes it between the check and the installer, which
/// runs as administrator: the file is open with others allowed only to read,
/// so no other program of this user can write, replace or move it meanwhile.
#[cfg(windows)]
fn run_checked_msi(
    msi: &std::path::Path,
    sha256: &str,
) -> anyhow::Result<std::process::ExitStatus> {
    use std::io::Read;
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    let mut held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(msi)?;
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = held.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        context.update(&buffer[..read]);
    }
    if hex::encode(context.finish()) != sha256.to_ascii_lowercase() {
        anyhow::bail!("O instalador baixado não confere com a lista.");
    }
    let root = std::env::var_os("SystemRoot").context("SystemRoot não definido")?;
    let status = std::process::Command::new(
        std::path::Path::new(&root)
            .join("System32")
            .join("msiexec.exe"),
    )
    .arg("/i")
    .arg(msi)
    .status()?;
    drop(held);
    Ok(status)
}

/// Runs this program with `args` as administrator (Windows asks), hidden,
/// and waits for it: its exit code.
#[cfg(windows)]
#[allow(unsafe_code)] // Win32 calls, on handles this function owns
pub(crate) fn run_elevated_wait(args: &str) -> anyhow::Result<u32> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
    use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    use windows::core::{HSTRING, PCWSTR, w};
    let exe = HSTRING::from(std::env::current_exe()?.as_os_str());
    let params = HSTRING::from(args);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // (It fails, most often, because the user said no.)
    if unsafe { ShellExecuteExW(&mut info) }.is_err() || info.hProcess.is_invalid() {
        anyhow::bail!("Nada foi instalado: é preciso permitir como administrador.");
    }
    let mut code = 1;
    unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        let got = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        got?;
    }
    Ok(code)
}

/// Opens the new version's page in the browser.
#[tauri::command]
fn open_update(shared: State<'_, Shared>) -> Result<(), String> {
    let page = shared
        .view
        .lock()
        .expect("lock")
        .update
        .as_ref()
        .map(|u| u.page.clone())
        .ok_or("Nenhuma versão nova.")?;
    open_url(&page).map_err(|e| e.to_string())
}

/// `url` in the system's browser.
fn open_url(url: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let mut command = std::process::Command::new("rundll32");
        command.args(["url.dll,FileProtocolHandler", url]);
        command.creation_flags(0x0800_0000); // no console window
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = std::process::Command::new("/usr/bin/open");
        command.arg(url);
        command
    };
    command.spawn().map(drop)
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
    /// The viewer running (which one, and where it takes what it is asked).
    hub: Mutex<Option<(u64, ChildStdin)>>,
    next_hub: AtomicU64,
    /// Numbers the viewer sessions.
    next_viewer: AtomicU64,
    /// Asks the server at once who on the list is online.
    presence: Arc<Notify>,
    /// The account of this computer's technician (`None`: no folder for it).
    account: Option<Arc<Account>>,
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
                .inner_size(940.0, 620.0)
                .min_inner_size(560.0, 480.0)
                .center()
        });
        match home {
            Some(home) => {
                let _ = home.unminimize();
                let _ = home.show();
                let _ = home.set_focus();
                app.state::<Shared>().presence.notify_one();
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
        ToUi::Name(name) => view.name = Some(name),
        ToUi::Key(key) => view.host_key = Some(key),
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
        ToUi::Technicians(list) => view.technicians = list,
        ToUi::Password(on) => view.password = Some(on),
        ToUi::Owners(text) => view.owners = text,
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

/// Stops trusting a technician without the password (by its key). Anyone at
/// the computer may: it only takes access away.
#[tauri::command]
fn revoke_technician(shared: State<'_, Shared>, key: String) {
    shared.send(FromUi::Revoke(key));
}

/// Gives this computer another name (empty: back to the system's). The
/// service's takes an administrator: Windows asks for one.
#[tauri::command]
fn rename(shared: State<'_, Shared>, name: String) -> Result<(), String> {
    let typed = name.trim().to_owned();
    if !typed.is_empty() {
        crate::name::clean(&typed).map_err(|e| e.to_string())?;
    }
    #[cfg(windows)]
    if shared.managed.load(Ordering::Relaxed) {
        return rename_service(&typed);
    }
    shared.send(FromUi::Rename(typed));
    Ok(())
}

/// Runs `service name` as an administrator (Windows asks). The service
/// restarts with the name, and this interface with it.
#[cfg(windows)]
fn rename_service(name: &str) -> Result<(), String> {
    let args = if name.is_empty() {
        "service name --reset".to_owned()
    } else {
        format!("service name {}", quote_arg(name))
    };
    run_elevated(
        &args,
        "O nome não mudou: é preciso permitir como administrador.",
    )
}

/// Runs this program with `args` as an administrator (Windows asks);
/// `refused` if it did not start.
#[cfg(windows)]
#[allow(unsafe_code)] // Win32 call
fn run_elevated(args: &str, refused: &str) -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    use windows::core::{HSTRING, w};
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let done = unsafe {
        ShellExecuteW(
            None,
            w!("runas"),
            &HSTRING::from(exe.as_os_str()),
            &HSTRING::from(args),
            None,
            SW_HIDE,
        )
    };
    // (Above 32: it started. Otherwise, most often, the user said no.)
    if done.0 as usize > 32 {
        Ok(())
    } else {
        Err(refused.to_owned())
    }
}

/// `text` as one argument of a Windows command line.
#[cfg(any(windows, test))]
fn quote_arg(text: &str) -> String {
    // Backslashes count only before a quote: doubled there, and one more
    // escapes the quote itself.
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in text.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
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
        session_of_chat(label)
            .is_some_and(|n| tell_viewer(&shared, &ToViewer::Chat(n, text.clone())))
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
    remember: Option<bool>,
    account: Option<bool>,
) -> Result<(), String> {
    let password = password.filter(|p| !p.is_empty()).map(Zeroizing::new);
    let remember = remember.unwrap_or(false) && password.is_some();
    // With the password, the computer can bind to this device's account
    // (it checks the ask, signed by this device, and the account's list).
    let account = match account.unwrap_or(false) && password.is_some() {
        true => Some(the_account(&shared)?.chain_id().ok_or(
            "Para inserir o computador na conta, entre na conta e crie a lista de dispositivos.",
        )?),
        false => None,
    };
    let id: DeviceId = id.parse().map_err(|e| format!("{e}"))?;
    let shown = id.to_string();
    let n = shared.next_viewer.fetch_add(1, Ordering::Relaxed);
    {
        let mut view = shared.view.lock().expect("lock");
        if view.id.as_deref() == Some(shown.as_str()) {
            return Err("Esse é o ID deste computador.".into());
        }
        // (Each host takes one session at a time anyway.)
        if view.viewers.iter().any(|v| v.id == shown) {
            return Err("Esse computador já está aberto.".into());
        }
        if view.viewers.len() >= MAX_VIEWERS {
            return Err("Feche uma das conexões antes de abrir outra.".into());
        }
        view.viewer_failure = None;
        view.viewer_news = None;
        view.viewers.push(ViewerItem {
            session: n,
            id: shown,
            name: None,
            asking: true,
            locked: false,
            hub: 0,
            to_account: account.is_some(),
        });
    }
    if let Err(e) = open_viewer(&app, n, id, (password, remember, account)) {
        let message = format!("Não foi possível abrir a conexão: {e:#}");
        let mut view = shared.view.lock().expect("lock");
        view.viewers.retain(|v| v.session != n);
        view.viewer_failure = Some(message.clone());
        drop(view);
        changed(&app);
        return Err(message);
    }
    changed(&app);
    Ok(())
}

/// Puts computer `id`, called `name`, which a session just reached with its
/// password (asking it to bind to the account), in the account's list: from
/// then on the account's devices come in without the password. Its key is
/// the one the session proved: the viewer writes it to `known_hosts` only
/// once in (with the password, once the computer proved it knows it), before
/// it says it is connected. The outcome shows on the home screen.
fn put_in_account(app: AppHandle, id: String, name: String) {
    tauri::async_runtime::spawn(async move {
        let shared = app.state::<Shared>();
        let done = async {
            let key = hext_crypto::config_dir()
                .ok()
                .and_then(|dir| {
                    let known = hext_signal::KnownHosts::new(dir.join("known_hosts"));
                    known.get(id.parse().ok()?).ok()?
                })
                .ok_or_else(|| "a chave do computador não ficou guardada".to_owned())?;
            the_account(&shared)?
                .adopt(&key.to_string(), &id, &name)
                .await
        };
        let outcome = done.await;
        {
            let mut view = shared.view.lock().expect("lock");
            match outcome {
                Ok(_) => {
                    view.viewer_news = Some(format!(
                        "{name} foi inserido em Meus computadores. Com o Hext instalado nele \
                         (Windows), os dispositivos da conta passam a entrar sem senha."
                    ));
                    if !view.computers.iter().any(|c| c.id == id) {
                        view.computers.push(Computer {
                            id: id.clone(),
                            name: name.clone(),
                            host: None,
                        });
                    }
                }
                Err(e) => {
                    view.viewer_failure =
                        Some(format!("{name} não foi inserido em Meus computadores: {e}"));
                }
            }
        }
        changed(&app);
        shared.presence.notify_one();
    });
}

/// Takes a computer off the list.
#[tauri::command]
fn forget(app: AppHandle, shared: State<'_, Shared>, id: String) {
    let mut recent = Recent::load();
    recent.forget(&id);
    recent.save();
    shared.view.lock().expect("lock").recent = recent.into_list();
    changed(&app);
    shared.presence.notify_one();
}

/// A viewer connected to computer `id`, called `name`: on the list it goes.
fn remember(app: &AppHandle, id: &str, name: &str) {
    let mut recent = Recent::load();
    recent.visited(id, name);
    recent.save();
    let shared = app.state::<Shared>();
    let mut view = shared.view.lock().expect("lock");
    view.recent = recent.into_list();
    shared.presence.notify_one();
}

/// An account call from the home screen (see [`Account::call`]). The view
/// follows what it says of the account.
#[tauri::command]
async fn account(
    app: AppHandle,
    shared: State<'_, Shared>,
    call: AccountCall,
) -> Result<AccountReply, String> {
    let Some(account) = shared.account.clone() else {
        return Err("Sem uma pasta para guardar a conta neste computador.".into());
    };
    let answer = account.call(call).await;
    {
        let mut view = shared.view.lock().expect("lock");
        match &answer {
            _ if !account.kept() => {
                view.account = None;
                view.computers.clear();
            }
            Ok(AccountReply::LoggedIn { account, .. } | AccountReply::Account(account)) => {
                view.account = Some(account.clone());
            }
            Ok(AccountReply::RecoveryCodes { .. }) => {
                if let Some(account) = &mut view.account {
                    account.totp = true;
                }
            }
            _ => {}
        }
    }
    changed(&app);
    answer
}

fn the_account(shared: &Shared) -> Result<Arc<Account>, String> {
    shared
        .account
        .clone()
        .ok_or_else(|| "Sem uma pasta para guardar a conta neste computador.".to_owned())
}

/// The account's devices and computers (marcos 3 and 4). The home screen
/// lists the computers.
#[tauri::command]
async fn account_devices(
    app: AppHandle,
    shared: State<'_, Shared>,
) -> Result<crate::account::DevicesView, String> {
    let host = shared.view.lock().expect("lock").host_key.clone();
    let mut devices = the_account(&shared)?.devices(host.as_deref()).await;
    if let Ok(devices) = devices.as_mut() {
        devices.bindable = cfg!(windows) && shared.managed.load(Ordering::Relaxed);
    }
    let computers = devices
        .as_ref()
        .map(|d| {
            d.computers
                .iter()
                .filter(|c| !c.me)
                .map(|c| Computer {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    host: Some(c.host.clone()),
                })
                .chain(d.shared.iter().map(|c| Computer {
                    id: c.id.clone(),
                    name: format!("{}{SHARED}", c.name),
                    host: None,
                }))
                .collect()
        })
        .unwrap_or_default();
    let changed_now = {
        let mut view = shared.view.lock().expect("lock");
        let mut computers: Vec<Computer> = computers;
        rename_all(&mut computers, &view.names);
        let before = std::mem::replace(&mut view.computers, computers);
        before != view.computers
    };
    if changed_now {
        changed(&app);
        shared.presence.notify_one();
    }
    devices
}

/// Puts this computer in the account's list (marco 4): the account's
/// devices see it in Meus computadores.
#[tauri::command]
async fn computer_add(shared: State<'_, Shared>) -> Result<(), String> {
    let (key, id, name) = {
        let view = shared.view.lock().expect("lock");
        (view.host_key.clone(), view.id.clone(), view.name.clone())
    };
    let (Some(key), Some(id)) = (key, id) else {
        return Err("Este computador ainda não tem um ID.".into());
    };
    let name = name.unwrap_or_else(crate::device_name);
    the_account(&shared)?.add_computer(&key, &id, &name).await
}

/// Binds this computer to the account (marco 5): its devices come in
/// without anyone accepting. It takes an administrator: Windows asks.
#[tauri::command]
fn account_bind(shared: State<'_, Shared>, chain: String) -> Result<(), String> {
    let chain = chain.trim().to_owned();
    crate::owners::parse_id(&chain).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    if shared.managed.load(Ordering::Relaxed) {
        return run_elevated(
            &format!("service account {chain}"),
            "Nada mudou: é preciso permitir como administrador.",
        );
    }
    let _ = shared;
    // (The Mac's access is its user's: no administrator.)
    #[cfg(target_os = "macos")]
    if crate::service::installed() {
        return crate::service::account_from_app(Some(chain.as_str()))
            .map_err(|e| format!("{e:#}"));
    }
    Err("Só com o serviço instalado (HextRemote service install).".to_owned())
}

/// Sets (or changes) the unattended password typed in the Segurança tab
/// (`Some`), or turns it off (`None`). On Windows it takes an administrator:
/// Windows asks, and nothing else opens. Waits for it.
#[tauri::command]
async fn password_set(password: Option<String>) -> Result<(), String> {
    crate::service::set_password_from_app(password)
        .await
        .map_err(|e| format!("{e:#}"))
}

/// Unbinds this computer from the account: every access asks again. It
/// takes an administrator: Windows asks. Waits for it, to tell how it went
/// (the home screen follows within seconds).
#[tauri::command]
async fn account_unbind() -> Result<(), String> {
    #[cfg(windows)]
    let result = {
        let done =
            tauri::async_runtime::spawn_blocking(|| run_elevated_wait("service account --off"))
                .await
                .map_err(|e| e.to_string())?;
        match done {
            Ok(0) => Ok(()),
            Ok(code) => Err(format!("Não foi possível desligar ({code}).")),
            Err(_) => Err("Nada mudou: é preciso permitir como administrador.".to_owned()),
        }
    };
    #[cfg(not(windows))]
    let result = crate::service::account_from_app(None).map_err(|e| format!("{e:#}"));
    result
}

/// Opens one of the account's computers to another account (marco 6).
#[tauri::command]
#[allow(clippy::too_many_arguments)] // as the page sends them
async fn share_add(
    shared: State<'_, Shared>,
    host: String,
    with: String,
    name: String,
    control: bool,
    clipboard: bool,
    files: bool,
    restart: bool,
    days: Option<u32>,
) -> Result<(), String> {
    let allowed = hext_signal::chain::Allowed {
        control,
        clipboard,
        files,
        restart,
    };
    the_account(&shared)?
        .share(&host, &with, &name, allowed, days)
        .await
}

/// The accesses to the account's computers (marco 7).
#[tauri::command]
async fn account_activity(
    shared: State<'_, Shared>,
) -> Result<Vec<crate::account::ActivityItem>, String> {
    the_account(&shared)?.activity().await
}

/// Takes a computer back from another account.
#[tauri::command]
async fn share_remove(shared: State<'_, Shared>, host: String, with: String) -> Result<(), String> {
    the_account(&shared)?.unshare(&host, &with).await
}

/// Takes a computer out of the account's list.
#[tauri::command]
async fn computer_remove(shared: State<'_, Shared>, host: String) -> Result<(), String> {
    the_account(&shared)?.remove_computer(&host).await
}

/// Starts the account's list of devices with this one; the recovery code.
#[tauri::command]
async fn chain_create(shared: State<'_, Shared>) -> Result<String, String> {
    the_account(&shared)?.create_chain().await
}

/// A code to show, for adding another device.
#[tauri::command]
fn chain_invite() -> String {
    hext_signal::chain::new_code(hext_signal::chain::JOIN_CODE_LEN)
}

/// Lets in the device that typed `code`, if one did by now; its name.
#[tauri::command]
async fn chain_approve(shared: State<'_, Shared>, code: String) -> Result<Option<String>, String> {
    the_account(&shared)?.approve(&code).await
}

/// Asks to join the account's list, with the code another device shows.
#[tauri::command]
async fn chain_join(shared: State<'_, Shared>, code: String) -> Result<(), String> {
    the_account(&shared)?.join(&code).await
}

/// Joins the account's list with the recovery code.
#[tauri::command]
async fn chain_recover(shared: State<'_, Shared>, code: String) -> Result<(), String> {
    the_account(&shared)?.recover(&code).await
}

/// Takes a device out of the account's list.
#[tauri::command]
async fn chain_remove(shared: State<'_, Shared>, key: String) -> Result<(), String> {
    the_account(&shared)?.remove(&key).await
}

/// The authenticator link as a QR code (SVG).
#[tauri::command]
fn account_qr(text: String) -> Option<String> {
    crate::account::qr_svg(&text)
}

/// Asks the server which account is logged in here, once at the start.
fn show_account(app: AppHandle) {
    let Some(account) = app.state::<Shared>().account.clone() else {
        return;
    };
    if !account.kept() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let current = account.current().await;
        let shared = app.state::<Shared>();
        shared.view.lock().expect("lock").account = current;
        changed(&app);
    });
}

/// What a computer shared by another account is called on the list.
const SHARED: &str = " (compartilhado)";

/// Shows the names in `names` (by ID) instead of the ones on `list`: a
/// computer renamed (`service name`) is listed by its name of now. Whether
/// one changed.
fn rename_all(list: &mut [Computer], names: &HashMap<String, String>) -> bool {
    let mut changed = false;
    for computer in list {
        let Some(name) = names.get(&computer.id) else {
            continue;
        };
        let shown = if computer.host.is_none() && computer.name.ends_with(SHARED) {
            format!("{name}{SHARED}")
        } else {
            name.clone()
        };
        if computer.name != shown {
            computer.name = shown;
            changed = true;
        }
    }
    changed
}

/// Keeps [`View::online`] up to date while the home screen is open.
fn watch_presence(app: AppHandle) {
    let Ok(dir) = hext_crypto::config_dir() else {
        return;
    };
    let wake = app.state::<Shared>().presence.clone();
    let asking = app.clone();
    let wanted = move || -> Vec<DeviceId> {
        let open = asking
            .get_webview_window("main")
            .is_some_and(|home| home.is_visible().unwrap_or(false));
        if !open {
            return Vec::new();
        }
        let shared = asking.state::<Shared>();
        let view = shared.view.lock().expect("lock");
        view.recent
            .iter()
            .chain(&view.computers)
            .filter_map(|c| c.id.parse().ok())
            .collect()
    };
    let show = move |answer: Option<(Vec<DeviceId>, Vec<HostName>)>| {
        let shared = app.state::<Shared>();
        let mut view = shared.view.lock().expect("lock");
        let (online, names) = answer.unzip();
        let mut renamed = false;
        if let Some(names) = names.filter(|names| !names.is_empty()) {
            let mut recent = Recent::load();
            let mut saved = false;
            for HostName { id, name } in names {
                saved |= recent.rename(&id.to_string(), &name);
                view.names.insert(id.to_string(), name);
            }
            if saved {
                recent.save();
            }
            let names = std::mem::take(&mut view.names);
            renamed |= rename_all(&mut view.recent, &names);
            renamed |= rename_all(&mut view.computers, &names);
            view.names = names;
        }
        let online = online.map(|online| {
            view.recent
                .iter()
                .chain(&view.computers)
                .filter(|c| c.id.parse().is_ok_and(|id: DeviceId| online.contains(&id)))
                .map(|c| c.id.clone())
                .collect()
        });
        if view.online != online || renamed {
            view.online = online;
            drop(view);
            changed(&app);
        }
    };
    tauri::async_runtime::spawn(crate::presence::watch(
        dir.join(VIEWER_KEY),
        wanted,
        show,
        wake,
    ));
}

/// The chat window of viewer session `n`.
fn chat_label(n: u64) -> String {
    format!("{VIEWER_CHAT_PREFIX}{n}")
}

/// The viewer session of chat window `label`.
fn session_of_chat(label: &str) -> Option<u64> {
    label.strip_prefix(VIEWER_CHAT_PREFIX)?.parse().ok()
}

/// Asks the viewer for something; false if there is no viewer, or it is gone.
fn tell_viewer(shared: &Shared, asked: &ToViewer) -> bool {
    let mut hub = shared.hub.lock().expect("lock");
    let Some((_, stdin)) = hub.as_mut() else {
        return false;
    };
    let line = asked.line();
    let told = writeln!(stdin, "{}", line.as_str()).and_then(|()| stdin.flush());
    if told.is_err() {
        *hub = None;
    }
    told.is_ok()
}

/// Opens computer `id` as viewer session `n`: in the viewer running, or a
/// new one. One viewer holds them all, each in its window: they share its
/// connection to the network (ARCHITECTURE 3.5 keeps it apart from this
/// process).
fn open_viewer(
    app: &AppHandle,
    n: u64,
    id: DeviceId,
    (password, remember, account): (Option<Zeroizing<String>>, bool, Option<[u8; 32]>),
) -> Result<()> {
    let shared = app.state::<Shared>();
    let asked = ToViewer::Open {
        session: n,
        id,
        password,
        remember,
        account,
    };
    let running = shared.hub.lock().expect("lock").as_ref().map(|(g, _)| *g);
    let generation = match running {
        Some(generation) if tell_viewer(&shared, &asked) => generation,
        // None running, or it just left.
        _ => {
            let ToViewer::Open {
                password,
                remember,
                account,
                ..
            } = asked
            else {
                unreachable!("an open");
            };
            start_viewer(app, n, id, (password, remember, account))?
        }
    };
    let mut view = shared.view.lock().expect("lock");
    if let Some(item) = view.viewers.iter_mut().find(|v| v.session == n) {
        item.hub = generation;
    }
    Ok(())
}

/// Starts the viewer, a process of its own (ARCHITECTURE 3.5), with session
/// `n`, and follows it; which one it is.
fn start_viewer(
    app: &AppHandle,
    n: u64,
    id: DeviceId,
    (password, remember, account): (Option<Zeroizing<String>>, bool, Option<[u8; 32]>),
) -> Result<u64> {
    // Not the key the portable host is using: our relay serves one
    // connection per key, so the two would keep knocking each other off.
    let key = hext_crypto::config_dir()?.join(VIEWER_KEY);
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("view")
        .arg(id.get().to_string())
        .arg("--device-key")
        .arg(key)
        .arg("--report-to-ui")
        .arg("--session")
        .arg(n.to_string())
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
        if let Some(chain) = account {
            command.arg("--account").arg(hex::encode(chain));
        } else if remember {
            command.arg("--remember");
        }
    }
    let mut child = command.spawn().context("iniciar o visualizador")?;
    let mut stdin = child.stdin.take().context("entrada do visualizador")?;
    if let Some(password) = &password {
        writeln!(stdin, "{}", password.as_str()).context("passar a senha ao visualizador")?;
    }
    let stdout = child.stdout.take().context("saída do visualizador")?;
    let stderr = child.stderr.take().context("erros do visualizador")?;
    let shared = app.state::<Shared>();
    let generation = shared.next_hub.fetch_add(1, Ordering::Relaxed);
    *shared.hub.lock().expect("lock") = Some((generation, stdin));

    let reading = app.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some((n, line)) = of_session_line(&line) {
                viewer_said(&reading, n, line);
            }
        }
    });
    let app = app.clone();
    std::thread::spawn(move || {
        // What `main` prints when it fails: `Error: <the reason>`.
        let failure = BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
            .find_map(|line| line.strip_prefix("Error: ").map(str::to_owned));
        let _ = child.wait();
        let shared = app.state::<Shared>();
        {
            let mut hub = shared.hub.lock().expect("lock");
            if hub.as_ref().is_some_and(|(g, _)| *g == generation) {
                *hub = None;
            }
        }
        // Whatever it still held is gone with it.
        let gone: Vec<ViewerItem> = {
            let mut view = shared.view.lock().expect("lock");
            let (gone, kept) = std::mem::take(&mut view.viewers)
                .into_iter()
                .partition(|v| v.hub == generation);
            view.viewers = kept;
            if gone.iter().any(|v| v.asking) {
                view.viewer_failure =
                    Some(failure.unwrap_or_else(|| "A conexão terminou com um erro.".to_owned()));
            }
            gone
        };
        let mut chats = shared.chats.lock().expect("lock");
        for item in gone {
            if let Some(chat) = chats.get_mut(&chat_label(item.session)) {
                chat.live = false;
            }
        }
        drop(chats);
        changed(&app);
    });
    Ok(generation)
}

/// A line of the viewer about session `n`.
fn viewer_said(app: &AppHandle, n: u64, line: &str) {
    let label = chat_label(n);
    let shared = app.state::<Shared>();
    let in_chat = |author, text: String| {
        if let Some(chat) = shared.chats.lock().expect("lock").get_mut(&label) {
            chat.push(author, text);
        }
        changed(app);
    };
    if let Some(name) = line.strip_prefix(VIEWER_CONNECTED) {
        let name = name.trim().to_owned();
        let chat = Chat::new(name.clone());
        shared.chats.lock().expect("lock").insert(label, chat);
        let (id, to_account) = {
            let mut view = shared.view.lock().expect("lock");
            let Some(item) = view.viewers.iter_mut().find(|v| v.session == n) else {
                return;
            };
            item.asking = false;
            item.locked = false;
            item.name = Some(name.clone());
            (item.id.clone(), std::mem::take(&mut item.to_account))
        };
        remember(app, &id, &name);
        if to_account {
            put_in_account(app.clone(), id, name);
        }
        changed(app);
    } else if line == VIEWER_CHAT_OPEN {
        open_chat(app, label, true);
    } else if line == VIEWER_CHAT_NO_ONE {
        let text = "Ninguém recebeu: não há usuário no computador, ou a conexão caiu.";
        in_chat(Author::App, text.into());
        open_chat(app, label, false);
    } else if let Some(text) = chat_of_viewer_line(line) {
        in_chat(Author::Them, text.into());
        open_chat(app, label, false);
    } else if line == HOST_LOCKED {
        let mut view = shared.view.lock().expect("lock");
        if let Some(item) = view.viewers.iter_mut().find(|v| v.session == n) {
            item.locked = true;
        }
        drop(view);
        changed(app);
    } else if let Some(reason) = ending_of_viewer_line(line) {
        {
            let mut view = shared.view.lock().expect("lock");
            if let Some(at) = view.viewers.iter().position(|v| v.session == n) {
                let item = view.viewers.remove(at);
                if let Some(reason) = reason {
                    let who = item.name.unwrap_or(item.id);
                    view.viewer_failure = Some(format!("{who}: {reason}"));
                }
            }
        }
        if let Some(chat) = shared.chats.lock().expect("lock").get_mut(&label) {
            chat.live = false;
        }
        changed(app);
    }
}

/// Brings the window of viewer session `session` forward.
#[tauri::command]
fn viewer_focus(shared: State<'_, Shared>, session: u64) {
    tell_viewer(&shared, &ToViewer::Focus(session));
}

/// Ends viewer session `session`.
#[tauri::command]
fn viewer_close(shared: State<'_, Shared>, session: u64) {
    tell_viewer(&shared, &ToViewer::Close(session));
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
                installable: cfg!(any(windows, target_os = "macos"))
                    && matches!(link, Link::Portable)
                    && !crate::service::installed(),
                version: crate::APP_VERSION,
                ..View::default()
            }),
            to_host,
            managed: AtomicBool::new(stays),
            can_restart,
            chats: Mutex::default(),
            hub: Mutex::default(),
            next_hub: AtomicU64::new(1),
            next_viewer: AtomicU64::new(1),
            presence: Arc::default(),
            account: hext_crypto::config_dir()
                .ok()
                .map(|dir| Arc::new(Account::new(dir.join(VIEWER_KEY)))),
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
            rename,
            revoke_technician,
            connect,
            viewer_focus,
            viewer_close,
            forget,
            account,
            account_qr,
            account_devices,
            chain_create,
            chain_invite,
            chain_approve,
            chain_join,
            chain_recover,
            chain_remove,
            computer_add,
            account_bind,
            account_unbind,
            password_set,
            share_add,
            share_remove,
            account_activity,
            open_update,
            install_here,
            computer_remove
        ])
        .on_window_event(move |window, event| {
            // Back on the home screen: who is online now.
            if matches!(event, WindowEvent::Focused(true)) && window.label() == "main" {
                window.app_handle().state::<Shared>().presence.notify_one();
            }
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
            watch_presence(handle.clone());
            watch_updates(handle.clone());
            show_account(handle.clone());
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

#[cfg(test)]
mod tests {
    use super::quote_arg;

    #[test]
    fn a_name_goes_as_one_windows_argument() {
        assert_eq!(quote_arg("Sala 2"), r#""Sala 2""#);
        assert_eq!(quote_arg(r#"a"b"#), r#""a\"b""#);
        assert_eq!(quote_arg(r"a\b"), r#""a\b""#);
        assert_eq!(quote_arg(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(quote_arg(r"fim\"), r#""fim\\""#);
    }
}
