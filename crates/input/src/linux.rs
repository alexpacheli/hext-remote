//! Input injection on X11 with the XTest extension (F7, milestone 4).
//!
//! Keys go by key code (the scancode's evdev code plus 8), so the X server's
//! keyboard layout (ABNT2, US...) decides the character, as on Windows.
//! Positions come relative to the main monitor; X11 counts from the corner
//! of all of them, so the main one's place ([`hext_capture::desktop_origin`])
//! is added back.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use hext_proto::{InputEvent, MouseButton};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
    MOTION_NOTIFY_EVENT, Window,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use crate::keymap::evdev_keycode;
use crate::{Area, Injector};

/// How long the main monitor's place is trusted before asking again (a
/// monitor plugged in or rearranged moves it).
const ORIGIN_FRESH: Duration = Duration::from_secs(3);

/// The X11 key code for a PC set-1 scancode.
pub(crate) fn x11_keycode(scancode: u16) -> Option<u8> {
    evdev_keycode(scancode).and_then(|code| u8::try_from(code + 8).ok())
}

/// The X11 button for a mouse button.
pub(crate) fn x11_button(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Middle => 2,
        MouseButton::Right => 3,
        MouseButton::Back => 8,
        MouseButton::Forward => 9,
    }
}

/// Whole notches out of `rest` plus `delta`, the fraction kept for later.
pub(crate) fn notches(rest: &mut f32, delta: f32) -> i32 {
    *rest += delta;
    let whole = rest.trunc();
    *rest -= whole;
    whole as i32
}

struct X11 {
    conn: RustConnection,
    root: Window,
}

impl X11 {
    fn connect() -> Option<Self> {
        let (conn, screen) = RustConnection::connect(None)
            .inspect_err(|e| tracing::warn!("mouse e teclado: sem conexão com o X11: {e}"))
            .ok()?;
        let version = conn
            .xtest_get_version(2, 2)
            .ok()
            .and_then(|c| c.reply().ok());
        if version.is_none() {
            tracing::warn!("mouse e teclado: o X11 não tem a extensão XTest");
            return None;
        }
        let root = conn.setup().roots[screen].root;
        Some(Self { conn, root })
    }

    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) -> bool {
        let sent = self
            .conn
            .xtest_fake_input(kind, detail, 0, self.root, x, y, 0)
            .is_ok()
            && self.conn.flush().is_ok();
        if !sent {
            tracing::debug!("XTest: evento não enviado");
        }
        sent
    }
}

/// Replays input with XTest. Connects on first use, and again after the
/// connection breaks.
#[derive(Default)]
pub struct XTestInjector {
    x11: Option<X11>,
    keys_down: HashSet<u8>,
    buttons_down: HashSet<u8>,
    /// Sub-notch scroll left over from trackpads.
    wheel_rest: (f32, f32),
    origin: Option<((i32, i32), Instant)>,
}

impl XTestInjector {
    fn x11(&mut self) -> Option<&X11> {
        if self.x11.is_none() {
            self.x11 = X11::connect();
        }
        self.x11.as_ref()
    }

    fn origin(&mut self) -> (i32, i32) {
        match self.origin {
            Some((origin, at)) if at.elapsed() < ORIGIN_FRESH => origin,
            _ => {
                let origin = hext_capture::desktop_origin();
                self.origin = Some((origin, Instant::now()));
                origin
            }
        }
    }

    fn send(&mut self, kind: u8, detail: u8, x: i16, y: i16) {
        let Some(x11) = self.x11() else {
            return;
        };
        if !x11.fake(kind, detail, x, y) {
            // Broken connection: a new one next time.
            self.x11 = None;
        }
    }

    fn button(&mut self, button: u8, pressed: bool) {
        let kind = if pressed {
            BUTTON_PRESS_EVENT
        } else {
            BUTTON_RELEASE_EVENT
        };
        self.send(kind, button, 0, 0);
    }

    /// One click of the wheel's `button` (4 up, 5 down, 6 left, 7 right).
    fn click(&mut self, button: u8, times: i32) {
        for _ in 0..times.unsigned_abs().min(50) {
            self.button(button, true);
            self.button(button, false);
        }
    }
}

impl Injector for XTestInjector {
    fn inject(&mut self, event: &InputEvent, area: Area) {
        match *event {
            InputEvent::MouseMove { x, y } => {
                let (px, py) = area.to_pixel(x, y);
                let (ox, oy) = self.origin();
                let clamp = |v: i32| v.clamp(i16::MIN.into(), i16::MAX.into()) as i16;
                self.send(MOTION_NOTIFY_EVENT, 0, clamp(px + ox), clamp(py + oy));
            }
            InputEvent::MouseButton { button, pressed } => {
                let button = x11_button(button);
                if pressed {
                    self.buttons_down.insert(button);
                } else {
                    self.buttons_down.remove(&button);
                }
                self.button(button, pressed);
            }
            InputEvent::Wheel { dx, dy } => {
                if !dx.is_finite() || !dy.is_finite() {
                    return;
                }
                let v = notches(&mut self.wheel_rest.1, dy);
                let h = notches(&mut self.wheel_rest.0, dx);
                // Positive dy scrolls up (button 4); positive dx right (7).
                self.click(if v > 0 { 4 } else { 5 }, v);
                self.click(if h > 0 { 7 } else { 6 }, h);
            }
            InputEvent::Key { scancode, pressed } => {
                let Some(code) = x11_keycode(scancode) else {
                    tracing::debug!("tecla sem equivalente no X11: {scancode:#06x}");
                    return;
                };
                let kind = if pressed {
                    self.keys_down.insert(code);
                    KEY_PRESS_EVENT
                } else {
                    self.keys_down.remove(&code);
                    KEY_RELEASE_EVENT
                };
                self.send(kind, code, 0, 0);
            }
            InputEvent::ReleaseAll => self.release_all(),
            // Only the Windows service sends it.
            InputEvent::SecureAttention => {}
        }
    }

    fn release_all(&mut self) {
        for code in std::mem::take(&mut self.keys_down) {
            self.send(KEY_RELEASE_EVENT, code, 0, 0);
        }
        for button in std::mem::take(&mut self.buttons_down) {
            self.button(button, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x11_codes() {
        assert_eq!(x11_keycode(0x1E), Some(38)); // A
        assert_eq!(x11_keycode(0x01), Some(9)); // Escape
        assert_eq!(x11_keycode(0xE05B), Some(133)); // Super
        assert_eq!(x11_keycode(0x73), Some(97)); // ABNT2 "/?" (<AB11>)
        assert_eq!(x11_keycode(0x7E), Some(129)); // ABNT2 keypad "." (<KPPT>)
        assert_eq!(x11_keycode(0x54), None);
        assert_eq!(x11_button(MouseButton::Right), 3);
        assert_eq!(x11_button(MouseButton::Back), 8);
    }

    #[test]
    fn trackpad_scroll_adds_up_to_notches() {
        let mut rest = 0.0;
        assert_eq!(notches(&mut rest, 0.4), 0);
        assert_eq!(notches(&mut rest, 0.4), 0);
        assert_eq!(notches(&mut rest, 0.4), 1);
        assert_eq!(notches(&mut rest, -2.5), -2);
        assert!(rest.abs() < 0.31, "{rest}");
    }

    /// Keysyms of `code` in the X server's keymap.
    fn keysyms(conn: &RustConnection, code: u8) -> Vec<u32> {
        use x11rb::protocol::xproto::ConnectionExt as _;
        let reply = conn.get_keyboard_mapping(code, 1).unwrap().reply().unwrap();
        reply.keysyms
    }

    /// On a real X server (Xvfb in CI): XTest moves the pointer to the place
    /// asked and presses and releases keys, by the key codes of the X
    /// server's keymap.
    #[test]
    #[ignore = "needs an X server: xvfb-run cargo test -- --ignored x11_"]
    fn x11_injects_pointer_and_keys() {
        use x11rb::protocol::xproto::ConnectionExt as _;
        let (conn, screen) = RustConnection::connect(None).unwrap();
        let root = conn.setup().roots[screen].root;
        let mut injector = XTestInjector::default();
        let area = Area {
            x: 0,
            y: 0,
            width: 1280,
            height: 800,
        };
        injector.inject(&InputEvent::MouseMove { x: 0.5, y: 0.25 }, area);
        std::thread::sleep(Duration::from_millis(100));
        let pointer = conn.query_pointer(root).unwrap().reply().unwrap();
        let (ox, oy) = hext_capture::desktop_origin();
        let (px, py) = area.to_pixel(0.5, 0.25);
        assert_eq!(
            (i32::from(pointer.root_x), i32::from(pointer.root_y)),
            (px + ox, py + oy)
        );

        let down = |code: u8| {
            let keys = conn.query_keymap().unwrap().reply().unwrap().keys;
            keys[usize::from(code / 8)] & (1 << (code % 8)) != 0
        };
        let a = x11_keycode(0x1E).unwrap();
        injector.inject(
            &InputEvent::Key {
                scancode: 0x1E,
                pressed: true,
            },
            area,
        );
        std::thread::sleep(Duration::from_millis(50));
        assert!(down(a), "A não ficou pressionada");
        injector.release_all();
        std::thread::sleep(Duration::from_millis(50));
        assert!(!down(a), "A não foi solta");

        // The evdev keymap (every X server today) puts "a" on the A key.
        assert!(
            keysyms(&conn, a).contains(&0x61),
            "{:x?}",
            keysyms(&conn, a)
        );
    }

    /// The first symbol of each key of a keymap compiled by `xkbcomp -xkb`,
    /// by X11 key code.
    fn first_symbols(xkb: &str) -> std::collections::HashMap<u8, String> {
        let mut codes = std::collections::HashMap::new();
        let mut aliases = Vec::new();
        for line in xkb.lines().map(str::trim) {
            // `<AC10> = 47;` and `alias <KPPT> = <I129>;`
            if let Some(rest) = line.strip_prefix("alias ") {
                let mut parts = rest.trim_end_matches(';').split('=').map(str::trim);
                if let (Some(alias), Some(name)) = (parts.next(), parts.next()) {
                    aliases.push((alias.to_owned(), name.to_owned()));
                }
            } else if line.starts_with('<')
                && let Some((name, code)) = line.trim_end_matches(';').split_once('=')
                && let Ok(code) = code.trim().parse::<u8>()
            {
                codes.insert(name.trim().to_owned(), code);
            }
        }
        for (alias, name) in aliases {
            if let Some(&code) = codes.get(&name) {
                codes.insert(alias, code);
            }
        }
        let mut symbols = std::collections::HashMap::new();
        let mut rest = xkb;
        while let Some(at) = rest.find("key <") {
            rest = &rest[at + 4..];
            let name = &rest[..=rest.find('>').unwrap_or(0)];
            let block = &rest[..rest.find("};").unwrap_or(rest.len())];
            // `{ [ a, A ] }`, or `symbols[Group1]= [ a, A ]`.
            let block = block.find("]=").map_or(block, |at| &block[at + 2..]);
            if let (Some(&code), Some(open)) = (codes.get(name), block.find('[')) {
                let list = &block[open + 1..];
                let first = list.split([',', ']']).next().unwrap_or("").trim();
                symbols.insert(code, first.to_owned());
            }
        }
        symbols
    }

    /// The ABNT2 keys land where the Brazilian layout (`br(abnt2)` of the
    /// system's xkb data) puts "ç", the accents, "\" and "/", and the keypad
    /// "." key. Reads a keymap compiled by `xkbcomp -xkb` (CI makes it), as
    /// the X server of a Brazilian desktop would.
    #[test]
    #[ignore = "needs a compiled keymap: HEXT_TEST_XKB_KEYMAP=br.xkb cargo test -- --ignored x11_"]
    fn x11_abnt2_keys_in_the_brazilian_layout() {
        let path = std::env::var("HEXT_TEST_XKB_KEYMAP").expect("HEXT_TEST_XKB_KEYMAP");
        let symbols = first_symbols(&std::fs::read_to_string(path).unwrap());
        let at = |scancode: u16| {
            let code = x11_keycode(scancode).unwrap();
            symbols.get(&code).cloned().unwrap_or_default()
        };
        assert_eq!(at(0x1E), "a");
        assert_eq!(at(0x27), "ccedilla");
        assert_eq!(at(0x1A), "dead_acute");
        assert_eq!(at(0x28), "dead_tilde");
        assert_eq!(at(0x1B), "bracketleft");
        assert_eq!(at(0x2B), "bracketright");
        assert_eq!(at(0x56), "backslash");
        assert_eq!(at(0x73), "slash");
        assert_eq!(at(0x7E), "KP_Decimal");
        assert_eq!(at(0x53), "KP_Delete");
        assert_eq!(at(0xE038), "ISO_Level3_Shift");
    }
}
