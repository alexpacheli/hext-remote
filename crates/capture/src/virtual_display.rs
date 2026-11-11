//! A monitor that exists only for the session (docs/monitor-virtual.md), for a
//! computer with no monitor to capture.
//!
//! Windows: the Virtual Display Driver (`Root\MttVDD`, installed apart), whose
//! device is enabled while a session needs it and disabled after, through the
//! SetupAPI (as Device Manager does): the driver's own control pipe takes
//! orders from any local user, so it is not used. Needs an administrator (the
//! agent runs as SYSTEM). Elsewhere: not available yet.
//!
//! Only what this process turned on is turned off: a virtual monitor the
//! computer's owner keeps on stays on. A mark on disk says that the Hext
//! turned it on, so a service that died in between turns it off when it
//! starts again ([`turn_off_leftover`]).

use std::sync::Mutex;

use crate::CaptureError;

/// While alive, the virtual monitor stays on. The last one dropped turns it
/// off, if it was off before the first.
#[derive(Debug)]
pub struct Lease(());

/// How many leases there are, and whether the first found the monitor off.
static LEASES: Mutex<(usize, bool)> = Mutex::new((0, false));

/// The driver's device, as found.
/// (Only Windows finds it on or broken, for now.)
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    On,
    /// Disabled: off, and the Hext may turn it on.
    Off,
    /// Some other problem (`CM_PROB_*`): it would not come on.
    Broken(u32),
}

/// Whether this process can turn a virtual monitor on: the driver is
/// installed, this process may change devices (an administrator: the agent
/// of the service, as SYSTEM), and the service's folder is there to keep the
/// mark (what is turned on is turned off even if this process dies).
pub fn available() -> bool {
    imp::can_change() && imp::installed()
}

/// Whether the virtual monitor that is on was turned on by the Hext (this
/// process, or one before it that left the mark), and not by the computer's
/// owner.
pub fn turned_on_by_hext() -> bool {
    LEASES.lock().expect("lock").1 || imp::marked()
}

/// Turns the virtual monitor on (if it is not), for as long as the lease lives.
pub fn turn_on() -> Result<Lease, CaptureError> {
    let mut leases = LEASES.lock().expect("lock");
    if leases.0 == 0 {
        let was_on = match imp::state()? {
            State::On => true,
            State::Off => false,
            State::Broken(problem) => {
                return Err(CaptureError::Platform(format!(
                    "monitor virtual: o dispositivo não funciona (código {problem} no Gerenciador de Dispositivos)"
                )));
            }
        };
        if !was_on {
            imp::mark(true);
            if let Err(e) = imp::set_on(true) {
                imp::mark(false);
                return Err(e);
            }
            tracing::info!("monitor virtual: ligado");
        }
        // On with the mark: the Hext turned it on (an agent that was
        // replaced in the middle of a session), so it turns it off too.
        leases.1 = !was_on || imp::marked();
    }
    leases.0 += 1;
    Ok(Lease(()))
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut leases = LEASES.lock().expect("lock");
        leases.0 -= 1;
        if leases.0 == 0 && std::mem::take(&mut leases.1) {
            match imp::set_on(false) {
                Ok(()) => {
                    imp::mark(false);
                    tracing::info!("monitor virtual: desligado");
                }
                // The mark stays: the service tries again when it starts.
                Err(e) => tracing::warn!("monitor virtual: não desligou: {e}"),
            }
        }
    }
}

impl Lease {
    /// Whether letting go of the last lease turns the monitor off (the Hext
    /// turned it on).
    pub fn owns(&self) -> bool {
        LEASES.lock().expect("lock").1
    }
}

/// Turns off a virtual monitor the Hext left on (a service or agent that
/// died during a session). For the service, when it starts.
pub fn turn_off_leftover() {
    if !imp::marked() {
        return;
    }
    match imp::set_on(false) {
        Ok(()) => {
            imp::mark(false);
            tracing::info!("monitor virtual: desligado (ficou ligado de uma sessão anterior)");
        }
        Err(e) => tracing::warn!("monitor virtual: o que ficou ligado não desligou: {e}"),
    }
}

#[cfg(windows)]
#[allow(unsafe_code)] // SetupAPI, GDI
mod imp {
    use std::path::PathBuf;

    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_DEVNODE_STATUS_FLAGS, CM_Get_DevNode_Status, CM_PROB, CR_SUCCESS, DI_FUNCTION,
        DICS_DISABLE, DICS_ENABLE, DICS_FLAG_GLOBAL, DIF_PROPERTYCHANGE, GUID_DEVCLASS_DISPLAY,
        HDEVINFO, SP_CLASSINSTALL_HEADER, SP_DEVINFO_DATA, SP_PROPCHANGE_PARAMS, SPDRP_HARDWAREID,
        SetupDiCallClassInstaller, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo,
        SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW, SetupDiSetClassInstallParamsW,
    };
    use windows::Win32::Graphics::Gdi::{DISPLAY_DEVICEW, EnumDisplayDevicesW};
    use windows::Win32::UI::Shell::IsUserAnAdmin;
    use windows::core::{HSTRING, PCWSTR};

    use super::State;
    use crate::CaptureError;

    /// Disabled (`CM_PROB_DISABLED`).
    const PROBLEM_DISABLED: u32 = 22;

    /// The driver's device (its INF installs `Root\MttVDD`).
    const HARDWARE_ID: &str = "root\\mttvdd";
    /// Its monitor, as Windows names it (`MONITOR\MTT1337\…`).
    const MONITOR_ID: &str = "\\MTT1337";
    /// Where the driver reads its settings when nothing in the registry says
    /// otherwise (`HKLM\SOFTWARE\MikeTheTech\VirtualDisplayDriver\VDDPATH`,
    /// which the Hext does not set).
    const SETTINGS: &str = r"C:\VirtualDisplayDriver\vdd_settings.xml";

    /// One monitor, in the sizes a technician needs, at 60 Hz, 1920×1080
    /// first (the one it starts in), and a cursor of its own: without it,
    /// Windows draws the cursor into the image, and the viewer, which draws
    /// its own, would show two.
    const SETTINGS_XML: &str = r#"<?xml version='1.0' encoding='utf-8'?>
<!-- Escrito pelo Hext Remote: o monitor virtual das sessões sem monitor. -->
<vdd_settings>
    <monitors>
        <count>1</count>
    </monitors>
    <gpu>
        <friendlyname>default</friendlyname>
    </gpu>
    <global>
        <g_refresh_rate>60</g_refresh_rate>
    </global>
    <resolutions>
        <resolution>
            <width>1920</width>
            <height>1080</height>
            <refresh_rate>60</refresh_rate>
        </resolution>
        <resolution>
            <width>1280</width>
            <height>720</height>
            <refresh_rate>60</refresh_rate>
        </resolution>
        <resolution>
            <width>1600</width>
            <height>900</height>
            <refresh_rate>60</refresh_rate>
        </resolution>
        <resolution>
            <width>2560</width>
            <height>1440</height>
            <refresh_rate>60</refresh_rate>
        </resolution>
    </resolutions>
    <cursor>
        <HardwareCursor>true</HardwareCursor>
        <CursorMaxX>128</CursorMaxX>
        <CursorMaxY>128</CursorMaxY>
        <AlphaCursorSupport>true</AlphaCursorSupport>
        <XorCursorSupportLevel>2</XorCursorSupportLevel>
    </cursor>
</vdd_settings>
"#;

    struct List(HDEVINFO);

    impl Drop for List {
        fn drop(&mut self) {
            let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) };
        }
    }

    fn hardware_id(list: &List, data: &SP_DEVINFO_DATA) -> String {
        let mut buf = [0u8; 1024];
        if unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                list.0,
                data,
                SPDRP_HARDWAREID,
                None,
                Some(&mut buf),
                None,
            )
        }
        .is_err()
        {
            return String::new();
        }
        let wide: Vec<u16> = buf
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        // A list of strings: the first is the one.
        let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
        String::from_utf16_lossy(&wide[..end])
    }

    /// The driver's devices (one, unless installed twice), among the display
    /// adapters (its class): not every device of the computer.
    fn devices() -> Result<(List, Vec<SP_DEVINFO_DATA>), CaptureError> {
        let flags = Default::default();
        let list = List(
            unsafe {
                SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_DISPLAY), PCWSTR::null(), None, flags)
            }
            .map_err(|e| CaptureError::Platform(format!("SetupAPI: {e}")))?,
        );
        let mut found = Vec::new();
        for index in 0.. {
            let mut data = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            if unsafe { SetupDiEnumDeviceInfo(list.0, index, &mut data) }.is_err() {
                break;
            }
            if hardware_id(&list, &data).eq_ignore_ascii_case(HARDWARE_ID) {
                found.push(data);
            }
        }
        Ok((list, found))
    }

    /// The device's `CM_PROB_*` (0: working), if it has a state.
    fn problem(data: &SP_DEVINFO_DATA) -> Option<u32> {
        let mut status = CM_DEVNODE_STATUS_FLAGS(0);
        let mut problem = CM_PROB(0);
        let result = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, data.DevInst, 0) };
        (result == CR_SUCCESS).then_some(problem.0)
    }

    pub fn installed() -> bool {
        devices().is_ok_and(|(_, found)| !found.is_empty())
    }

    pub fn state() -> Result<State, CaptureError> {
        let (_list, found) = devices()?;
        let problems: Vec<u32> = found.iter().filter_map(problem).collect();
        Ok(if problems.contains(&0) {
            State::On
        } else if let Some(&other) = problems.iter().find(|&&p| p != PROBLEM_DISABLED) {
            State::Broken(other)
        } else {
            State::Off
        })
    }

    pub fn can_change() -> bool {
        unsafe { IsUserAnAdmin() }.as_bool()
            && mark_path()
                .as_deref()
                .and_then(std::path::Path::parent)
                .is_some_and(std::path::Path::exists)
    }

    pub fn set_on(on: bool) -> Result<(), CaptureError> {
        let (list, found) = devices()?;
        if found.is_empty() {
            return Err(CaptureError::Platform(
                "monitor virtual: o driver não está instalado".into(),
            ));
        }
        for data in &found {
            let params = SP_PROPCHANGE_PARAMS {
                ClassInstallHeader: SP_CLASSINSTALL_HEADER {
                    cbSize: size_of::<SP_CLASSINSTALL_HEADER>() as u32,
                    InstallFunction: DIF_PROPERTYCHANGE,
                },
                StateChange: if on { DICS_ENABLE } else { DICS_DISABLE },
                Scope: DICS_FLAG_GLOBAL,
                HwProfile: 0,
            };
            unsafe {
                SetupDiSetClassInstallParamsW(
                    list.0,
                    Some(data),
                    Some(&params.ClassInstallHeader),
                    size_of::<SP_PROPCHANGE_PARAMS>() as u32,
                )
                .and_then(|()| {
                    SetupDiCallClassInstaller(DI_FUNCTION(DIF_PROPERTYCHANGE.0), list.0, Some(data))
                })
                .map_err(|e| CaptureError::Platform(format!("monitor virtual: {e}")))?;
            }
        }
        Ok(())
    }

    /// Whether `device` (`\\.\DISPLAYn`) shows the virtual monitor.
    pub fn is_virtual(device: &str) -> bool {
        let name = HSTRING::from(device);
        let mut monitor = DISPLAY_DEVICEW {
            cb: size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        if !unsafe { EnumDisplayDevicesW(&name, 0, &mut monitor, 0) }.as_bool() {
            return false;
        }
        let id = &monitor.DeviceID;
        let end = id.iter().position(|&c| c == 0).unwrap_or(id.len());
        String::from_utf16_lossy(&id[..end])
            .to_ascii_uppercase()
            .contains(MONITOR_ID)
    }

    pub fn settings() -> Option<(PathBuf, &'static str)> {
        Some((PathBuf::from(SETTINGS), SETTINGS_XML))
    }

    /// The mark that the Hext turned it on, in the service's folder
    /// (`%ProgramData%\HextRemote`; without a service, there is none).
    fn mark_path() -> Option<PathBuf> {
        let data = std::env::var_os("ProgramData").filter(|v| !v.is_empty())?;
        Some(
            PathBuf::from(data)
                .join("HextRemote")
                .join("monitor-virtual-ligado"),
        )
    }

    pub fn mark(on: bool) {
        let Some(path) = mark_path() else { return };
        let done = if on {
            if !path.parent().is_some_and(std::path::Path::exists) {
                return;
            }
            std::fs::write(&path, b"")
        } else {
            std::fs::remove_file(&path).or_else(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(e)
                }
            })
        };
        if let Err(e) = done {
            tracing::warn!("monitor virtual: {}: {e}", path.display());
        }
    }

    pub fn marked() -> bool {
        mark_path().is_some_and(|path| path.exists())
    }
}

#[cfg(not(windows))]
mod imp {
    use crate::CaptureError;

    use super::State;

    pub fn installed() -> bool {
        false
    }

    pub fn can_change() -> bool {
        false
    }

    pub fn state() -> Result<State, CaptureError> {
        Ok(State::Off)
    }

    pub fn set_on(_on: bool) -> Result<(), CaptureError> {
        Err(CaptureError::Unsupported)
    }

    pub fn settings() -> Option<(std::path::PathBuf, &'static str)> {
        None
    }

    pub fn mark(_on: bool) {}

    pub fn marked() -> bool {
        false
    }
}

/// Where the driver reads its settings, and the Hext's, for the service to
/// write unless someone keeps their own there (a computer that uses the
/// driver for something else keeps its setup).
pub fn settings() -> Option<(std::path::PathBuf, &'static str)> {
    imp::settings()
}

/// Whether display `id` ([`crate::Display::id`]) is the virtual monitor.
#[cfg(windows)]
pub(crate) fn is_virtual(id: &str) -> bool {
    imp::is_virtual(id)
}
