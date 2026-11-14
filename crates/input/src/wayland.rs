//! Input on Wayland (F7, milestone 5): through the RemoteDesktop portal
//! session the capture opened ([`hext_capture::send_portal_input`]), the
//! only way a Wayland desktop takes input from another program.
//!
//! Keys go as evdev codes, so the desktop's own layout (ABNT2, US...)
//! decides the character, as with X11; the pointer goes to a place of the
//! displays, which the capture turns into the shared screen under it.

use std::collections::HashSet;

use hext_capture::{PortalInput, send_portal_input};
use hext_proto::{InputEvent, MouseButton};

use crate::keymap::evdev_keycode;
use crate::linux::notches;
use crate::{Area, Injector};

/// The evdev code of a mouse button (`BTN_LEFT`...).
pub(crate) fn evdev_button(button: MouseButton) -> u16 {
    match button {
        MouseButton::Left => 0x110,
        MouseButton::Right => 0x111,
        MouseButton::Middle => 0x112,
        MouseButton::Back => 0x113,
        MouseButton::Forward => 0x114,
    }
}

/// Replays input through the portal.
#[derive(Default)]
pub struct PortalInjector {
    keys_down: HashSet<u16>,
    buttons_down: HashSet<u16>,
    /// Sub-notch scroll left over from trackpads.
    wheel_rest: (f32, f32),
}

impl PortalInjector {
    /// What `event` becomes for the portal.
    pub(crate) fn translate(&mut self, event: &InputEvent, area: Area) -> Vec<PortalInput> {
        match *event {
            InputEvent::MouseMove { x, y } => {
                let (x, y) = area.to_pixel(x, y);
                vec![PortalInput::MoveTo { x, y }]
            }
            InputEvent::MouseButton { button, pressed } => {
                let code = evdev_button(button);
                if pressed {
                    self.buttons_down.insert(code);
                } else {
                    self.buttons_down.remove(&code);
                }
                vec![PortalInput::Button { code, pressed }]
            }
            InputEvent::Wheel { dx, dy } => {
                if !dx.is_finite() || !dy.is_finite() {
                    return Vec::new();
                }
                let dy = notches(&mut self.wheel_rest.1, dy).clamp(-50, 50);
                let dx = notches(&mut self.wheel_rest.0, dx).clamp(-50, 50);
                if (dx, dy) == (0, 0) {
                    return Vec::new();
                }
                vec![PortalInput::Scroll { dx, dy }]
            }
            InputEvent::Key { scancode, pressed } => {
                let Some(code) = evdev_keycode(scancode) else {
                    tracing::debug!("tecla sem equivalente no Linux: {scancode:#06x}");
                    return Vec::new();
                };
                if pressed {
                    self.keys_down.insert(code);
                } else {
                    self.keys_down.remove(&code);
                }
                vec![PortalInput::Key { code, pressed }]
            }
            InputEvent::ReleaseAll => self.releases(),
            // Only the Windows service sends it.
            InputEvent::SecureAttention => Vec::new(),
        }
    }

    fn releases(&mut self) -> Vec<PortalInput> {
        let keys = std::mem::take(&mut self.keys_down)
            .into_iter()
            .map(|code| PortalInput::Key {
                code,
                pressed: false,
            });
        let buttons = std::mem::take(&mut self.buttons_down)
            .into_iter()
            .map(|code| PortalInput::Button {
                code,
                pressed: false,
            });
        keys.chain(buttons).collect()
    }
}

impl Injector for PortalInjector {
    fn inject(&mut self, event: &InputEvent, area: Area) {
        for input in self.translate(event, area) {
            send_portal_input(input);
        }
    }

    fn release_all(&mut self) {
        for input in self.releases() {
            send_portal_input(input);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Area = Area {
        x: 1920,
        y: 0,
        width: 1280,
        height: 1024,
    };

    #[test]
    fn events_become_portal_input() {
        let mut injector = PortalInjector::default();
        assert_eq!(
            injector.translate(&InputEvent::MouseMove { x: 0.0, y: 1.0 }, AREA),
            [PortalInput::MoveTo { x: 1920, y: 1023 }]
        );
        // Keys by evdev code: the ABNT2 "/?" key is KEY_RO.
        assert_eq!(
            injector.translate(
                &InputEvent::Key {
                    scancode: 0x73,
                    pressed: true
                },
                AREA
            ),
            [PortalInput::Key {
                code: 89,
                pressed: true
            }]
        );
        assert_eq!(
            injector.translate(
                &InputEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed: true
                },
                AREA
            ),
            [PortalInput::Button {
                code: 0x110,
                pressed: true
            }]
        );
        assert_eq!(
            injector.translate(&InputEvent::Wheel { dx: 0.0, dy: 2.0 }, AREA),
            [PortalInput::Scroll { dx: 0, dy: 2 }]
        );
        assert!(
            injector
                .translate(&InputEvent::Wheel { dx: 0.3, dy: 0.0 }, AREA)
                .is_empty()
        );
        // What is held is let go.
        let mut released = injector.translate(&InputEvent::ReleaseAll, AREA);
        released.sort_by_key(|i| format!("{i:?}"));
        assert_eq!(
            released,
            [
                PortalInput::Button {
                    code: 0x110,
                    pressed: false
                },
                PortalInput::Key {
                    code: 89,
                    pressed: false
                },
            ]
        );
        assert!(injector.translate(&InputEvent::ReleaseAll, AREA).is_empty());
    }

    #[test]
    fn mouse_buttons_by_evdev_code() {
        assert_eq!(evdev_button(MouseButton::Right), 0x111);
        assert_eq!(evdev_button(MouseButton::Middle), 0x112);
        assert_eq!(evdev_button(MouseButton::Forward), 0x114);
    }
}
