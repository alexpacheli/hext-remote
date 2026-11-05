//! This computer's clipboard during a session: text only, both ways.
//!
//! What was copied before the session stays out of it: only a change is
//! told. Windows has a counter for changes; elsewhere the text is compared.

use std::io;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use arboard::{Clipboard as System, Error};
use hext_proto::MAX_CLIPBOARD_TEXT;
use tracing::warn;

/// How long a change takes to be noticed, at most.
const POLL: Duration = Duration::from_millis(300);

/// The clipboard being watched, until dropped.
pub struct Clipboard {
    set: mpsc::Sender<String>,
}

impl Clipboard {
    /// Watches the clipboard on a thread of its own, calling `changed` with
    /// each new text. What [`Self::set`] puts there does not come back.
    pub fn watch(changed: impl FnMut(String) + Send + 'static) -> io::Result<Self> {
        let (set, texts) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("hext-clipboard".into())
            .spawn(move || match System::new() {
                Ok(system) => {
                    let _ = ready_tx.send(Ok(()));
                    watch(system, &texts, changed);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(io::Error::other(e)));
                }
            })?;
        ready_rx
            .recv()
            .map_err(|_| io::Error::other("clipboard thread died"))??;
        Ok(Self { set })
    }

    /// Replaces the clipboard's content.
    pub fn set(&self, text: String) {
        let _ = self.set.send(text);
    }
}

fn watch(mut system: System, texts: &mpsc::Receiver<String>, mut changed: impl FnMut(String)) {
    let mut version = os::version();
    let mut known = Known::default();
    known.saw(read(&mut system).ok().flatten());
    loop {
        match texts.recv_timeout(POLL) {
            Ok(text) => match system.set_text(text.as_str()) {
                Ok(()) => known.set(text),
                Err(e) => warn!("área de transferência: {e}"),
            },
            Err(RecvTimeoutError::Timeout) => {
                let now = os::version();
                if now.is_some() && now == version {
                    continue;
                }
                if os::private() {
                    version = now;
                    continue;
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
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod os {
    use windows::Win32::System::DataExchange::{
        GetClipboardSequenceNumber, IsClipboardFormatAvailable, RegisterClipboardFormatW,
    };
    use windows::core::w;

    /// Goes up on every change, without opening the clipboard.
    pub fn version() -> Option<u32> {
        // SAFETY: takes nothing and only reads a counter.
        Some(unsafe { GetClipboardSequenceNumber() })
    }

    /// Password managers mark what they copy with this format.
    pub fn private() -> bool {
        // SAFETY: the name is a valid string; both calls only look it up.
        unsafe {
            let format =
                RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
            format != 0 && IsClipboardFormatAvailable(format).is_ok()
        }
    }
}

#[cfg(not(windows))]
mod os {
    pub fn version() -> Option<u32> {
        None
    }

    pub fn private() -> bool {
        false
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
        assert_eq!(known.saw(Some(long)), None);
    }

    #[test]
    fn no_text_tells_nothing() {
        assert_eq!(Known::default().saw(None), None);
    }
}
