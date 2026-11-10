//! Spike do monitor virtual (docs/monitor-virtual.md, marco 0), Windows.
//!
//! Liga e desliga o dispositivo do Virtual Display Driver (`Root\MttVDD`)
//! como o serviço do Hext faria (SetupAPI, sem o pipe do driver), mede quanto
//! o monitor leva para aparecer na lista do DXGI e captura nele.
//!
//!   vdd-spike status
//!   vdd-spike enable        (administrador)
//!   vdd-spike disable       (administrador)
//!   vdd-spike capture [N]   (monitor N da lista; padrão: o último)
//!   vdd-spike cycle [N]     (N vezes: liga, captura, desliga; administrador)

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).map(String::as_str);
    match arg(0).unwrap_or("status") {
        "status" => status(),
        "capture" => capture(arg(1).map(str::parse).transpose()?),
        #[cfg(windows)]
        "enable" => enable(),
        #[cfg(windows)]
        "disable" => win::set_enabled(false),
        #[cfg(windows)]
        "cycle" => cycle(arg(1).map(str::parse).transpose()?.unwrap_or(5)),
        other => {
            anyhow::bail!("comando desconhecido: {other} (status, enable, disable, capture, cycle)")
        }
    }
}

use std::time::{Duration, Instant};

/// How long a display may take to show up after the device is enabled.
#[cfg(windows)]
const APPEAR_WAIT: Duration = Duration::from_secs(20);

fn status() -> anyhow::Result<()> {
    #[cfg(windows)]
    win::print_devices()?;
    list_displays()?;
    Ok(())
}

fn list_displays() -> anyhow::Result<Vec<hext_capture::Display>> {
    let displays = hext_capture::displays(false).unwrap_or_else(|e| {
        println!("monitores: erro ({e})");
        Vec::new()
    });
    println!("monitores no DXGI: {}", displays.len());
    for (i, d) in displays.iter().enumerate() {
        println!(
            "  {i}: {}  {}x{} em ({}, {})",
            d.id, d.rect.width, d.rect.height, d.rect.x, d.rect.y
        );
    }
    Ok(displays)
}

/// Opens display `index` (default: the last) and counts frames for 3 s.
fn capture(index: Option<usize>) -> anyhow::Result<()> {
    let displays = list_displays()?;
    let index = index.unwrap_or(displays.len().saturating_sub(1));
    let display = displays
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("não há monitor {index}"))?;
    let start = Instant::now();
    let mut capturer = hext_capture::open_display(display, false, 60)?;
    println!("captura aberta em {:?}", start.elapsed());
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

#[cfg(windows)]
fn enable() -> anyhow::Result<()> {
    let before = hext_capture::displays(false).map(|d| d.len()).unwrap_or(0);
    let start = Instant::now();
    win::set_enabled(true)?;
    println!("dispositivo habilitado em {:?}", start.elapsed());
    wait_for_more_than(before, start)?;
    list_displays()?;
    Ok(())
}

#[cfg(windows)]
fn wait_for_more_than(before: usize, start: Instant) -> anyhow::Result<()> {
    while start.elapsed() < APPEAR_WAIT {
        let now = hext_capture::displays(false).map(|d| d.len()).unwrap_or(0);
        if now > before {
            println!("monitor apareceu no DXGI em {:?}", start.elapsed());
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!("o monitor não apareceu no DXGI em {APPEAR_WAIT:?}")
}

#[cfg(windows)]
fn cycle(times: u32) -> anyhow::Result<()> {
    for i in 1..=times {
        println!("--- ciclo {i}/{times}");
        let before = hext_capture::displays(false).map(|d| d.len()).unwrap_or(0);
        let start = Instant::now();
        win::set_enabled(true)?;
        wait_for_more_than(before, start)?;
        capture(None)?;
        win::set_enabled(false)?;
        std::thread::sleep(Duration::from_secs(1));
    }
    println!("--- fim");
    list_displays()?;
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)] // SetupAPI
mod win {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_DEVNODE_STATUS_FLAGS, CM_Get_DevNode_Status, CM_PROB, CR_SUCCESS, DI_FUNCTION,
        DICS_DISABLE, DICS_ENABLE, DICS_FLAG_GLOBAL, DIF_PROPERTYCHANGE, DIGCF_ALLCLASSES,
        HDEVINFO, SETUP_DI_REGISTRY_PROPERTY, SP_CLASSINSTALL_HEADER, SP_DEVINFO_DATA,
        SP_PROPCHANGE_PARAMS, SPDRP_FRIENDLYNAME, SPDRP_HARDWAREID, SetupDiCallClassInstaller,
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
        SetupDiGetDeviceRegistryPropertyW, SetupDiSetClassInstallParamsW,
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

    fn state(data: &SP_DEVINFO_DATA) -> String {
        let mut status = CM_DEVNODE_STATUS_FLAGS(0);
        let mut problem = CM_PROB(0);
        let result = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, data.DevInst, 0) };
        if result != CR_SUCCESS {
            return format!("sem estado (CONFIGRET {})", result.0);
        }
        match problem.0 {
            0 => "habilitado".into(),
            PROBLEM_DISABLED => "desabilitado".into(),
            other => format!("com problema {other}"),
        }
    }

    pub fn print_devices() -> anyhow::Result<()> {
        let (list, found) = devices()?;
        if found.is_empty() {
            println!("dispositivo Root\\MttVDD: não instalado");
        }
        for data in &found {
            println!(
                "dispositivo: {} ({})",
                property(&list, data, SPDRP_FRIENDLYNAME),
                state(data)
            );
        }
        Ok(())
    }

    pub fn set_enabled(enabled: bool) -> anyhow::Result<()> {
        let (list, found) = devices()?;
        anyhow::ensure!(
            !found.is_empty(),
            "o driver não está instalado (Root\\MttVDD)"
        );
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
            println!(
                "dispositivo {}: {}",
                if enabled {
                    "habilitado"
                } else {
                    "desabilitado"
                },
                state(data)
            );
        }
        Ok(())
    }
}
