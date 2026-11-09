//! Session pipelines on top of the transport.
//!
//! - [`host::serve`]: capture → encode → video stream, input stream → injector.
//! - [`client::start`]: video stream → decode → caller's callback, and the
//!   caller's input events → input stream.

mod chat_pace;
pub mod client;
pub mod files;
pub mod host;
mod lockout;
mod resume;
mod session_codes;
mod stats;

pub use lockout::Lockout;
pub use resume::{
    KeptResume, NETWORK_GRACE, RESTART_LIFETIME, ResumeToken, ResumeTokens, TokenHash,
};
pub use session_codes::{Grant, SessionCodes};
pub use stats::Stats;

use hext_capture::CaptureError;
use hext_codec::CodecError;
use hext_net::NetError;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Net(#[from] NetError),
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error("{0}")]
    Protocol(String),
}

pub type Result<T, E = SessionError> = std::result::Result<T, E>;

/// Access codes are shown grouped ("482 913 557"); compare them without
/// separators and in constant time.
pub fn codes_match(expected: &str, given: &str) -> bool {
    let normalize = |s: &str| -> Vec<u8> {
        s.bytes()
            .filter(|b| !b.is_ascii_whitespace() && *b != b'-')
            .collect()
    };
    hext_crypto::constant_time_eq(&normalize(expected), &normalize(given))
}

/// How much of a peer's name is shown.
const MAX_SHOWN_NAME: usize = 64;

/// A name as the other side sent it, made safe to show and log: nothing that
/// moves a terminal's cursor or reverses text around it, and not too long.
pub fn shown_name(name: &str) -> String {
    name.chars()
        .filter(|&c| {
            !c.is_control()
                && !matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        })
        .take(MAX_SHOWN_NAME)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_shown_lose_what_hides_them() {
        assert_eq!(shown_name("abc\u{202E}gnp.exe\n"), "abcgnp.exe");
        assert_eq!(shown_name(&"x".repeat(500)).len(), MAX_SHOWN_NAME);
        assert_eq!(shown_name("Recepção"), "Recepção");
    }

    #[test]
    fn access_codes() {
        assert!(codes_match("482913557", "482 913 557"));
        assert!(codes_match("482913557", "482-913-557"));
        assert!(!codes_match("482913557", "482913558"));
        assert!(!codes_match("482913557", "48291355"));
        assert!(!codes_match("482913557", ""));
    }
}
