//! Physical key → PC set-1 scancode (`0xE0xx` for extended keys).
//!
//! Scancodes name key *positions*, so the host's keyboard layout decides the
//! character: an ABNT2 viewer and a US host still agree on which key moved.

use winit::keyboard::KeyCode;

/// What the macOS Command key sends to the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKey {
    /// Command acts as Ctrl, so Cmd+C copies on a Windows host.
    Ctrl,
    /// Command acts as the Windows key.
    Win,
}

/// Scancode for `code`, or `None` for keys with no PC equivalent.
pub fn scancode(code: KeyCode, command: CommandKey) -> Option<u16> {
    use KeyCode::*;
    let sc = match code {
        Escape => 0x01,
        Digit1 => 0x02,
        Digit2 => 0x03,
        Digit3 => 0x04,
        Digit4 => 0x05,
        Digit5 => 0x06,
        Digit6 => 0x07,
        Digit7 => 0x08,
        Digit8 => 0x09,
        Digit9 => 0x0A,
        Digit0 => 0x0B,
        Minus => 0x0C,
        Equal => 0x0D,
        Backspace => 0x0E,
        Tab => 0x0F,
        KeyQ => 0x10,
        KeyW => 0x11,
        KeyE => 0x12,
        KeyR => 0x13,
        KeyT => 0x14,
        KeyY => 0x15,
        KeyU => 0x16,
        KeyI => 0x17,
        KeyO => 0x18,
        KeyP => 0x19,
        BracketLeft => 0x1A,
        BracketRight => 0x1B,
        Enter => 0x1C,
        ControlLeft => 0x1D,
        KeyA => 0x1E,
        KeyS => 0x1F,
        KeyD => 0x20,
        KeyF => 0x21,
        KeyG => 0x22,
        KeyH => 0x23,
        KeyJ => 0x24,
        KeyK => 0x25,
        KeyL => 0x26,
        Semicolon => 0x27,
        Quote => 0x28,
        Backquote => 0x29,
        ShiftLeft => 0x2A,
        Backslash => 0x2B,
        KeyZ => 0x2C,
        KeyX => 0x2D,
        KeyC => 0x2E,
        KeyV => 0x2F,
        KeyB => 0x30,
        KeyN => 0x31,
        KeyM => 0x32,
        Comma => 0x33,
        Period => 0x34,
        Slash => 0x35,
        ShiftRight => 0x36,
        NumpadMultiply => 0x37,
        AltLeft => 0x38,
        Space => 0x39,
        CapsLock => 0x3A,
        F1 => 0x3B,
        F2 => 0x3C,
        F3 => 0x3D,
        F4 => 0x3E,
        F5 => 0x3F,
        F6 => 0x40,
        F7 => 0x41,
        F8 => 0x42,
        F9 => 0x43,
        F10 => 0x44,
        ScrollLock => 0x46,
        Numpad7 => 0x47,
        Numpad8 => 0x48,
        Numpad9 => 0x49,
        NumpadSubtract => 0x4A,
        Numpad4 => 0x4B,
        Numpad5 => 0x4C,
        Numpad6 => 0x4D,
        NumpadAdd => 0x4E,
        Numpad1 => 0x4F,
        Numpad2 => 0x50,
        Numpad3 => 0x51,
        Numpad0 => 0x52,
        NumpadDecimal => 0x53,
        IntlBackslash => 0x56,
        F11 => 0x57,
        F12 => 0x58,
        NumpadEqual => 0x59,
        F13 => 0x64,
        F14 => 0x65,
        F15 => 0x66,
        F16 => 0x67,
        F17 => 0x68,
        F18 => 0x69,
        F19 => 0x6A,
        F20 => 0x6B,
        F21 => 0x6C,
        F22 => 0x6D,
        F23 => 0x6E,
        KanaMode => 0x70,
        // ABNT2 "/?" key, next to right Shift.
        IntlRo => 0x73,
        F24 => 0x76,
        Convert => 0x79,
        NonConvert => 0x7B,
        IntlYen => 0x7D,
        // ABNT2 numpad "." (the decimal key is ",").
        NumpadComma => 0x7E,
        MediaTrackPrevious => 0xE010,
        MediaTrackNext => 0xE019,
        NumpadEnter => 0xE01C,
        ControlRight => 0xE01D,
        AudioVolumeMute => 0xE020,
        MediaPlayPause => 0xE022,
        MediaStop => 0xE024,
        AudioVolumeDown => 0xE02E,
        AudioVolumeUp => 0xE030,
        NumpadDivide => 0xE035,
        PrintScreen => 0xE037,
        AltRight => 0xE038,
        NumLock => 0xE045,
        Home => 0xE047,
        ArrowUp => 0xE048,
        PageUp => 0xE049,
        ArrowLeft => 0xE04B,
        ArrowRight => 0xE04D,
        End => 0xE04F,
        ArrowDown => 0xE050,
        PageDown => 0xE051,
        Insert => 0xE052,
        Delete => 0xE053,
        SuperLeft if command == CommandKey::Ctrl && cfg!(target_os = "macos") => 0x1D,
        SuperRight if command == CommandKey::Ctrl && cfg!(target_os = "macos") => 0xE01D,
        SuperLeft => 0xE05B,
        SuperRight => 0xE05C,
        ContextMenu => 0xE05D,
        _ => return None,
    };
    Some(sc)
}

// Note: in `CommandKey::Ctrl` (a Mac viewer controlling a non-Mac host),
// `scancode()` maps both the Command and the physical Control keys to the
// Control scancodes. Four physical keys (two Control, two Command) cannot map
// to two Control scancodes without collision, so holding Control and Command
// at the same time is tracked as one key on the host. This is rare in practice
// (Cmd is remapped precisely so the user presses *it* for shortcuts); the
// `ReleaseAll` on focus loss clears any key left stuck by such a collision.

/// macOS virtual key code (`kVK_*`) for a PC set-1 scancode, for hosts
/// that inject with CGEvent. `None` for keys a Mac keyboard does not have.
pub fn mac_keycode(scancode: u16) -> Option<u16> {
    let code = match scancode {
        0x01 => 0x35,   // Escape
        0x02 => 0x12,   // 1
        0x03 => 0x13,   // 2
        0x04 => 0x14,   // 3
        0x05 => 0x15,   // 4
        0x06 => 0x17,   // 5
        0x07 => 0x16,   // 6
        0x08 => 0x1A,   // 7
        0x09 => 0x1C,   // 8
        0x0A => 0x19,   // 9
        0x0B => 0x1D,   // 0
        0x0C => 0x1B,   // -
        0x0D => 0x18,   // =
        0x0E => 0x33,   // Backspace (kVK_Delete)
        0x0F => 0x30,   // Tab
        0x10 => 0x0C,   // Q
        0x11 => 0x0D,   // W
        0x12 => 0x0E,   // E
        0x13 => 0x0F,   // R
        0x14 => 0x11,   // T
        0x15 => 0x10,   // Y
        0x16 => 0x20,   // U
        0x17 => 0x22,   // I
        0x18 => 0x1F,   // O
        0x19 => 0x23,   // P
        0x1A => 0x21,   // [
        0x1B => 0x1E,   // ]
        0x1C => 0x24,   // Return
        0x1D => 0x3B,   // Left Control
        0x1E => 0x00,   // A
        0x1F => 0x01,   // S
        0x20 => 0x02,   // D
        0x21 => 0x03,   // F
        0x22 => 0x05,   // G
        0x23 => 0x04,   // H
        0x24 => 0x26,   // J
        0x25 => 0x28,   // K
        0x26 => 0x25,   // L
        0x27 => 0x29,   // ;
        0x28 => 0x27,   // '
        0x29 => 0x32,   // `
        0x2A => 0x38,   // Left Shift
        0x2B => 0x2A,   // Backslash
        0x2C => 0x06,   // Z
        0x2D => 0x07,   // X
        0x2E => 0x08,   // C
        0x2F => 0x09,   // V
        0x30 => 0x0B,   // B
        0x31 => 0x2D,   // N
        0x32 => 0x2E,   // M
        0x33 => 0x2B,   // ,
        0x34 => 0x2F,   // .
        0x35 => 0x2C,   // /
        0x36 => 0x3C,   // Right Shift
        0x37 => 0x43,   // Keypad *
        0x38 => 0x3A,   // Left Alt (Option)
        0x39 => 0x31,   // Space
        0x3A => 0x39,   // Caps Lock
        0x3B => 0x7A,   // F1
        0x3C => 0x78,   // F2
        0x3D => 0x63,   // F3
        0x3E => 0x76,   // F4
        0x3F => 0x60,   // F5
        0x40 => 0x61,   // F6
        0x41 => 0x62,   // F7
        0x42 => 0x64,   // F8
        0x43 => 0x65,   // F9
        0x44 => 0x6D,   // F10
        0x47 => 0x59,   // Keypad 7
        0x48 => 0x5B,   // Keypad 8
        0x49 => 0x5C,   // Keypad 9
        0x4A => 0x4E,   // Keypad -
        0x4B => 0x56,   // Keypad 4
        0x4C => 0x57,   // Keypad 5
        0x4D => 0x58,   // Keypad 6
        0x4E => 0x45,   // Keypad +
        0x4F => 0x53,   // Keypad 1
        0x50 => 0x54,   // Keypad 2
        0x51 => 0x55,   // Keypad 3
        0x52 => 0x52,   // Keypad 0
        0x53 => 0x41,   // Keypad .
        0x56 => 0x0A,   // ISO key next to left Shift
        0x57 => 0x67,   // F11
        0x58 => 0x6F,   // F12
        0x59 => 0x51,   // Keypad =
        0x64 => 0x69,   // F13
        0x65 => 0x6B,   // F14
        0x66 => 0x71,   // F15
        0x67 => 0x6A,   // F16
        0x68 => 0x40,   // F17
        0x69 => 0x4F,   // F18
        0x6A => 0x50,   // F19
        0x6B => 0x5A,   // F20
        0x70 => 0x68,   // Kana
        0x73 => 0x5E,   // ABNT2 "/?" (JIS underscore position)
        0x7D => 0x5D,   // Yen
        0x7E => 0x5F,   // ABNT2 numpad "." (keypad comma)
        0xE01C => 0x4C, // Keypad Enter
        0xE01D => 0x3E, // Right Control
        0xE020 => 0x4A, // Mute
        0xE02E => 0x49, // Volume down
        0xE030 => 0x48, // Volume up
        0xE035 => 0x4B, // Keypad /
        0xE038 => 0x3D, // Right Alt (Option)
        0xE045 => 0x47, // Num Lock (Clear)
        0xE047 => 0x73, // Home
        0xE048 => 0x7E, // Up
        0xE049 => 0x74, // Page Up
        0xE04B => 0x7B, // Left
        0xE04D => 0x7C, // Right
        0xE04F => 0x77, // End
        0xE050 => 0x7D, // Down
        0xE051 => 0x79, // Page Down
        0xE052 => 0x72, // Insert (Help)
        0xE053 => 0x75, // Delete (forward)
        0xE05B => 0x37, // Left Windows -> Command
        0xE05C => 0x36, // Right Windows -> Right Command
        0xE05D => 0x6E, // Menu
        _ => return None,
    };
    Some(code)
}

/// Linux key code (`KEY_*` of `input-event-codes.h`, what evdev reports) for
/// a PC set-1 scancode, for hosts on Linux. The X11 key code is this plus 8;
/// the Wayland portal takes it as it is. The keyboard layout of the host
/// (ABNT2, US...) then decides the character, as on Windows.
pub fn evdev_keycode(scancode: u16) -> Option<u16> {
    let code = match scancode {
        // Set 1 and evdev agree from Escape to F10, the keypad included
        // (the set-1 table is where evdev's numbers came from).
        0x01..=0x44 => scancode,
        0x45 | 0xE045 => 69, // Num Lock
        0x46 => 70,          // Scroll Lock
        0x47..=0x53 => scancode,
        0x56 => 86,    // ISO key next to left Shift (KEY_102ND)
        0x57 => 87,    // F11
        0x58 => 88,    // F12
        0x59 => 117,   // Keypad = (KEY_KPEQUAL)
        0x64 => 183,   // F13
        0x65 => 184,   // F14
        0x66 => 185,   // F15
        0x67 => 186,   // F16
        0x68 => 187,   // F17
        0x69 => 188,   // F18
        0x6A => 189,   // F19
        0x6B => 190,   // F20
        0x6C => 191,   // F21
        0x6D => 192,   // F22
        0x6E => 193,   // F23
        0x70 => 93,    // Kana (KEY_KATAKANAHIRAGANA)
        0x73 => 89,    // ABNT2 "/?" (KEY_RO)
        0x76 => 194,   // F24
        0x79 => 92,    // Henkan
        0x7B => 94,    // Muhenkan
        0x7D => 124,   // Yen
        0x7E => 121,   // ABNT2 keypad "." (KEY_KPCOMMA)
        0xE010 => 165, // Previous track
        0xE019 => 163, // Next track
        0xE01C => 96,  // Keypad Enter
        0xE01D => 97,  // Right Control
        0xE020 => 113, // Mute
        0xE022 => 164, // Play/Pause
        0xE024 => 166, // Stop
        0xE02E => 114, // Volume down
        0xE030 => 115, // Volume up
        0xE035 => 98,  // Keypad /
        0xE037 => 99,  // Print Screen (KEY_SYSRQ)
        0xE038 => 100, // Right Alt (AltGr)
        0xE047 => 102, // Home
        0xE048 => 103, // Up
        0xE049 => 104, // Page Up
        0xE04B => 105, // Left
        0xE04D => 106, // Right
        0xE04F => 107, // End
        0xE050 => 108, // Down
        0xE051 => 109, // Page Down
        0xE052 => 110, // Insert
        0xE053 => 111, // Delete
        0xE05B => 125, // Left Windows (KEY_LEFTMETA)
        0xE05C => 126, // Right Windows
        0xE05D => 127, // Menu (KEY_COMPOSE)
        _ => return None,
    };
    Some(code)
}

/// Whether a Mac key (`kVK_*`) is one a real keyboard marks as on the numeric
/// pad (the keypad and the arrows): the one flag an injected key carries of
/// its own.
///
/// The "secondary Fn" flag macOS also puts on arrows, F-keys and Home/End is
/// left out on purpose: posted with injected keys it can stay behind as a
/// held Fn/Globe, and then typing "é" opens the emoji picker (Fn+E), "c" the
/// Control Center (Fn+C), and so on.
pub fn mac_numeric_pad(keycode: u16) -> bool {
    matches!(
        keycode,
        0x41 | 0x43
            | 0x45
            | 0x47
            | 0x4B
            | 0x4C
            | 0x4E
            | 0x51..=0x59
            | 0x5B
            | 0x5C
            | 0x5F
            | 0x7B..=0x7E
    )
}

/// For viewers on Windows/Linux controlling a Mac: Ctrl and the Windows key
/// trade places, so Ctrl+C sends Cmd+C (copy) and Win+... sends Control.
pub fn swap_ctrl_and_super(scancode: u16) -> u16 {
    match scancode {
        0x1D => 0xE05B,
        0xE01D => 0xE05C,
        0xE05B => 0x1D,
        0xE05C => 0xE01D,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_keys() {
        assert_eq!(scancode(KeyCode::KeyA, CommandKey::Win), Some(0x1E));
        assert_eq!(scancode(KeyCode::Enter, CommandKey::Win), Some(0x1C));
        assert_eq!(scancode(KeyCode::ArrowLeft, CommandKey::Win), Some(0xE04B));
        assert_eq!(scancode(KeyCode::IntlRo, CommandKey::Win), Some(0x73));
        assert_eq!(scancode(KeyCode::SuperLeft, CommandKey::Win), Some(0xE05B));
        assert_eq!(scancode(KeyCode::Fn, CommandKey::Win), None);
    }

    #[test]
    fn command_as_ctrl_only_on_macos() {
        let expected = if cfg!(target_os = "macos") {
            0x1D
        } else {
            0xE05B
        };
        assert_eq!(
            scancode(KeyCode::SuperLeft, CommandKey::Ctrl),
            Some(expected)
        );
    }

    #[test]
    fn no_duplicate_scancodes() {
        use std::collections::HashMap;
        let all = [
            KeyCode::Escape,
            KeyCode::Digit1,
            KeyCode::KeyQ,
            KeyCode::KeyA,
            KeyCode::KeyZ,
            KeyCode::Enter,
            KeyCode::NumpadEnter,
            KeyCode::ControlLeft,
            KeyCode::ControlRight,
            KeyCode::AltLeft,
            KeyCode::AltRight,
            KeyCode::Numpad7,
            KeyCode::Home,
            KeyCode::Numpad8,
            KeyCode::ArrowUp,
            KeyCode::Slash,
            KeyCode::NumpadDivide,
            KeyCode::IntlRo,
            KeyCode::NumpadComma,
            KeyCode::NumpadDecimal,
            KeyCode::Delete,
            KeyCode::Insert,
        ];
        let mut seen = HashMap::new();
        for key in all {
            let sc = scancode(key, CommandKey::Win).unwrap();
            if let Some(other) = seen.insert(sc, key) {
                panic!("{key:?} and {other:?} share scancode {sc:#06x}");
            }
        }
    }

    #[test]
    fn mac_keycodes() {
        // PC scancodes from the table above, through to the Mac codes.
        assert_eq!(mac_keycode(0x1E), Some(0x00)); // A
        assert_eq!(mac_keycode(0x1C), Some(0x24)); // Return
        assert_eq!(mac_keycode(0xE04B), Some(0x7B)); // Left
        assert_eq!(mac_keycode(0xE05B), Some(0x37)); // Win -> Command
        assert_eq!(mac_keycode(0x46), None); // Scroll Lock
        // Every key the viewer can send maps to a distinct Mac key.
        let mut seen = std::collections::HashMap::new();
        for code in 0..=0xE0FFu16 {
            if let Some(mac) = mac_keycode(code)
                && let Some(other) = seen.insert(mac, code)
            {
                panic!("{code:#06x} and {other:#06x} both map to {mac:#04x}");
            }
        }
    }

    #[test]
    fn linux_keycodes() {
        assert_eq!(evdev_keycode(0x1E), Some(30)); // A (KEY_A)
        assert_eq!(evdev_keycode(0x1C), Some(28)); // Enter
        assert_eq!(evdev_keycode(0x39), Some(57)); // Space
        assert_eq!(evdev_keycode(0x53), Some(83)); // Keypad , on ABNT2 (KEY_KPDOT)
        assert_eq!(evdev_keycode(0xE04B), Some(105)); // Left
        assert_eq!(evdev_keycode(0xE038), Some(100)); // AltGr
        assert_eq!(evdev_keycode(0xE05B), Some(125)); // Win -> Super
        assert_eq!(evdev_keycode(0x54), None);
        // The ABNT2 keys: "ç" and the accents sit where the US keyboard has
        // ";" "'" "[" and "]", and two keys the US one lacks.
        assert_eq!(evdev_keycode(0x27), Some(39)); // ç (KEY_SEMICOLON)
        assert_eq!(evdev_keycode(0x28), Some(40)); // ~ ^ (KEY_APOSTROPHE)
        assert_eq!(evdev_keycode(0x1A), Some(26)); // ´ ` (KEY_LEFTBRACE)
        assert_eq!(evdev_keycode(0x1B), Some(27)); // [ { (KEY_RIGHTBRACE)
        assert_eq!(evdev_keycode(0x2B), Some(43)); // ] } (KEY_BACKSLASH)
        assert_eq!(evdev_keycode(0x56), Some(86)); // \ | (KEY_102ND)
        assert_eq!(evdev_keycode(0x73), Some(89)); // / ? (KEY_RO)
        assert_eq!(evdev_keycode(0x7E), Some(121)); // keypad . (KEY_KPCOMMA)
        // Every key the viewer can send reaches a Linux key, each its own.
        let mut seen = std::collections::HashMap::new();
        for code in all_viewer_keys() {
            let sc = scancode(code, CommandKey::Win).unwrap();
            let linux = evdev_keycode(sc).unwrap_or_else(|| panic!("{code:?} ({sc:#06x})"));
            assert!(linux + 8 <= 255, "{code:?}: X11 key codes stop at 255");
            if let Some(other) = seen.insert(linux, sc) {
                panic!("{sc:#06x} and {other:#06x} both map to {linux}");
            }
        }
    }

    /// The keys the viewer sends (every one `scancode` maps).
    fn all_viewer_keys() -> Vec<KeyCode> {
        use KeyCode::*;
        [
            Escape,
            Digit1,
            Digit2,
            Digit3,
            Digit4,
            Digit5,
            Digit6,
            Digit7,
            Digit8,
            Digit9,
            Digit0,
            Minus,
            Equal,
            Backspace,
            Tab,
            KeyQ,
            KeyW,
            KeyE,
            KeyR,
            KeyT,
            KeyY,
            KeyU,
            KeyI,
            KeyO,
            KeyP,
            BracketLeft,
            BracketRight,
            Enter,
            ControlLeft,
            KeyA,
            KeyS,
            KeyD,
            KeyF,
            KeyG,
            KeyH,
            KeyJ,
            KeyK,
            KeyL,
            Semicolon,
            Quote,
            Backquote,
            ShiftLeft,
            Backslash,
            KeyZ,
            KeyX,
            KeyC,
            KeyV,
            KeyB,
            KeyN,
            KeyM,
            Comma,
            Period,
            Slash,
            ShiftRight,
            NumpadMultiply,
            AltLeft,
            Space,
            CapsLock,
            F1,
            F2,
            F3,
            F4,
            F5,
            F6,
            F7,
            F8,
            F9,
            F10,
            ScrollLock,
            Numpad7,
            Numpad8,
            Numpad9,
            NumpadSubtract,
            Numpad4,
            Numpad5,
            Numpad6,
            NumpadAdd,
            Numpad1,
            Numpad2,
            Numpad3,
            Numpad0,
            NumpadDecimal,
            IntlBackslash,
            F11,
            F12,
            NumpadEqual,
            F13,
            F14,
            F15,
            F16,
            F17,
            F18,
            F19,
            F20,
            F21,
            F22,
            F23,
            KanaMode,
            IntlRo,
            F24,
            Convert,
            NonConvert,
            IntlYen,
            NumpadComma,
            MediaTrackPrevious,
            MediaTrackNext,
            NumpadEnter,
            ControlRight,
            AudioVolumeMute,
            MediaPlayPause,
            MediaStop,
            AudioVolumeDown,
            AudioVolumeUp,
            NumpadDivide,
            PrintScreen,
            AltRight,
            NumLock,
            Home,
            ArrowUp,
            PageUp,
            ArrowLeft,
            ArrowRight,
            End,
            ArrowDown,
            PageDown,
            Insert,
            Delete,
            SuperLeft,
            SuperRight,
            ContextMenu,
        ]
        .into()
    }

    #[test]
    fn numeric_pad_keys() {
        // Keypad and arrows carry the flag; letters, F-keys and Home do not.
        let pad = [
            0x47, 0x52, 0x53, 0x37, 0x4A, 0x4E, 0x59, 0x7E, 0xE01C, 0xE035, 0xE045, 0xE048, 0xE04B,
            0xE04D, 0xE050,
        ];
        for sc in pad {
            assert!(mac_numeric_pad(mac_keycode(sc).unwrap()), "{sc:#06x}");
        }
        for sc in [0x12, 0x1A, 0x1C, 0x3B, 0x57, 0xE047, 0xE053, 0xE05B, 0x2A] {
            assert!(!mac_numeric_pad(mac_keycode(sc).unwrap()), "{sc:#06x}");
        }
    }

    #[test]
    fn ctrl_and_super_swap() {
        assert_eq!(swap_ctrl_and_super(0x1D), 0xE05B);
        assert_eq!(swap_ctrl_and_super(swap_ctrl_and_super(0xE01D)), 0xE01D);
        assert_eq!(swap_ctrl_and_super(0x1E), 0x1E);
    }
}
