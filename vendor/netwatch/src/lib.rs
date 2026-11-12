//! Networking related utilities
// Patched copy (Hext Remote): as a path dependency its warnings are not
// capped as a registry crate's are, and CI denies warnings.
#![allow(warnings)]

pub mod interfaces;
pub mod ip;
mod ip_family;
pub mod netmon;
#[cfg(not(wasm_browser))]
mod udp;

pub use self::ip_family::IpFamily;
#[cfg(not(wasm_browser))]
pub use self::udp::{UdpSender, UdpSocket};
