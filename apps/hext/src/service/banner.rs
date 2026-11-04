//! The session's indicator where the interface cannot be seen: the sign-in
//! screen is a desktop of its own, closed to the user's processes, and on a
//! locked computer the user's windows are behind the lock. The agent runs as
//! SYSTEM, so it can put a window there: a plain Win32 one, with nothing to
//! load and nothing to attack but a click.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::{info, warn};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_SINGLELINE, DT_VCENTER,
    DeleteObject, DrawTextW, EndPaint, FillRect, HBRUSH, PAINTSTRUCT, SetBkMode, SetTextColor,
    TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
    GetSystemMetrics, HWND_TOPMOST, MSG, PM_REMOVE, PeekMessageW, RegisterClassW, SM_CXSCREEN,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetWindowPos, TranslateMessage,
    WINDOW_EX_STYLE, WM_DESTROY, WM_LBUTTONUP, WM_PAINT, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};
use windows::core::{PCWSTR, w};

use super::windows::console_locked;

const WIDTH: i32 = 520;
const HEIGHT: i32 = 44;
/// Where Encerrar is, from the right edge.
const BUTTON_WIDTH: i32 = 96;
/// How often to look for a change of desktop. A window that fell behind
/// comes back on top with it.
const CHECK_EVERY: Duration = Duration::from_millis(500);
const DARK: COLORREF = COLORREF(0x0026_1E1E);
const RED: COLORREF = COLORREF(0x0026_26DC);
const WHITE: COLORREF = COLORREF(0x00FF_FFFF);

/// What a click needs, and what the window shows: the window procedure gets
/// nothing else.
static TEXT: Mutex<Vec<u16>> = Mutex::new(Vec::new());
static ENDED: AtomicBool = AtomicBool::new(false);

/// The indicator, on screen until dropped.
pub struct Banner {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Banner {
    /// Shows `text` while the session lasts, `everywhere` or only where the
    /// interface cannot be seen. `on_end` runs if someone there presses
    /// Encerrar.
    pub fn show(text: &str, everywhere: bool, on_end: impl Fn() + Send + 'static) -> Option<Self> {
        *TEXT.lock().expect("lock") = text.encode_utf16().collect();
        ENDED.store(false, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        let running = stop.clone();
        let thread = std::thread::Builder::new()
            .name("hext-banner".into())
            .spawn(move || run(&running, everywhere, on_end));
        match thread {
            Ok(thread) => Some(Self {
                stop,
                thread: Some(thread),
            }),
            Err(e) => {
                warn!("indicador da tela de login: {e}");
                None
            }
        }
    }
}

impl Drop for Banner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Follows the input desktop: a window wherever the interface's bar cannot
/// be seen.
fn run(stop: &AtomicBool, everywhere: bool, on_end: impl Fn()) {
    #[allow(unsafe_code)] // one Win32 class, for this process
    let class = unsafe { register_class() };
    let mut window: Option<Window> = None;
    let mut place: (Option<String>, bool) = (None, false);
    while !stop.load(Ordering::Relaxed) {
        let desktop = hext_capture::input_desktop_name();
        let needed = desktop.as_deref().is_some_and(|desktop| {
            everywhere || is_closed_to_the_user(desktop) || console_locked()
        });
        let now = (desktop, needed);
        if now != place {
            info!("indicador: desktop de entrada {now:?}");
            // A thread with a window cannot change desktops.
            window = None;
            place = now;
            if needed {
                hext_capture::follow_input_desktop();
                window = class.and_then(Window::new);
            }
        }
        if let Some(window) = &window {
            window.keep_on_top();
        }
        pump(CHECK_EVERY);
        if ENDED.swap(false, Ordering::Relaxed) {
            on_end();
        }
    }
}

/// Whether `desktop` is closed to the interface.
fn is_closed_to_the_user(desktop: &str) -> bool {
    !desktop.eq_ignore_ascii_case("default")
}

/// Handles the window's messages for a while.
#[allow(unsafe_code)] // Win32 message loop
fn pump(time: Duration) {
    const SLICE: Duration = Duration::from_millis(50);
    let mut left = time;
    while !left.is_zero() {
        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(SLICE);
        left = left.saturating_sub(SLICE);
    }
}

#[allow(unsafe_code)] // Win32 calls
unsafe fn register_class() -> Option<PCWSTR> {
    let name = w!("HextRemoteBanner");
    let instance = unsafe { GetModuleHandleW(None) }.ok()?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance.into(),
        lpszClassName: name,
        ..Default::default()
    };
    // (Zero the second time around: already there.)
    unsafe { RegisterClassW(&class) };
    Some(name)
}

/// The window, destroyed when dropped.
struct Window(HWND);

#[allow(unsafe_code)] // Win32 calls
impl Window {
    fn new(class: PCWSTR) -> Option<Self> {
        let instance = unsafe { GetModuleHandleW(None) }.ok()?;
        let x = (unsafe { GetSystemMetrics(SM_CXSCREEN) } - WIDTH) / 2;
        let style: WINDOW_EX_STYLE = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let window = unsafe {
            CreateWindowExW(
                style,
                class,
                w!("Hext Remote: sessão ativa"),
                WS_POPUP | WS_VISIBLE,
                x,
                8,
                WIDTH,
                HEIGHT,
                None,
                None,
                Some(instance.into()),
                None,
            )
        };
        match window {
            Ok(window) => Some(Self(window)),
            Err(e) => {
                warn!("indicador da tela de login: {e}");
                None
            }
        }
    }

    fn keep_on_top(&self) {
        let _ = unsafe {
            SetWindowPos(
                self.0,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
        };
    }
}

#[allow(unsafe_code)] // Win32 call
impl Drop for Window {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

#[allow(unsafe_code)] // Win32 window procedure
unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_PAINT => {
            unsafe { paint(window) };
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            if x >= WIDTH - BUTTON_WIDTH {
                ENDED.store(true, Ordering::Relaxed);
            }
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

#[allow(unsafe_code)] // Win32 drawing
unsafe fn paint(window: HWND) {
    let mut paint = PAINTSTRUCT::default();
    let mut area = RECT::default();
    unsafe {
        let dc = BeginPaint(window, &mut paint);
        let _ = GetClientRect(window, &mut area);
        let fill = |rect: &RECT, color: COLORREF| {
            let brush: HBRUSH = CreateSolidBrush(color);
            FillRect(dc, rect, brush);
            let _ = DeleteObject(brush.into());
        };
        fill(&area, DARK);
        let button = RECT {
            left: area.right - BUTTON_WIDTH + 6,
            top: 6,
            right: area.right - 6,
            bottom: area.bottom - 6,
        };
        fill(&button, RED);
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, WHITE);
        let mut end: Vec<u16> = "Encerrar".encode_utf16().collect();
        let mut rect = button;
        DrawTextW(
            dc,
            &mut end,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        let mut text = TEXT.lock().expect("lock").clone();
        let mut rect = RECT {
            left: 14,
            right: area.right - BUTTON_WIDTH,
            ..area
        };
        DrawTextW(
            dc,
            &mut text,
            &mut rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );
        let _ = EndPaint(window, &paint);
    }
}
