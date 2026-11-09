//! Keeping the screen on while it is being shown: a display turned off for
//! being idle has nothing to capture, and the viewer would see black until
//! someone at the computer moved the mouse.
#![allow(unsafe_code)] // system calls

/// While it lives, the display is on: turned on when made (if it slept) and
/// kept from sleeping for being idle. Dropped, the usual timeouts apply
/// again. Made and dropped on the same thread (on Windows the request is
/// the thread's).
pub struct KeepAwake {
    #[cfg(target_os = "macos")]
    assertion: Option<u32>,
    /// Bound to its thread.
    _thread: std::marker::PhantomData<*const ()>,
}

impl KeepAwake {
    pub fn new() -> Self {
        let awake = Self {
            #[cfg(target_os = "macos")]
            assertion: macos::keep_on(),
            _thread: std::marker::PhantomData,
        };
        #[cfg(windows)]
        windows::keep_on();
        awake
    }
}

impl Default for KeepAwake {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        #[cfg(windows)]
        windows::release();
        #[cfg(target_os = "macos")]
        if let Some(id) = self.assertion.take() {
            macos::release(id);
        }
    }
}

#[cfg(windows)]
mod windows {
    use windows::Win32::System::Power::{
        ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput,
    };

    pub fn keep_on() {
        // Kept on until released; also wakes a display that slept.
        let previous = unsafe {
            SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED)
        };
        if previous.0 == 0 {
            tracing::warn!("tela: o pedido para mantê-la ligada falhou");
        }
        // A mouse move of nothing: some monitors only wake on input. (The
        // pointer stays where it is.)
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dwFlags: MOUSEEVENTF_MOVE,
                    ..Default::default()
                },
            },
        };
        // (Refused on a desktop this thread cannot reach: then only the
        // request above wakes it.)
        let _ = unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
    }

    pub fn release() {
        unsafe {
            SetThreadExecutionState(ES_CONTINUOUS);
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2_core_foundation::CFString;

    /// `kIOPMAssertionLevelOn`.
    const LEVEL_ON: u32 = 255;
    /// `kIOPMUserActiveLocal`.
    const USER_ACTIVE_LOCAL: u32 = 0;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            kind: *const CFString,
            level: u32,
            name: *const CFString,
            id: *mut u32,
        ) -> i32;
        fn IOPMAssertionDeclareUserActivity(name: *const CFString, kind: u32, id: *mut u32) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    /// Wakes the display and keeps it on; the assertion to release.
    pub fn keep_on() -> Option<u32> {
        let name = CFString::from_str("Hext Remote: sessão em andamento");
        let kind = CFString::from_str("PreventUserIdleDisplaySleep");
        let mut id = 0;
        let status = unsafe { IOPMAssertionCreateWithName(&*kind, LEVEL_ON, &*name, &mut id) };
        if status != 0 {
            tracing::warn!("tela: o pedido para mantê-la ligada falhou ({status})");
        }
        // Wakes a sleeping display, as someone touching the mouse would;
        // then let go (the assertion above keeps it on).
        let mut activity = 0;
        if unsafe { IOPMAssertionDeclareUserActivity(&*name, USER_ACTIVE_LOCAL, &mut activity) }
            == 0
        {
            release(activity);
        }
        (status == 0).then_some(id)
    }

    pub fn release(id: u32) {
        unsafe {
            IOPMAssertionRelease(id);
        }
    }
}
