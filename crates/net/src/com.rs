//! COM and WMI for this process, set up once before `iroh` queries WMI.
//!
//! Each endpoint's network monitor asks WMI for the default route, from a
//! blocking thread, opening and closing its own connection each time; with
//! several endpoints opening at once (the signaling tests open five) those
//! queries overlap. On the Windows runner of CI the tests hung in calls
//! that allocate, or crashed with an access violation. Under page heap
//! every run stopped inside COM in those queries, on memory already freed:
//! mostly the entry COM keeps for the WMI service (its `OXIDEntry`), once
//! in code of `wbemprox.dll` already unloaded.
//!
//! What COM builds for the first connection it takes down again with the
//! last; overlapping queries race that. So, once, before any endpoint:
//!
//! - The process joins the multithreaded apartment and sets its COM
//!   security, as the `wmi` crate otherwise does inside whichever query
//!   first finds COM not set up (Windows wants the security set before any
//!   object is in use; the same choices `wmi` makes).
//! - WMI's DLLs (`wbemprox.dll`, `fastprox.dll`) are loaded and kept.
//! - A connection to WMI is opened and kept for the life of the process,
//!   so the service's entry is never taken down while a query runs.

#[allow(unsafe_code)] // Win32 and COM calls, on values that live here
pub fn init() {
    use std::sync::Once;

    use windows::Win32::Foundation::RPC_E_TOO_LATE;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, CoCreateInstance, CoIncrementMTAUsage, CoInitializeSecurity,
        EOAC_NONE, RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE,
    };
    use windows::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
    use windows::Win32::System::Wmi::{IWbemContext, IWbemLocator, WbemLocator};
    use windows::core::{BSTR, HSTRING, PCWSTR};

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

        // The connection kept open (where `wmi` connects, the same way).
        let kept = (|| -> windows::core::Result<()> {
            // SAFETY: COM is set up above; both objects are kept for good.
            unsafe {
                let locator: IWbemLocator =
                    CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;
                let services = locator.ConnectServer(
                    &BSTR::from(r"ROOT\CIMV2"),
                    &BSTR::new(),
                    &BSTR::new(),
                    &BSTR::new(),
                    0,
                    &BSTR::new(),
                    None::<&IWbemContext>,
                )?;
                std::mem::forget(services);
                std::mem::forget(locator);
            }
            Ok(())
        })();
        if let Err(e) = kept {
            tracing::warn!("WMI: {e}");
        }
    });
}
