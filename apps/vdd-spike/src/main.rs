//! Spike do monitor virtual (docs/monitor-virtual.md, marco 0), Windows.
//!
//! Liga e desliga o dispositivo do Virtual Display Driver (`Root\MttVDD`)
//! como o serviço do Hext faria (SetupAPI, sem o pipe do driver), mede quanto
//! o monitor leva para aparecer na lista do DXGI e captura nele.
//!
//!   vdd-spike status
//!   vdd-spike enable           (administrador)
//!   vdd-spike disable          (administrador)
//!   vdd-spike capture [N]      (monitor N da lista; padrão: o último)
//!   vdd-spike cycle [N]        (N vezes: liga, captura, desliga; administrador)
//!   vdd-spike modes [N]        (resoluções que o monitor N aceita)
//!   vdd-spike resolution N WxH (muda o monitor N para WxH a 60 Hz)

use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).map(String::as_str);
    let index = |i: usize| arg(i).map(str::parse::<usize>).transpose();
    match arg(0).unwrap_or("status") {
        "status" => status(),
        "capture" => capture(index(1)?),
        #[cfg(windows)]
        "enable" => enable(),
        #[cfg(windows)]
        "disable" => disable(),
        #[cfg(windows)]
        "cycle" => cycle(arg(1).map(str::parse).transpose()?.unwrap_or(5)),
        #[cfg(windows)]
        "modes" => gdi::modes(&pick(index(1)?)?.id),
        #[cfg(windows)]
        "resolution" => {
            let (w, h) = arg(2)
                .and_then(|s| s.split_once(['x', 'X']))
                .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                .ok_or_else(|| anyhow::anyhow!("uso: resolution N 1920x1080"))?;
            gdi::set_resolution(&pick(index(1)?)?.id, w, h)
        }
        other => anyhow::bail!(
            "comando desconhecido: {other} (status, enable, disable, capture, cycle, modes, resolution)"
        ),
    }
}

/// How long a display may take to show up or go away.
#[cfg(windows)]
const CHANGE_WAIT: Duration = Duration::from_secs(20);

fn status() -> anyhow::Result<()> {
    #[cfg(windows)]
    win::print_devices()?;
    list_displays();
    Ok(())
}

fn displays() -> Vec<hext_capture::Display> {
    hext_capture::displays(false).unwrap_or_else(|e| {
        println!("monitores: erro ({e})");
        Vec::new()
    })
}

fn list_displays() {
    let displays = displays();
    println!("monitores no DXGI: {}", displays.len());
    for (i, d) in displays.iter().enumerate() {
        println!(
            "  {i}: {}  {}x{} em ({}, {})",
            d.id, d.rect.width, d.rect.height, d.rect.x, d.rect.y
        );
    }
}

/// Display `index` of the list (default: the last, where the virtual one goes).
fn pick(index: Option<usize>) -> anyhow::Result<hext_capture::Display> {
    let mut displays = displays();
    let index = index.unwrap_or(displays.len().saturating_sub(1));
    anyhow::ensure!(index < displays.len(), "não há monitor {index}");
    Ok(displays.swap_remove(index))
}

/// Opens display `index` (default: the last) and counts frames for 3 s.
fn capture(index: Option<usize>) -> anyhow::Result<()> {
    let display = pick(index)?;
    let start = Instant::now();
    let mut capturer = hext_capture::open_display(&display, false, 60)?;
    println!("captura de {} aberta em {:?}", display.id, start.elapsed());
    let (mut frames, mut size) = (0u32, None);
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until {
        if capturer.next_frame(Duration::from_millis(100))? {
            frames += 1;
            if let Some(f) = capturer.frame() {
                size = Some((f.width, f.height));
            }
        }
    }
    match size {
        Some((w, h)) => println!("OK: {frames} imagens em 3 s, {w}x{h}"),
        None => println!(
            "SEM IMAGEM em 3 s (normal se nada mudou na tela: mova uma janela para esse monitor e repita)"
        ),
    }
    Ok(())
}

/// Waits until the number of displays satisfies `done`.
#[cfg(windows)]
fn wait_for(what: &str, start: Instant, done: impl Fn(usize) -> bool) -> anyhow::Result<()> {
    while start.elapsed() < CHANGE_WAIT {
        if done(displays().len()) {
            println!("{what} em {:?}", start.elapsed());
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!("{what}: não aconteceu em {CHANGE_WAIT:?}")
}

#[cfg(windows)]
fn enable() -> anyhow::Result<()> {
    if win::is_enabled()? {
        println!("o dispositivo já estava habilitado (rode disable antes para medir)");
    } else {
        let before = displays().len();
        let start = Instant::now();
        win::set_enabled(true)?;
        wait_for("habilitado; o monitor apareceu no DXGI", start, |n| {
            n > before
        })?;
    }
    list_displays();
    Ok(())
}

#[cfg(windows)]
fn disable() -> anyhow::Result<()> {
    if !win::is_enabled()? {
        println!("o dispositivo já estava desabilitado");
    } else {
        let before = displays().len();
        let start = Instant::now();
        win::set_enabled(false)?;
        wait_for("desabilitado; o monitor saiu do DXGI", start, |n| {
            n < before
        })?;
    }
    list_displays();
    Ok(())
}

#[cfg(windows)]
fn cycle(times: u32) -> anyhow::Result<()> {
    // Starts from off, so each round measures a real appearance.
    disable()?;
    for i in 1..=times {
        println!("--- ciclo {i}/{times}");
        enable()?;
        capture(None)?;
        disable()?;
        std::thread::sleep(Duration::from_secs(1));
    }
    println!("--- fim: {times} ciclos");
    Ok(())
}

/// The display's resolutions (GDI: what Windows itself offers in Settings).
#[cfg(windows)]
#[allow(unsafe_code)] // GDI
mod gdi {
    use windows::Win32::Graphics::Gdi::{
        CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
        DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS,
        ENUM_DISPLAY_SETTINGS_MODE, EnumDisplaySettingsW,
    };
    use windows::core::HSTRING;

    fn mode(device: &HSTRING, n: ENUM_DISPLAY_SETTINGS_MODE) -> Option<DEVMODEW> {
        let mut mode = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        unsafe { EnumDisplaySettingsW(device, n, &mut mode) }
            .as_bool()
            .then_some(mode)
    }

    pub fn modes(id: &str) -> anyhow::Result<()> {
        let device = HSTRING::from(id);
        if let Some(m) = mode(&device, ENUM_CURRENT_SETTINGS) {
            println!(
                "{id}: atual {}x{} a {} Hz",
                m.dmPelsWidth, m.dmPelsHeight, m.dmDisplayFrequency
            );
        }
        let mut seen = Vec::new();
        for n in 0.. {
            let Some(m) = mode(&device, ENUM_DISPLAY_SETTINGS_MODE(n)) else {
                break;
            };
            let key = (m.dmPelsWidth, m.dmPelsHeight, m.dmDisplayFrequency);
            if !seen.contains(&key) {
                seen.push(key);
            }
        }
        println!("{} modos:", seen.len());
        for (w, h, hz) in seen {
            println!("  {w}x{h} a {hz} Hz");
        }
        Ok(())
    }

    pub fn set_resolution(id: &str, width: u32, height: u32) -> anyhow::Result<()> {
        let device = HSTRING::from(id);
        let mut m = mode(&device, ENUM_CURRENT_SETTINGS)
            .ok_or_else(|| anyhow::anyhow!("{id}: sem a configuração atual"))?;
        m.dmPelsWidth = width;
        m.dmPelsHeight = height;
        m.dmDisplayFrequency = 60;
        m.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
        let result =
            unsafe { ChangeDisplaySettingsExW(&device, Some(&m), None, CDS_UPDATEREGISTRY, None) };
        anyhow::ensure!(
            result == DISP_CHANGE_SUCCESSFUL,
            "{id}: o Windows recusou {width}x{height} a 60 Hz (DISP_CHANGE {})",
            result.0
        );
        println!("{id}: agora {width}x{height} a 60 Hz");
        Ok(())
    }
}

#[cfg(windows)]
#[allow(unsafe_code)] // SetupAPI
mod win {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_DEVNODE_STATUS_FLAGS, CM_Get_DevNode_Status, CM_PROB, CR_SUCCESS, DI_FUNCTION,
        DICS_DISABLE, DICS_ENABLE, DICS_FLAG_GLOBAL, DIF_PROPERTYCHANGE, DIGCF_ALLCLASSES,
        HDEVINFO, SETUP_DI_REGISTRY_PROPERTY, SP_CLASSINSTALL_HEADER, SP_DEVINFO_DATA,
        SP_PROPCHANGE_PARAMS, SPDRP_DEVICEDESC, SPDRP_FRIENDLYNAME, SPDRP_HARDWAREID,
        SetupDiCallClassInstaller, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo,
        SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW, SetupDiSetClassInstallParamsW,
    };
    use windows::core::PCWSTR;

    /// The driver's hardware ID (its INF installs `Root\MttVDD`).
    const HARDWARE_ID: &str = "root\\mttvdd";
    /// `CM_PROB_DISABLED`.
    const PROBLEM_DISABLED: u32 = 22;

    struct List(HDEVINFO);

    impl Drop for List {
        fn drop(&mut self) {
            let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) };
        }
    }

    fn property(list: &List, data: &SP_DEVINFO_DATA, prop: SETUP_DI_REGISTRY_PROPERTY) -> String {
        let mut buf = [0u8; 2048];
        if unsafe {
            SetupDiGetDeviceRegistryPropertyW(list.0, data, prop, None, Some(&mut buf), None)
        }
        .is_err()
        {
            return String::new();
        }
        let wide: Vec<u16> = buf
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        // REG_MULTI_SZ: the first string is enough.
        let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
        String::from_utf16_lossy(&wide[..end])
    }

    /// Every device in the system whose hardware ID is the driver's.
    fn devices() -> anyhow::Result<(List, Vec<SP_DEVINFO_DATA>)> {
        let list =
            List(unsafe { SetupDiGetClassDevsW(None, PCWSTR::null(), None, DIGCF_ALLCLASSES) }?);
        let mut found = Vec::new();
        for index in 0.. {
            let mut data = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            if unsafe { SetupDiEnumDeviceInfo(list.0, index, &mut data) }.is_err() {
                break;
            }
            if property(&list, &data, SPDRP_HARDWAREID).to_lowercase() == HARDWARE_ID {
                found.push(data);
            }
        }
        Ok((list, found))
    }

    /// The `CM_PROB_*` of the device, or `None` without a state.
    fn problem(data: &SP_DEVINFO_DATA) -> Option<u32> {
        let mut status = CM_DEVNODE_STATUS_FLAGS(0);
        let mut problem = CM_PROB(0);
        let result = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, data.DevInst, 0) };
        (result == CR_SUCCESS).then_some(problem.0)
    }

    fn state(data: &SP_DEVINFO_DATA) -> String {
        match problem(data) {
            None => "sem estado".into(),
            Some(0) => "habilitado".into(),
            Some(PROBLEM_DISABLED) => "desabilitado".into(),
            Some(other) => format!("com problema {other}"),
        }
    }

    fn installed() -> anyhow::Result<(List, Vec<SP_DEVINFO_DATA>)> {
        let (list, found) = devices()?;
        anyhow::ensure!(
            !found.is_empty(),
            "o driver não está instalado (Root\\MttVDD)"
        );
        Ok((list, found))
    }

    pub fn is_enabled() -> anyhow::Result<bool> {
        let (_list, found) = installed()?;
        Ok(found.iter().any(|data| problem(data) == Some(0)))
    }

    pub fn print_devices() -> anyhow::Result<()> {
        let (list, found) = devices()?;
        if found.is_empty() {
            println!("dispositivo Root\\MttVDD: não instalado");
        }
        for data in &found {
            let mut name = property(&list, data, SPDRP_FRIENDLYNAME);
            if name.is_empty() {
                name = property(&list, data, SPDRP_DEVICEDESC);
            }
            println!("dispositivo: {name} ({})", state(data));
        }
        Ok(())
    }

    pub fn set_enabled(enabled: bool) -> anyhow::Result<()> {
        let (list, found) = installed()?;
        for data in &found {
            let params = SP_PROPCHANGE_PARAMS {
                ClassInstallHeader: SP_CLASSINSTALL_HEADER {
                    cbSize: size_of::<SP_CLASSINSTALL_HEADER>() as u32,
                    InstallFunction: DIF_PROPERTYCHANGE,
                },
                StateChange: if enabled { DICS_ENABLE } else { DICS_DISABLE },
                Scope: DICS_FLAG_GLOBAL,
                HwProfile: 0,
            };
            unsafe {
                SetupDiSetClassInstallParamsW(
                    list.0,
                    Some(data),
                    Some(&params.ClassInstallHeader),
                    size_of::<SP_PROPCHANGE_PARAMS>() as u32,
                )?;
                SetupDiCallClassInstaller(DI_FUNCTION(DIF_PROPERTYCHANGE.0), list.0, Some(data))
                    .map_err(|e| anyhow::anyhow!("{e} (rode como administrador)"))?;
            }
        }
        Ok(())
    }
}
