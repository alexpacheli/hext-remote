//! This computer's clipboard during a session: text only, both ways.
//!
//! What was copied before the session stays out of it: only a change is
//! told, or the content when asked ([`Clipboard::tell`]). Windows has a
//! counter for changes; elsewhere the text is compared. What password
//! managers mark as private never leaves.

use std::io;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use arboard::{Clipboard as System, Error};
use hext_proto::MAX_CLIPBOARD_TEXT;
use tracing::warn;

/// How long a change takes to be noticed, at most.
const POLL: Duration = Duration::from_millis(300);

/// What the watching thread is asked to do.
enum Ask {
    /// Replace the content.
    Set(String),
    /// Tell the content now, news or not.
    Tell,
}

/// The clipboard being watched, until dropped.
pub struct Clipboard {
    asks: mpsc::Sender<Ask>,
}

impl Clipboard {
    /// Watches the clipboard on a thread of its own, calling `changed` with
    /// each new text. What [`Self::set`] puts there does not come back.
    pub fn watch(changed: impl FnMut(String) + Send + 'static) -> io::Result<Self> {
        let (asks, asked) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("hext-clipboard".into())
            .spawn(move || match System::new() {
                Ok(system) => {
                    let _ = ready_tx.send(Ok(()));
                    watch(system, &asked, changed);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(io::Error::other(e)));
                }
            })?;
        ready_rx
            .recv()
            .map_err(|_| io::Error::other("clipboard thread died"))??;
        Ok(Self { asks })
    }

    /// Replaces the clipboard's content.
    pub fn set(&self, text: String) {
        let _ = self.asks.send(Ask::Set(text));
    }

    /// Calls `changed` with what the clipboard holds now (unless it is
    /// private, or empty), even if it was there before: copied elsewhere
    /// while the session was not in use, to paste into it.
    pub fn tell(&self) {
        let _ = self.asks.send(Ask::Tell);
    }
}

fn watch(mut system: System, asked: &mpsc::Receiver<Ask>, mut changed: impl FnMut(String)) {
    let mut version = os::version();
    let mut known = Known::default();
    known.saw(read(&mut system).ok().flatten());
    loop {
        match asked.recv_timeout(POLL) {
            Ok(Ask::Set(text)) => match system.set_text(text.as_str()) {
                Ok(()) => known.set(text),
                Err(e) => warn!("área de transferência: {e}"),
            },
            Ok(Ask::Tell) => {
                if os::private() != Some(false) {
                    continue;
                }
                if let Ok(text) = read(&mut system) {
                    version = os::version();
                    if let Some(text) = known.told(text) {
                        changed(text);
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                let now = os::version();
                if now.is_some() && now == version {
                    continue;
                }
                match os::private() {
                    // Busy: the next round tries again.
                    None => continue,
                    Some(true) => {
                        version = now;
                        continue;
                    }
                    Some(false) => {}
                }
                // Still in another program's hands: the next round tries again.
                let Ok(text) = read(&mut system) else {
                    continue;
                };
                version = now;
                if let Some(text) = known.saw(text) {
                    changed(text);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// The text on the clipboard, if it has any. An error: try again.
fn read(system: &mut System) -> Result<Option<String>, Error> {
    match system.get_text() {
        Ok(text) => Ok(Some(text).filter(|text| !text.is_empty())),
        Err(Error::ContentNotAvailable) => Ok(None),
        Err(e) => Err(e),
    }
}

/// What the clipboard is known to hold.
#[derive(Default)]
struct Known(Option<String>);

impl Known {
    fn set(&mut self, text: String) {
        self.0 = Some(text);
    }

    /// The clipboard holds `text`: the text to tell, if it is news.
    fn saw(&mut self, text: Option<String>) -> Option<String> {
        let text = text?;
        if self.0.as_ref() == Some(&text) {
            return None;
        }
        self.0 = Some(text.clone());
        (text.len() <= MAX_CLIPBOARD_TEXT).then_some(text)
    }

    /// The clipboard holds `text`, asked for: the text to tell, news or not.
    fn told(&mut self, text: Option<String>) -> Option<String> {
        let text = text?;
        self.0 = Some(text.clone());
        (text.len() <= MAX_CLIPBOARD_TEXT).then_some(text)
    }
}

/// The marks of what must not leave this computer, by their names: whoever
/// copies a password says so with one of them.
#[cfg(any(windows, target_os = "macos", test))]
mod marks {
    /// Windows: formats on the clipboard, present at all. ("Clipboard Viewer
    /// Ignore": older password managers.)
    #[cfg(windows)]
    pub const WINDOWS: [&str; 2] = [
        "ExcludeClipboardContentFromMonitorProcessing",
        "Clipboard Viewer Ignore",
    ];
    /// Windows: a format whose value 0 keeps the content out of the history
    /// (and so out of here).
    #[cfg(windows)]
    pub const WINDOWS_HISTORY: &str = "CanIncludeInClipboardHistory";
    /// macOS: pasteboard types (nspasteboard.org).
    #[cfg(any(target_os = "macos", test))]
    pub const MAC: [&str; 3] = [
        "org.nspasteboard.ConcealedType",
        "org.nspasteboard.TransientType",
        "org.nspasteboard.AutoGeneratedType",
    ];

    /// Whether types named `names` mark the content as private on macOS.
    #[cfg(any(target_os = "macos", test))]
    pub fn mac_private<'a>(mut names: impl Iterator<Item = &'a str>) -> bool {
        names.any(|name| MAC.contains(&name))
    }

    /// Whether the value of `WINDOWS_HISTORY` keeps the content here: `0`,
    /// or one that cannot be read (better here than out).
    #[cfg(any(windows, test))]
    pub fn history_private(value: Option<&[u8]>) -> bool {
        match value.and_then(|bytes| bytes.first_chunk::<4>()) {
            Some(value) => u32::from_le_bytes(*value) == 0,
            None => true,
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod os {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, GetClipboardSequenceNumber, IsClipboardFormatAvailable,
        OpenClipboard, RegisterClipboardFormatW,
    };
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows::core::HSTRING;

    use super::marks;

    /// Goes up on every change, without opening the clipboard.
    pub fn version() -> Option<u32> {
        // SAFETY: takes nothing and only reads a counter.
        Some(unsafe { GetClipboardSequenceNumber() })
    }

    /// The format named `name`, if the clipboard holds it.
    fn present(name: &str) -> Option<u32> {
        let name = HSTRING::from(name);
        // SAFETY: the name is a valid string; both calls only look it up.
        unsafe {
            let format = RegisterClipboardFormatW(&name);
            (format != 0 && IsClipboardFormatAvailable(format).is_ok()).then_some(format)
        }
    }

    /// Whether what was copied is marked private (password managers do);
    /// `None`: the clipboard is busy, try again.
    pub fn private() -> Option<bool> {
        if marks::WINDOWS.iter().any(|name| present(name).is_some()) {
            return Some(true);
        }
        let Some(history) = present(marks::WINDOWS_HISTORY) else {
            return Some(false);
        };
        // SAFETY: opened and closed here, by this thread; the data is read
        // only while locked, and only as many bytes as it has.
        unsafe {
            OpenClipboard(None).ok()?;
            let value = GetClipboardData(history).ok().and_then(|data| {
                let global = HGLOBAL(data.0);
                let len = GlobalSize(global);
                let at = GlobalLock(global).cast::<u8>();
                if at.is_null() {
                    return None;
                }
                let bytes = std::slice::from_raw_parts(at, len.min(4)).to_vec();
                let _ = GlobalUnlock(global);
                Some(bytes)
            });
            let _ = CloseClipboard();
            Some(marks::history_private(value.as_deref()))
        }
    }
}

#[cfg(target_os = "macos")]
mod os {
    use objc2_app_kit::NSPasteboard;

    use super::marks;

    pub fn version() -> Option<u32> {
        None
    }

    /// Whether what was copied is marked private (password managers do).
    pub fn private() -> Option<bool> {
        let Some(types) = NSPasteboard::generalPasteboard().types() else {
            return Some(false);
        };
        let names: Vec<String> = types.iter().map(|name| name.to_string()).collect();
        Some(marks::mac_private(names.iter().map(String::as_str)))
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod os {
    pub fn version() -> Option<u32> {
        None
    }

    pub fn private() -> Option<bool> {
        Some(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_was_there_before_is_not_told() {
        let mut known = Known::default();
        known.saw(Some("antes".into()));
        assert_eq!(known.saw(Some("antes".into())), None);
    }

    #[test]
    fn a_change_is_told_once() {
        let mut known = Known::default();
        assert_eq!(known.saw(Some("a".into())), Some("a".into()));
        assert_eq!(known.saw(Some("a".into())), None);
        assert_eq!(known.saw(Some("b".into())), Some("b".into()));
        assert_eq!(known.saw(Some("a".into())), Some("a".into()));
    }

    #[test]
    fn what_came_from_the_other_side_does_not_go_back() {
        let mut known = Known::default();
        known.set("do outro lado".into());
        assert_eq!(known.saw(Some("do outro lado".into())), None);
    }

    #[test]
    fn too_long_a_text_stays_here() {
        let mut known = Known::default();
        let long = "x".repeat(MAX_CLIPBOARD_TEXT + 1);
        assert_eq!(known.saw(Some(long.clone())), None);
        assert_eq!(known.saw(Some(long.clone())), None);
        assert_eq!(known.told(Some(long)), None);
    }

    #[test]
    fn no_text_tells_nothing() {
        assert_eq!(Known::default().saw(None), None);
        assert_eq!(Known::default().told(None), None);
    }

    #[test]
    fn asked_for_it_is_told_even_if_known() {
        let mut known = Known::default();
        known.saw(Some("copiado fora".into()));
        assert_eq!(
            known.told(Some("copiado fora".into())),
            Some("copiado fora".into())
        );
        // And not again as news.
        assert_eq!(known.saw(Some("copiado fora".into())), None);
    }

    #[test]
    fn password_managers_marks() {
        assert!(marks::mac_private(
            ["public.utf8-plain-text", "org.nspasteboard.ConcealedType"].into_iter()
        ));
        assert!(marks::mac_private(
            ["org.nspasteboard.TransientType"].into_iter()
        ));
        assert!(marks::mac_private(
            ["org.nspasteboard.AutoGeneratedType"].into_iter()
        ));
        assert!(!marks::mac_private(["public.utf8-plain-text"].into_iter()));
        assert!(marks::history_private(Some(&0u32.to_le_bytes())));
        assert!(!marks::history_private(Some(&1u32.to_le_bytes())));
        assert!(marks::history_private(None));
        assert!(marks::history_private(Some(&[1])));
    }
}
