//! The interface (Tauri 2): the home screen with the ID, the window that
//! asks about each request and the bar shown during a session.
//!
//! It always runs as the user, never as SYSTEM: a webview is far too much
//! to expose to a user's session with the system's rights. So the service
//! starts this in the user's session ([`Link::Service`]) and only trades
//! messages with it (see [`crate::ui_link`]). Portable, the host runs in
//! this same process ([`Link::Portable`]), with the same messages.
//!
//! Windows are made when needed: this stays running in every session of
//! an installed computer, and an idle webview is not cheap.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use anyhow::{Context, Result};
use hext_proto::Permissions;
use hext_signal::DeviceId;
use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, RunEvent, State, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, WindowEvent,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tracing::warn;
use zeroize::Zeroizing;

use crate::agent::{read_msg, write_msg};
use crate::recent::{Computer, Recent};
use crate::ui_link::{FromUi, HOST_LOCKED, SessionView, ToUi, UiHub, VIEWER_CONNECTED};

/// Tells the pages to read [`View`] again.
const CHANGED: &str = "changed";

/// Who owns the sessions.
pub enum Link {
    /// This process (the portable app).
    Portable,
    /// The service, through this pipe.
    #[cfg(windows)]
    Service(String),
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
}

struct Shared {
    view: Mutex<View>,
    to_host: mpsc::UnboundedSender<FromUi>,
    /// The sessions are the service's, not this process's.
    managed: bool,
}

impl Shared {
    fn send(&self, msg: FromUi) {
        let _ = self.to_host.send(msg);
    }
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
            None if !app.state::<Shared>().managed && app.webview_windows().is_empty() => {
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
    const WIDTH: f64 = 480.0;
    let app = app.clone();
    std::thread::spawn(move || {
        let bar = window(&app, "bar", "bar.html", |builder| {
            builder
                .title("Hext Remote: sessão ativa")
                .inner_size(WIDTH, 44.0)
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
            let _ = bar.set_position(PhysicalPosition::new(x, monitor.position().y + 8));
        }
    });
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
        ToUi::Notice(notice) => view.notice = Some(notice),
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
                can_restart: app.state::<Shared>().managed,
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
            view.session = Some(session);
            open_bar(app);
        }
        ToUi::SessionEnded => {
            view.session = None;
            dismiss(app, "bar");
        }
    }
    drop(view);
    changed(app);
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
                Link::Portable => {
                    let hub = UiHub::default();
                    let (ours, theirs) = tokio::io::duplex(1 << 16);
                    let attached = hub.clone();
                    tokio::spawn(async move { attached.attach(theirs).await });
                    tokio::spawn(async move {
                        if let Err(e) = crate::host::portable(hub.clone()).await {
                            warn!("{e:#}");
                            hub.set_notice(format!("Não foi possível receber conexões: {e:#}"));
                        }
                    });
                    talk(&app, ours, outgoing).await;
                }
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

#[tauri::command]
fn view(shared: State<'_, Shared>) -> View {
    shared.view.lock().expect("lock").clone()
}

/// What the user chooses in the window: viewing goes without saying.
#[derive(Debug, Clone, Copy, Deserialize)]
struct Choice {
    control: bool,
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
        restart: choice.restart,
        ..Permissions::VIEW_ONLY
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

#[tauri::command]
fn end_session(window: WebviewWindow, shared: State<'_, Shared>) {
    if window.label() == "bar" {
        shared.send(FromUi::EndSession);
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
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("view")
        .arg(id.get().to_string())
        .arg("--device-key")
        .arg(key)
        .arg("--report-to-ui")
        .stdin(if password.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
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
    if let (Some(password), Some(mut stdin)) = (&password, child.stdin.take()) {
        writeln!(stdin, "{}", password.as_str()).context("passar a senha ao visualizador")?;
    }
    let stdout = child.stdout.take().context("saída do visualizador")?;
    let stderr = child.stderr.take().context("erros do visualizador")?;

    let connected = app.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(name) = line.strip_prefix(VIEWER_CONNECTED) {
                remember(&connected, &id.to_string(), name.trim());
                changed(&connected);
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
        app.state::<Shared>().view.lock().expect("lock").viewer = state;
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
    #[cfg(windows)]
    let managed = matches!(link, Link::Service(_));
    #[cfg(not(windows))]
    let managed = false;
    let (to_host, outgoing) = mpsc::unbounded_channel();

    let app = tauri::Builder::default()
        // A second start brings this one forward (it would be a second host),
        // unless it is the service's interface finding the portable app here:
        // it tries again while the app stays open, with the service asking
        // through its message box meanwhile.
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            if !args.iter().any(|arg| arg == "ui") {
                open_home(app);
            }
        }))
        .manage(Shared {
            view: Mutex::new(View {
                recent: Recent::load().into_list(),
                ..View::default()
            }),
            to_host,
            managed,
        })
        .invoke_handler(tauri::generate_handler![
            view,
            answer,
            end_session,
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
                "main" if !managed && (wanted(app, "ask") || wanted(app, "bar")) => {
                    api.prevent_close();
                    let _ = window.minimize();
                }
                _ => {}
            }
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            if !managed {
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
        } if managed => api.prevent_exit(),
        // (Here and not on the window: only by now can the label be reused.)
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::Destroyed,
            ..
        } => reopen(app, &label),
        _ => {}
    });
    Ok(())
}
