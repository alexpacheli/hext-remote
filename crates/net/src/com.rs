//! COM for this process, set up once before `iroh` queries WMI.
//!
//! Each endpoint's network monitor asks WMI for the default route, from a
//! blocking thread. The `wmi` crate, finding COM not yet set up, sets it up
//! itself inside that query: it joins the process to the multithreaded
//! apartment (`CoIncrementMTAUsage`) and then sets the process's security
//! (`CoInitializeSecurity`). With several endpoints opening at once, one
//! thread sets the security while others, already in the apartment, are
//! creating and calling WMI objects; Windows requires it before any of
//! that, and inside COM objects were used after being freed. On the Windows
//! runner of CI, the signaling tests (five endpoints at once) hung in calls
//! that allocate or crashed (access violation); under page heap, every run
//! stopped in COM inside those WMI queries.
//!
//! Done once here, before any endpoint, nothing is left for the queries to
//! set up: they find the apartment and the security in place. The same
//! choices `wmi` would make, so nothing else changes.

#[allow(unsafe_code)] // plain Win32 calls, with no pointers of ours
pub fn init() {
    use std::sync::Once;

    use windows::Win32::Foundation::RPC_E_TOO_LATE;
    use windows::Win32::System::Com::{
        CoIncrementMTAUsage, CoInitializeSecurity, EOAC_NONE, RPC_C_AUTHN_LEVEL_DEFAULT,
        RPC_C_IMP_LEVEL_IMPERSONATE,
    };

    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // The process stays in the apartment for good (the cookie that
        // would leave it is dropped), as `wmi` does.
        // SAFETY: takes nothing and returns a cookie.
        if let Err(e) = unsafe { CoIncrementMTAUsage() } {
            tracing::warn!("CoIncrementMTAUsage: {e}");
            return;
        }
        // SAFETY: no pointers: the defaults, as `wmi` passes them.
        let security = unsafe {
            CoInitializeSecurity(
                None,
                -1, // let COM choose
                None,
                None,
                RPC_C_AUTHN_LEVEL_DEFAULT,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
                None,
            )
        };
        // Already set (by the interface, say): those choices stand.
        if let Err(e) = security
            && e.code() != RPC_E_TOO_LATE
        {
            tracing::warn!("CoInitializeSecurity: {e}");
        }
    });
}
