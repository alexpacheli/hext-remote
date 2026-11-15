//! The host's sound, heard in the viewer (docs/planos/F7.md, marco 3).
//!
//! Host: [`Capture`] records what the computer plays (WASAPI loopback on
//! Windows, ScreenCaptureKit on macOS 13+), cuts it into 10 ms frames and
//! encodes them with Opus ([`Encoder`]). A frame of digital silence, after a
//! short while of them, is not sent at all ([`SilenceGate`]).
//!
//! Viewer: [`Player`] decodes the packets as they come ([`Depacketizer`]:
//! a lost packet is rebuilt from the copy the next one carries, or
//! concealed by Opus) into a short queue ([`Playout`], about 40 ms) that the
//! audio device drains. The queue never grows past a bound: a burst after
//! the network stalls is cut, not played late. The device opens only once
//! sound arrives.
//!
//! Sound runs on threads of its own, never on the window's.

mod capture;
mod dsp;
mod encode;
mod player;
mod playout;
#[cfg(target_os = "macos")]
mod sck;
#[cfg(windows)]
mod wasapi;

pub use capture::{Capture, loopback_available};
pub use dsp::{Framer, Resampler, from_stereo, to_stereo};
pub use encode::{Encoder, SilenceGate};
pub use player::Player;
pub use playout::{Depacketizer, Playout};

/// Opus's own rate: what travels, whatever the devices use.
pub const SAMPLE_RATE: u32 = 48_000;
/// Stereo, interleaved (left, right, left...).
pub const CHANNELS: usize = 2;
/// Samples per channel in a frame: 10 ms.
pub const FRAME: usize = SAMPLE_RATE as usize / 100;
/// Opus bit rate: good music in stereo. Each packet also carries the one
/// before it, so about twice this travels while something plays.
pub const BITRATE: i32 = 64_000;

/// Where the sound's measurements go, like the video's.
pub const LOG_TARGET: &str = "hext_audio";

/// Whether an audio device stream's error means it stopped. A glitch (an
/// xrun: a buffer that ran over or dry), a refused real-time priority or a
/// stream already following the new default device leave it running, and
/// opening it again would lose more sound than they did (on Windows, the
/// loopback reports glitches several times a second).
pub(crate) fn stream_stopped(error: &cpal::Error) -> bool {
    !matches!(
        error.kind(),
        cpal::ErrorKind::Xrun | cpal::ErrorKind::RealtimeDenied | cpal::ErrorKind::DeviceChanged
    )
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("opus: {0}")]
    Opus(#[from] opus::Error),
    #[error("{0}")]
    Device(String),
    #[error("este sistema não captura o som")]
    Unsupported,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_error_that_stops_the_stream_opens_the_device_again() {
        for kind in [
            cpal::ErrorKind::Xrun,
            cpal::ErrorKind::RealtimeDenied,
            cpal::ErrorKind::DeviceChanged,
        ] {
            assert!(!stream_stopped(&kind.into()), "{kind:?}");
        }
        for kind in [
            cpal::ErrorKind::DeviceNotAvailable,
            cpal::ErrorKind::StreamInvalidated,
            cpal::ErrorKind::HostUnavailable,
        ] {
            assert!(stream_stopped(&kind.into()), "{kind:?}");
        }
    }
}
