//! Networking related utilities
// Patched copy (Hext Remote): as a path dependency its warnings are not
// capped as a registry crate's are, and CI denies warnings. The modules
// copied unchanged keep their warnings quiet; `interfaces` (with the
// patched `interfaces/windows.rs`) is checked as our own code is.

pub mod interfaces;
#[allow(warnings)]
pub mod ip;
#[allow(warnings)]
mod ip_family;
#[allow(warnings)]
pub mod netmon;
#[cfg(not(wasm_browser))]
#[allow(warnings)]
mod udp;

pub use self::ip_family::IpFamily;
#[cfg(not(wasm_browser))]
pub use self::udp::{UdpSender, UdpSocket};
