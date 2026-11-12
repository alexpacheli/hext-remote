//! COM and WMI for this process, set up once before `iroh` queries WMI.
//!
//! Each endpoint's network monitor asks WMI for the default route, from a
//! blocking thread, and with several endpoints opening at once (the
//! signaling tests open five) those queries overlap. On the Windows runner
//! of CI the tests hung in calls that allocate, or crashed with an access
//! violation; under page heap every run stopped inside COM in those
//! queries, one of them in code of `wbemprox.dll` already unloaded.
//!
//! Two things, both done once here before any endpoint:
//!
//! - WMI's own DLLs stay loaded. When a query lets go of its objects, the
//!   DLL that served them (`wbemprox.dll`, `fastprox.dll`) has nothing in
//!   use and COM unloads it shortly after, from a thread-pool timer, while
//!   another thread may still be running inside it.
//! - The process joins the multithreaded apartment and its COM security is
//!   set before the first query. Otherwise the `wmi` crate does both inside
//!   whichever query first finds COM not set up, while other threads are
//!   already calling WMI; Windows wants the security set before any of
//!   that. The same choices `wmi` would make, so nothing else changes.

#[allow(unsafe_code)] // plain Win32 calls, with no pointers of ours
pub fn init() {
    use std::sync::Once;

    use windows::Win32::Foundation::RPC_E_TOO_LATE;
    use windows::Win32::System::Com::{
        CoIncrementMTAUsage, CoInitializeSecurity, EOAC_NONE, RPC_C_AUTHN_LEVEL_DEFAULT,
        RPC_C_IMP_LEVEL_IMPERSONATE,
    };
    use windows::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
    use windows::core::{HSTRING, PCWSTR};

    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // Loaded here too, and never freed: COM unloading them only drops
        // its own reference. From System32 only.
        for dll in ["wbemprox.dll", "fastprox.dll"] {
            let name = HSTRING::from(dll);
            // SAFETY: a valid, terminated name; the module is kept for good.
            let loaded = unsafe {
                LoadLibraryExW(PCWSTR(name.as_ptr()), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
            };
            if let Err(e) = loaded {
                tracing::warn!("{dll}: {e}");
            }
        }
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
