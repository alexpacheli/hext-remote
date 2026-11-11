//! The Windows service (`HextRemote`): runs as SYSTEM from boot, keeps the
//! computer online with its own ID and owns the sessions; an agent in the
//! console session (relaunched when that session changes) captures and
//! injects input there. As tested in the secure-desktop spike. The interface
//! the user sees is a third process, in that same session, running as the
//! user (see [`crate::ui`]).
#![allow(unsafe_code)] // Win32 calls: services, tokens, processes, pipes

use std::ffi::c_void;
use std::future::Future;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};

use super::access::{Access, MIN_PASSWORD, Password};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use hext_codec::EncoderSettings;
use hext_crypto::pake::Verifier;
use hext_crypto::{FileKeyStore, load_or_create, machine_dir};
use hext_net::EndpointConfig;
use hext_proto::ChatText;
use hext_proto::{PeerInfo, Permissions};
use hext_session::files::{FileAccess, Transfer};
use hext_session::host::{
    ActiveSession, BoxFuture, HostConfig, Presence, Restart, UnattendedAccess,
};
use hext_session::{KeptResume, Lockout, ResumeTokens, SessionCodes};
use hext_signal::DeviceId;
use ring::rand::{SecureRandom, SystemRandom};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
use tokio::sync::{Notify, Semaphore};
use tracing::{info, warn};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_SERVICE_SPECIFIC_ERROR, HANDLE, HLOCAL, LUID,
    LocalFree, WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, DuplicateTokenEx, GetAce,
    GetTokenInformation, INHERIT_ONLY_ACE, ImpersonateLoggedOnUser, IsWellKnownSid,
    LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    PSID, RevertToSelf, SE_SHUTDOWN_NAME, SECURITY_ATTRIBUTES, SecurityImpersonation,
    SetTokenInformation, TOKEN_ALL_ACCESS, TOKEN_PRIVILEGES, TOKEN_USER, TokenPrimary,
    TokenPrivileges, TokenSessionId, TokenUser, WinBuiltinAdministratorsSid, WinLocalSystemSid,
};
use windows::Win32::Storage::FileSystem::CreateDirectoryW;
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW, SetDefaultDllDirectories,
};
use windows::Win32::System::Pipes::GetNamedPipeClientProcessId;
use windows::Win32::System::RemoteDesktop::{
    WTS_CURRENT_SERVER_HANDLE, WTS_SESSIONSTATE_LOCK, WTSFreeMemory, WTSGetActiveConsoleSessionId,
    WTSINFOEXW, WTSQuerySessionInformationW, WTSQueryUserToken, WTSSendMessageW, WTSSessionInfoEx,
};
use windows::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP,
    SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_RUNNING, SERVICE_START_PENDING,
    SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING,
    SERVICE_STOPPED, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus,
    StartServiceCtrlDispatcherW,
};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, GetCurrentProcess,
    OpenProcessToken, PROCESS_INFORMATION, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};
use windows::Win32::UI::Shell::IsUserAnAdmin;
use windows::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_DEFBUTTON2, MB_ICONQUESTION, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO,
    MESSAGEBOX_RESULT,
};
use windows::core::{PCWSTR, PWSTR, s, w};
use zeroize::Zeroizing;

use crate::agent::{AgentHub, Indicator, Stream};
use crate::signaling::Ask;
use crate::ui_link::UiHub;
use crate::{APP_VERSION, DEFAULT_RELAY, DEFAULT_SERVER};

const SERVICE: &str = "HextRemote";
/// Where the policy that lets services send Ctrl+Alt+Del lives.
const POLICY_KEY: &str = r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";
const SAS_VALUE: &str = "SoftwareSASGeneration";
/// The policy that lets the sign-in screen shut the computer down.
const SHUTDOWN_WITHOUT_LOGON: &str = "shutdownwithoutlogon";
/// The computer's folder: the administrators' (as owner), open only to them
/// and the system, passed on to all inside, nothing inherited from above.
const MACHINE_DIR_SDDL: &str = "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
/// The virtual monitor driver's folder: changed only by the system and the
/// administrators, read by the driver (a service account) and the users.
const VIRTUAL_DISPLAY_DIR_SDDL: &str =
    "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FR;;;LS)(A;OICI;FR;;;AU)";
/// The rights that change a file or folder (or what is in it), or who may.
const WRITE_RIGHTS: u32 = 0x0002 // write data, add file
    | 0x0004 // append, add subfolder
    | 0x0010 // write extended attributes
    | 0x0040 // delete child
    | 0x0100 // write attributes
    | 0x0001_0000 // delete
    | 0x0004_0000 // write DAC
    | 0x0008_0000 // write owner
    | 0x0200_0000 // maximum allowed
    | 0x1000_0000 // generic all
    | 0x4000_0000; // generic write
/// What in the computer's folder lets someone in or says who this computer is:
/// read only when the system or the administrators alone wrote it.
const SENSITIVE: [&str; 6] = [
    "device.key",
    "unattended.key",
    "technicians.json",
    "owners.json",
    "resume.json",
    "lockout.txt",
];

/// Ctrl+Alt+Del, from the system's own `sas.dll`: loaded when needed and from
/// System32 only, never from beside this program (an import would be looked
/// for there first, and the installer runs elevated from Downloads).
fn send_sas() -> Result<()> {
    type SendSas = unsafe extern "system" fn(windows_core::BOOL);
    let module = unsafe { LoadLibraryExW(w!("sas.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
        .context("sas.dll")?;
    let found = unsafe { GetProcAddress(module, s!("SendSAS")) }.context("SendSAS")?;
    // (The signature `sas.h` gives it.)
    let send: SendSas = unsafe { std::mem::transmute(found) };
    unsafe { send(false.into()) };
    Ok(())
}

/// From here on, libraries this process loads by name come from System32
/// only. For the service's commands, which may run elevated from wherever
/// the program was downloaded to (Downloads, with whatever else is there).
/// Not for the interface: WebView2 brings its own.
pub fn harden_dll_search() {
    if let Err(e) = unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) } {
        warn!("SetDefaultDllDirectories: {e}");
    }
}

/// A program of the system's own folder: nothing beside this program (in
/// Downloads, say) or on PATH gets to run in its place, elevated.
fn system_tool(name: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot")
        .filter(|root| !root.is_empty())
        .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    root.join("System32").join(name)
}

/// How long the user has to answer a request (the server gives up at 60 s).
const ANSWER_SECS: u32 = crate::signaling::ANSWER_TIME.as_secs() as u32;

/// Where the computer's ID is written for `service status`.
fn id_file() -> Result<PathBuf> {
    Ok(machine_dir()?.join("id.txt"))
}

/// The unattended access: the password, the technicians and the account
/// binding, in the computer's folder, which only the system and the
/// administrators reach: nobody else can put a key of their own there.
fn access() -> Result<Access> {
    Ok(Access::new(machine_dir()?, audit).trusting(trusted_file))
}

fn technicians_store() -> Result<crate::technicians::Technicians> {
    Ok(access()?.technicians())
}

/// Where the installed program lives.
fn program_dir() -> Result<PathBuf> {
    let base = std::env::var_os("ProgramFiles").context("sem %ProgramFiles%")?;
    Ok(PathBuf::from(base).join("Hext Remote"))
}

// ---------------------------------------------------------------- commands

fn sc(args: &[&str]) -> Result<String> {
    // (No console of its own: the interface has none to share.)
    let out = Command::new(system_tool("sc.exe"))
        .args(args)
        .creation_flags(CREATE_NO_WINDOW.0)
        .output()
        .context("sc.exe")?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(text)
    } else {
        bail!("sc {}: {}", args.join(" "), text.trim())
    }
}

fn exists() -> bool {
    sc(&["query", SERVICE]).is_ok()
}

pub fn installed() -> bool {
    exists()
}

/// Stops the service and waits until it has.
fn stop() {
    if sc(&["stop", SERVICE]).is_err() {
        return; // not running
    }
    for _ in 0..40 {
        match sc(&["query", SERVICE]) {
            Ok(state) if !state.contains("STOPPED") => {
                std::thread::sleep(Duration::from_millis(500));
            }
            _ => return,
        }
    }
}

fn require_admin() -> Result<()> {
    if !unsafe { IsUserAnAdmin() }.as_bool() {
        bail!("rode como administrador (terminal aberto com \"Executar como administrador\")");
    }
    Ok(())
}

fn icacls(args: &[&str]) -> Result<()> {
    let out = Command::new(system_tool("icacls.exe"))
        .args(args)
        .output()
        .context("icacls")?;
    if !out.status.success() {
        bail!(
            "icacls {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout).trim()
        );
    }
    Ok(())
}

/// Whether `sid` is the system or the administrators.
fn is_admins(sid: PSID) -> bool {
    unsafe {
        IsWellKnownSid(sid, WinLocalSystemSid).as_bool()
            || IsWellKnownSid(sid, WinBuiltinAdministratorsSid).as_bool()
    }
}

/// Who may change a file or folder, as far as trusting it goes.
struct Trust {
    /// It belongs to the system or the administrators.
    owner: bool,
    /// Nobody else may change it.
    writers: bool,
}

/// The [`Trust`] of `path`.
fn trust_of(path: &Path) -> Result<Trust> {
    /// Freed with its owner.
    struct Descriptor(PSECURITY_DESCRIPTOR);
    impl Drop for Descriptor {
        fn drop(&mut self) {
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.0.0)));
            }
        }
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut owner = PSID::default();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let status = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut dacl),
            None,
            &mut descriptor,
        )
    };
    if status.is_err() {
        bail!("segurança de {}: {status:?}", path.display());
    }
    let descriptor = Descriptor(descriptor);
    let owner = is_admins(owner);
    // (No list at all lets everyone in.)
    let mut writers = !dacl.is_null();
    let count = if dacl.is_null() {
        0
    } else {
        unsafe { (*dacl).AceCount }
    };
    for index in 0..u32::from(count) {
        let mut ace: *mut c_void = std::ptr::null_mut();
        unsafe { GetAce(dacl, index, &mut ace) }?;
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if u32::from(header.AceFlags) & INHERIT_ONLY_ACE.0 != 0 {
            continue; // only for what is created inside
        }
        match header.AceType {
            // Allowed: to whom, and what.
            0 => {
                let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
                let sid = PSID((&raw const allowed.SidStart).cast_mut().cast());
                if allowed.Mask & WRITE_RIGHTS != 0 && !is_admins(sid) {
                    writers = false;
                }
            }
            // Denied: takes nothing away from trusting it.
            1 => {}
            // Anything else (conditional, object): not something this
            // program writes.
            _ => writers = false,
        }
    }
    drop(descriptor);
    Ok(Trust { owner, writers })
}

/// Whether `path` belongs to the system or the administrators (not to an
/// account that could have put a key of its own there).
fn owned_by_admins(path: &Path) -> Result<bool> {
    Ok(trust_of(path)?.owner)
}

/// Whether the file at `path` may be believed: the system's or the
/// administrators', and nobody else may change it. Anything else could have
/// been put there, or changed, by another account of this computer.
fn trusted_file(path: &Path) -> bool {
    match trust_of(path) {
        Ok(trust) => trust.owner && trust.writers,
        Err(e) => {
            warn!("{e:#}");
            false
        }
    }
}

/// Makes `dir` closed from the start (see [`MACHINE_DIR_SDDL`]): there is no
/// moment in which another account could put something in it. Whether it is
/// new (`false`: it was there already).
fn create_locked_dir(dir: &Path) -> Result<bool> {
    create_dir_with(dir, MACHINE_DIR_SDDL)
}

/// Makes `dir` with these rights from the start; whether it is new.
fn create_dir_with(dir: &Path, sddl: &str) -> Result<bool> {
    let sddl: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
    let made = unsafe { CreateDirectoryW(PCWSTR(wide.as_ptr()), Some(&raw const attributes)) };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
    }
    match made {
        Ok(()) => Ok(true),
        Err(e) if e.code() == ERROR_ALREADY_EXISTS.to_hresult() => Ok(false),
        Err(e) => Err(e).with_context(|| dir.display().to_string()),
    }
}

/// Takes out of `dir` what the system or the administrators did not put
/// there (`strict`: or that someone else may change), before anything in it
/// is believed. Whether all of it went.
fn remove_foreign(dir: &Path, strict: bool) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return true;
    };
    let mut clean = true;
    for entry in entries.flatten() {
        let path = entry.path();
        let foreign = match trust_of(&path) {
            Ok(trust) => !trust.owner || (strict && !trust.writers),
            Err(_) => true,
        };
        if !foreign {
            continue;
        }
        // A link goes, not what it leads to.
        let removed = match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                std::fs::remove_dir(&path).or_else(|_| std::fs::remove_file(&path))
            }
            Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&path),
            _ => std::fs::remove_file(&path),
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        match removed {
            Ok(()) => {
                warn!("{}: de origem desconhecida, removido", path.display());
                audit(&format!(
                    "arquivo de origem desconhecida removido da pasta do computador: {name}"
                ));
            }
            Err(e) => {
                warn!(
                    "{}: de origem desconhecida, e não saiu: {e}",
                    path.display()
                );
                audit(&format!(
                    "arquivo de origem desconhecida na pasta do computador não pôde ser removido: \
                     {name}"
                ));
                clean = false;
            }
        }
    }
    clean
}

/// The computer's folder (the key, the ID and the logs), open only to the
/// system and the administrators: the key is sealed for the machine, so
/// any account that could read the file could use it.
fn prepare_machine_dir() -> Result<PathBuf> {
    let data = machine_dir()?;
    if data.exists() && !owned_by_admins(&data)? {
        // Someone else made it (before any install): nothing in it is ours.
        let aside = data.with_file_name(format!(
            "HextRemote.desconhecida-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs())
        ));
        std::fs::rename(&data, &aside).with_context(|| data.display().to_string())?;
        println!(
            "{} não foi criada pelo Hext Remote: movida para {}",
            data.display(),
            aside.display()
        );
    }
    if create_locked_dir(&data)? {
        // Closed since it was made: anything in it was not put there by us.
        if std::fs::read_dir(&data)?.next().is_some() {
            bail!(
                "{} apareceu com arquivos que não são do Hext Remote; nada foi instalado",
                data.display()
            );
        }
        return Ok(data);
    }
    lock_existing_dir(&data)?;
    Ok(data)
}

/// Closes a computer's folder already there (from an earlier install, which
/// may have left it open for a moment), after taking out what another account
/// put in it: taking ownership of all inside would hide who made it.
fn lock_existing_dir(data: &Path) -> Result<()> {
    if !remove_foreign(data, false) {
        bail!(
            "{} tem arquivos de outra conta que não puderam ser removidos (em uso?); feche os \
             programas ou reinicie, e tente de novo",
            data.display()
        );
    }
    let folder = data.display().to_string();
    icacls(&[&folder, "/setowner", "*S-1-5-32-544", "/T", "/C", "/Q"])?;
    // The folder alone: only the system and the administrators, passed on
    // to what is inside (the inheritance flags mean nothing on a file).
    icacls(&[
        &folder,
        "/inheritance:r",
        "/grant:r",
        "*S-1-5-18:(OI)(CI)F",
        "*S-1-5-32-544:(OI)(CI)F",
        "/Q",
    ])?;
    // What is already inside (the key of an earlier install) inherits
    // exactly that, and nothing of its own.
    if std::fs::read_dir(data)?.next().is_some() {
        let inside = data.join("*").display().to_string();
        icacls(&[&inside, "/reset", "/T", "/C", "/Q"])?;
    }
    Ok(())
}

/// At the service's start: the computer's folder closed, and nothing in it
/// believed that another account put or may change there. A key or binding
/// of someone else's that does not go is an error: better no service than
/// one that is someone else's.
fn check_machine_dir() -> Result<()> {
    let data = machine_dir()?;
    if !trusted_file(&data) {
        warn!("{}: aberta a outras contas; fechando", data.display());
        audit("a pasta do computador estava aberta a outras contas: fechada");
        lock_existing_dir(&data)?;
    }
    remove_foreign(&data, true);
    for name in SENSITIVE {
        let path = data.join(name);
        if path.exists() && !trusted_file(&path) {
            bail!(
                "{}: de origem desconhecida, e não saiu; o serviço não inicia com ele",
                path.display()
            );
        }
    }
    Ok(())
}

/// At the service's start: the virtual monitor a session left on goes off
/// (the service or the agent stopped in the middle), and, where the driver
/// is installed, its settings are the Hext's (docs/monitor-virtual.md),
/// unless there are some already.
fn prepare_virtual_display() {
    use hext_capture::virtual_display;
    virtual_display::turn_off_leftover();
    let Some((path, xml)) = virtual_display::settings() else {
        return;
    };
    if path.exists() || !virtual_display::available() {
        return;
    }
    let Some(dir) = path.parent() else { return };
    let written = create_dir_with(dir, VIRTUAL_DISPLAY_DIR_SDDL).and_then(|new| {
        // Someone else's folder: what goes in it, they could change.
        if !new && !trusted_file(dir) {
            bail!(
                "{}: a pasta não é do Hext; o arquivo fica de fora",
                dir.display()
            );
        }
        std::fs::write(&path, xml).with_context(|| path.display().to_string())
    });
    match written {
        Ok(()) => info!(
            "monitor virtual: configuração gravada em {}",
            path.display()
        ),
        Err(e) => warn!("monitor virtual: {e:#}"),
    }
}

pub fn install(name: Option<String>) -> Result<()> {
    require_admin()?;
    let data = prepare_machine_dir()?;
    if let Some(name) = name {
        crate::name::write(&data, Some(&name))?;
    }

    // The program, where the service runs it from (an update replaces it).
    let installed = exists();
    if installed {
        println!("Atualizando o serviço...");
        stop();
    }
    let dir = program_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| dir.display().to_string())?;
    let exe = dir.join("HextRemote.exe");
    let current = std::env::current_exe()?;
    if !same_file(&current, &exe)
        && let Err(e) = replace_exe(&current, &exe)
    {
        if installed {
            let _ = sc(&["start", SERVICE]); // the version there, not none
        }
        return Err(e);
    }

    let bin_path = format!("\"{}\" service run", exe.display());
    if installed {
        sc(&["config", SERVICE, "binPath=", &bin_path, "start=", "auto"])?;
    } else {
        sc(&[
            "create",
            SERVICE,
            "binPath=",
            &bin_path,
            "start=",
            "auto",
            "DisplayName=",
            "Hext Remote",
        ])?;
    }
    sc(&[
        "description",
        SERVICE,
        "Mantém este computador online com o seu ID do Hext Remote, para receber pedidos de acesso.",
    ])?;
    // Restarted if it ever fails, also when it stops with an error.
    sc(&[
        "failure",
        SERVICE,
        "reset=",
        "86400",
        "actions=",
        "restart/5000/restart/5000/restart/60000",
    ])?;
    sc(&["failureflag", SERVICE, "1"])?;
    if let Err(e) = allow_secure_attention() {
        println!("Não deu para liberar o Ctrl+Alt+Del remoto: {e:#}");
    }
    let _ = std::fs::remove_file(id_file()?);
    sc(&["start", SERVICE])?;

    println!("Serviço instalado e iniciado.");
    for _ in 0..30 {
        if let Ok(id) = std::fs::read_to_string(id_file()?) {
            println!("ID deste computador: {}", id.trim());
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    println!(
        "Ainda sem ID: o serviço não conseguiu entrar no servidor. Veja {}",
        data.join("service.log").display()
    );
    Ok(())
}

/// Puts the program at `exe`. A viewer the user opened may be running from
/// there, which the service does not stop: a running program cannot be
/// overwritten, but it can be moved out of the way.
fn replace_exe(new: &Path, exe: &Path) -> Result<()> {
    let old = exe.with_extension("old.exe");
    let _ = std::fs::remove_file(&old); // from the last update
    if std::fs::copy(new, exe).is_ok() {
        return Ok(());
    }
    std::fs::rename(exe, &old).with_context(|| format!("mover {}", exe.display()))?;
    std::fs::copy(new, exe).with_context(|| format!("copiar para {}", exe.display()))?;
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The policy value that lets programs send Ctrl+Alt+Del (bit 1:
/// services), if set.
fn sas_policy() -> Option<u32> {
    policy(SAS_VALUE)
}

/// The value `name` of the system's policies, if set (a number).
fn policy(name: &str) -> Option<u32> {
    let out = Command::new(system_tool("reg.exe"))
        .args(["query", POLICY_KEY, "/v", name])
        .creation_flags(CREATE_NO_WINDOW.0)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let hex = text.split_whitespace().find_map(|w| w.strip_prefix("0x"))?;
    u32::from_str_radix(hex, 16).ok()
}

fn set_sas_policy(value: Option<u32>) -> Result<()> {
    let value = value.map(|v| v.to_string());
    let args: Vec<&str> = match &value {
        Some(v) => vec![
            "add",
            POLICY_KEY,
            "/v",
            SAS_VALUE,
            "/t",
            "REG_DWORD",
            "/d",
            v,
            "/f",
        ],
        None => vec!["delete", POLICY_KEY, "/v", SAS_VALUE, "/f"],
    };
    let out = Command::new(system_tool("reg.exe"))
        .args(&args)
        .output()
        .context("reg")?;
    if !out.status.success() {
        bail!("reg {}", args.join(" "));
    }
    Ok(())
}

/// Where the policy value from before the install is kept, to put back.
fn sas_backup() -> Result<PathBuf> {
    Ok(machine_dir()?.join("sas-policy-before.txt"))
}

/// Lets services send Ctrl+Alt+Del, keeping what else the policy allowed
/// (and remembering it, for the uninstall).
fn allow_secure_attention() -> Result<()> {
    let before = sas_policy();
    if before.is_some_and(|v| v & 1 != 0) {
        return Ok(());
    }
    let backup = sas_backup()?;
    if !backup.exists() {
        let text = before.map_or_else(|| "none".to_owned(), |v| v.to_string());
        std::fs::write(&backup, text).with_context(|| backup.display().to_string())?;
    }
    set_sas_policy(Some(before.unwrap_or(0) | 1))
}

/// Puts the policy back as it was before the install.
fn restore_secure_attention() {
    let Ok(backup) = sas_backup() else { return };
    // (What another account wrote does not set a policy.)
    if backup.exists() && !trusted_file(&backup) {
        return;
    }
    let Ok(text) = std::fs::read_to_string(&backup) else {
        return; // the install did not change it
    };
    let before = text.trim().parse::<u32>().ok();
    if set_sas_policy(before).is_ok() {
        let _ = std::fs::remove_file(backup);
    }
}

pub fn uninstall(purge: bool) -> Result<()> {
    require_admin()?;
    if exists() {
        stop();
        sc(&["delete", SERVICE])?;
        restore_secure_attention();
        println!("Serviço removido.");
    } else {
        println!("O serviço não está instalado.");
    }
    // Unattended access does not come back with a later install.
    if let Ok(access) = access() {
        let file = access.password_file();
        let _ = Verifier::remove(&file);
    }
    let dir = program_dir()?;
    if dir.exists() && std::fs::remove_dir_all(&dir).is_err() {
        println!("Não deu para apagar {} (em uso?)", dir.display());
    }
    if purge {
        let data = machine_dir()?;
        if data.exists() {
            std::fs::remove_dir_all(&data).with_context(|| data.display().to_string())?;
            println!("Identidade deste computador apagada: o ID dele deixou de valer.");
        }
    }
    Ok(())
}

pub fn status() -> Result<()> {
    match sc(&["query", SERVICE]) {
        Ok(state) if state.contains("RUNNING") => println!("Serviço: rodando"),
        Ok(state) if state.contains("STOPPED") => println!("Serviço: parado"),
        Ok(_) => println!("Serviço: iniciando ou parando"),
        Err(_) => {
            println!("Serviço: não instalado (HextRemote service install)");
            return Ok(());
        }
    }
    println!(
        "Nome deste computador: {}",
        crate::name::described(&machine_dir()?)
    );
    match std::fs::read_to_string(id_file()?) {
        Ok(id) => println!("ID deste computador: {}", id.trim()),
        Err(_) => println!("ID: ainda não (ou rode como administrador para ler)"),
    }
    match std::fs::metadata(access()?.password_file()) {
        Ok(_) => {
            println!("Acesso não assistido: ligado");
            let count = technicians_store()?.list().len();
            if count > 0 {
                println!("Técnicos vinculados (entram sem a senha): {count}");
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("Acesso não assistido: desligado");
        }
        Err(_) => println!("Acesso não assistido: rode como administrador para ver"),
    }
    Ok(())
}

/// Shows or changes the name the service gives this computer. With the
/// service, it holds for the whole computer: an administrator's to change.
/// The service restarts to take it (a session on ends).
pub fn name(change: super::NameChange) -> Result<()> {
    if !exists() {
        return super::user_name(change);
    }
    if matches!(change, super::NameChange::Show) {
        println!(
            "Nome deste computador: {}",
            crate::name::described(&machine_dir()?)
        );
        return Ok(());
    }
    require_admin()?;
    let dir = prepare_machine_dir()?;
    let changed = super::apply_name(&dir, change)?;
    if changed && sc(&["query", SERVICE]).is_ok_and(|state| state.contains("RUNNING")) {
        println!("Reiniciando o serviço com o nome novo (uma sessão em andamento cai)...");
        stop();
        sc(&["start", SERVICE])?;
    }
    println!("Nome deste computador: {}", crate::name::described(&dir));
    Ok(())
}

/// Turns unattended access on (with a new password) or `off`. It takes an
/// administrator, because it holds for the whole computer. The service reads
/// the file on each request: nothing to restart. `pipe`: started by the app
/// (`password_from_app`), which sends the password typed in its own screen.
pub fn password(off: bool, pipe: Option<&str>) -> Result<()> {
    require_admin()?;
    if !exists() {
        bail!("instale o serviço primeiro (HextRemote service install)");
    }
    let (access, owners) = (access()?, owners()?);
    if off {
        access.password_off(&owners)?;
        println!("Acesso não assistido desligado.");
        return Ok(());
    }
    let password = match pipe {
        Some(name) => password_from_pipe(name)?,
        None => {
            let password = Zeroizing::new(rpassword::prompt_password("Nova senha: ")?);
            if password.chars().count() < MIN_PASSWORD {
                bail!("a senha precisa de ao menos {MIN_PASSWORD} caracteres");
            }
            let again = Zeroizing::new(rpassword::prompt_password("Repita a senha: ")?);
            if *password != *again {
                bail!("as senhas não são iguais");
            }
            password
        }
    };
    access.set_password(&password, &owners)?;
    println!("Acesso não assistido ligado.");
    println!("Durante a sessão, este computador mostra quem está conectado.");
    Ok(())
}

/// Turns the password off from the app: Windows asks for an administrator.
pub async fn password_off_from_app() -> Result<()> {
    let code =
        tokio::task::spawn_blocking(|| crate::ui::run_elevated_wait("service password --off"))
            .await?
            .map_err(|_| anyhow::anyhow!("Nada mudou: é preciso permitir como administrador."))?;
    if code != 0 {
        bail!("A senha não mudou.");
    }
    Ok(())
}

/// The start of the pipes the app sends a password through.
const PASSWORD_PIPE: &str = r"\\.\pipe\hext-password-";

/// The password the app sends through `name` (`password_from_app`).
fn password_from_pipe(name: &str) -> Result<Zeroizing<String>> {
    use std::io::Read;
    // Only a pipe of ours: this reads what comes, as administrator.
    let ours = name
        .strip_prefix(PASSWORD_PIPE)
        .is_some_and(|nonce| nonce.len() == 32 && nonce.bytes().all(|b| b.is_ascii_hexdigit()));
    if !ours {
        bail!("canal inválido");
    }
    let mut bytes = Zeroizing::new(Vec::new());
    std::fs::File::open(name)
        .context("o canal do app")?
        .take(4096)
        .read_to_end(&mut bytes)?;
    let password = Zeroizing::new(
        String::from_utf8(std::mem::take(&mut *bytes)).context("a senha não é texto")?,
    );
    if password.chars().count() < MIN_PASSWORD {
        bail!("a senha precisa de ao menos {MIN_PASSWORD} caracteres");
    }
    Ok(password)
}

/// Sets the password typed in the app's own screen. Windows asks for an
/// administrator, and `service password --pipe` runs as one, hidden: the
/// password goes to it through a pipe that only administrators and the
/// system open, and only that process may be on the line. Nothing on the
/// command line, nothing on disk.
#[allow(unsafe_code)] // Win32 calls, on a handle this function owns
pub async fn password_from_app(password: &str) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    use windows::Win32::System::Threading::{GetExitCodeProcess, GetProcessId, INFINITE};
    use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    use windows::core::HSTRING;

    struct Elevated(HANDLE);
    // The handle is only a number to the kernel.
    unsafe impl Send for Elevated {}
    impl Drop for Elevated {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    if password.chars().count() < MIN_PASSWORD {
        bail!("A senha precisa de ao menos {MIN_PASSWORD} caracteres.");
    }
    let name = pipe_name("password")?;
    let mut pipe = restricted_pipe(&name, "D:P(A;;GA;;;SY)(A;;GA;;;BA)")?;
    // (Off the async threads: it waits while Windows asks.)
    let command = format!("service password --pipe {name}");
    let elevated = tokio::task::spawn_blocking(move || {
        let exe = HSTRING::from(std::env::current_exe()?.as_os_str());
        let params = HSTRING::from(command);
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: w!("runas"),
            lpFile: PCWSTR(exe.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        // (It fails, most often, because the user said no.)
        if unsafe { ShellExecuteExW(&mut info) }.is_err() || info.hProcess.is_invalid() {
            bail!("Nada mudou: é preciso permitir como administrador.");
        }
        anyhow::Ok(Elevated(info.hProcess))
    })
    .await??;
    let pid = unsafe { GetProcessId(elevated.0) };
    let sent = async {
        wait_for(&pipe, pid).await?;
        pipe.write_all(password.as_bytes()).await?;
        pipe.flush().await?;
        anyhow::Ok(())
    }
    .await;
    drop(pipe);
    let code = tokio::task::spawn_blocking(move || {
        // (The whole handle, not its field: it is the one sent.)
        let elevated = elevated;
        let mut code = 1;
        unsafe {
            WaitForSingleObject(elevated.0, INFINITE);
            let _ = GetExitCodeProcess(elevated.0, &mut code);
        }
        code
    })
    .await?;
    if let Err(e) = sent {
        warn!("senha pelo app: {e:#}");
    }
    if code != 0 {
        bail!("A senha não mudou.");
    }
    Ok(())
}

/// Lists the technicians trusted without the password, or revokes them.
pub fn technicians(revoke: Option<&str>, revoke_all: bool) -> Result<()> {
    require_admin()?;
    access()?.technicians_command(revoke, revoke_all)
}

/// A line in the audit log, for the owner of the computer.
fn audit(entry: &str) {
    super::kept::audit_to(super::log_file("audit"), entry);
    if let Some(activity) = ACTIVITY.get() {
        let _ = activity.send(entry.to_owned());
    }
}

/// Where the service sends what the audit writes, for the account's
/// Atividade screen (unset outside the service).
static ACTIVITY: OnceLock<tokio::sync::mpsc::UnboundedSender<String>> = OnceLock::new();

/// This computer's binding to an account, as this computer's key sees it.
fn owners() -> Result<crate::owners::Owners> {
    let key = load_or_create(&FileKeyStore::machine(machine_dir()?.join("device.key")))?.0;
    Ok(access()?.owners(Arc::new(key)))
}

/// Binds this computer to the account with list `chain`, unbinds it
/// (`off`), or shows the binding. The service reads the file on each
/// request: nothing to restart.
pub fn account(chain: Option<&str>, off: bool) -> Result<()> {
    if chain.is_none() && !off {
        Access::show_account(&owners()?);
        return Ok(());
    }
    require_admin()?;
    if !exists() {
        bail!("instale o serviço primeiro (HextRemote service install)");
    }
    let owners = owners()?;
    if off {
        access()?.unbind_account(&owners)?;
        println!("Conta desvinculada: todo acesso volta a pedir aceite.");
        return Ok(());
    }
    access()?.bind_account(&owners, chain.unwrap_or_default())
}

/// Takes back the token kept before a restart, if it is still good: kept by
/// the system, and the way its viewer came in (the password, the technician)
/// still holds.
fn restore_resume(resumes: &ResumeTokens, access: &Access) {
    let Ok(dir) = machine_dir() else { return };
    let file = super::kept::kept_file(&dir);
    if file.exists() && !trusted_file(&file) {
        warn!("{}: de origem desconhecida, ignorado", file.display());
        super::kept::revoke(&dir);
        return;
    }
    super::kept::restore_if(&dir, resumes, |kept, password| {
        access.still(&kept.viewer, &kept.grant, password)
    });
}

/// Whether the user at this computer could restart it themselves: the
/// service restarts as the system, which must not be a way around that. With
/// nobody signed in, as the sign-in screen allows.
fn console_may_restart() -> std::result::Result<(), String> {
    let refused = "quem usa este computador não tem permissão para reiniciá-lo".to_owned();
    match UserToken::of(unsafe { WTSGetActiveConsoleSessionId() }) {
        Some(user) => match user.holds(SE_SHUTDOWN_NAME) {
            Ok(true) => Ok(()),
            Ok(false) => Err(refused),
            Err(e) => Err(format!("permissão para reiniciar: {e:#}")),
        },
        None if policy(SHUTDOWN_WITHOUT_LOGON) == Some(0) => Err(refused),
        None => Ok(()),
    }
}

/// Restarts this computer, with the token waiting on the other side.
struct MachineRestart(Access);

impl Restart for MachineRestart {
    fn restart(&self, kept: &KeptResume, who: &str) -> std::result::Result<(), String> {
        if let Err(e) = console_may_restart() {
            audit(&format!("reinício pedido por {who} recusado: {e}"));
            return Err(e);
        }
        let dir = machine_dir().map_err(|e| e.to_string())?;
        let password = self.0.verifier().map(|verifier| *verifier.salt());
        super::kept::keep_with(&dir, kept, password.as_ref())?;
        audit(&format!("reinício pedido: {who}"));
        // From the system's own folder: nothing on PATH gets to run as SYSTEM.
        let done = Command::new(system_tool("shutdown.exe"))
            .args(["/r", "/t", "5", "/d", "p:0:0"])
            .status()
            .map_err(|e| format!("shutdown: {e}"))?;
        if !done.success() {
            self.revoke();
            return Err(format!("shutdown: {done}"));
        }
        Ok(())
    }

    fn revoke(&self) {
        if let Ok(dir) = machine_dir() {
            super::kept::revoke(&dir);
        }
    }
}

/// The session on the bar and, for where the bar cannot be seen, on the
/// agent's indicator.
struct Indicators {
    ui: UiHub,
    agents: AgentHub,
}

impl Presence for Indicators {
    fn started(&self, session: &ActiveSession) -> Arc<Notify> {
        let who = match &session.id {
            Some(id) => format!("{} (ID {id})", session.name),
            None => session.name.clone(),
        };
        // No interface (another copy of the app holds its place, say): the
        // session still shows.
        self.agents.set_indicator(Some(Indicator {
            text: format!("{who} está acessando este computador"),
            everywhere: !self.ui.connected(),
        }));
        self.ui.started(session)
    }

    fn ended(&self) {
        self.agents.set_indicator(None);
        self.ui.ended();
    }

    fn outgoing(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<PathBuf>> {
        self.ui.outgoing()
    }

    fn transfer(&self, transfer: &Transfer) {
        self.ui.transfer(transfer);
    }

    fn chat(&self, text: ChatText) -> bool {
        self.ui.chat(text)
    }

    fn chat_out(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<ChatText>> {
        self.ui.chat_out()
    }
}

/// Files as the user at this computer has them: the service's rights go
/// nowhere near them. That user decides where Downloads leads, and the viewer
/// what the files are called; as SYSTEM, the two together could write
/// anywhere. A file chosen on the bar opens only if that user could open it.
struct UserFiles;

impl UserFiles {
    fn token() -> Option<UserToken> {
        UserToken::of(unsafe { WTSGetActiveConsoleSessionId() })
    }
}

/// `work` with the rights of `token`'s user.
fn as_user_of(token: &UserToken, work: &mut dyn FnMut()) -> std::io::Result<()> {
    /// A thread left as the user would serve the rest of the service
    /// with the wrong rights, and one left as nobody cannot be trusted.
    struct BackToSelf;
    impl Drop for BackToSelf {
        fn drop(&mut self) {
            if unsafe { RevertToSelf() }.is_err() {
                std::process::abort();
            }
        }
    }
    unsafe { ImpersonateLoggedOnUser(token.0) }?;
    let _back = BackToSelf;
    work();
    Ok(())
}

/// [`UserFiles`] for one file: the user it began with.
struct HeldUser {
    token: UserToken,
    downloads: Option<PathBuf>,
}

impl FileAccess for HeldUser {
    fn downloads(&self) -> Option<PathBuf> {
        self.downloads.clone()
    }

    fn as_user(&self, work: &mut dyn FnMut()) -> std::io::Result<()> {
        as_user_of(&self.token, work)
    }

    fn for_one_file(&self) -> Option<Arc<dyn FileAccess>> {
        None
    }
}

impl FileAccess for UserFiles {
    fn downloads(&self) -> Option<PathBuf> {
        crate::downloads_of(Some(Self::token()?.0))
    }

    fn as_user(&self, work: &mut dyn FnMut()) -> std::io::Result<()> {
        let token = Self::token().ok_or(std::io::ErrorKind::NotConnected)?;
        as_user_of(&token, work)
    }

    fn for_one_file(&self) -> Option<Arc<dyn FileAccess>> {
        let token = Self::token()?;
        let downloads = crate::downloads_of(Some(token.0));
        Some(Arc::new(HeldUser { token, downloads }))
    }
}

// ---------------------------------------------------------------- service

/// Wakes the service's work when Windows asks it to stop.
static STOP: Notify = Notify::const_new();
static STATUS: OnceLock<usize> = OnceLock::new();

/// Called by Windows (through `service run`).
pub fn run() -> Result<()> {
    let mut name: Vec<u16> = SERVICE.encode_utf16().chain([0]).collect();
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(name.as_mut_ptr()),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) }
        .context("isto é o serviço, iniciado pelo Windows; use `service install`")?;
    Ok(())
}

fn set_state(state: SERVICE_STATUS_CURRENT_STATE) {
    report(state, false);
}

/// Tells Windows the service's state; `failed`: it stopped on an error
/// (which starts the restart actions).
fn report(state: SERVICE_STATUS_CURRENT_STATE, failed: bool) {
    let Some(&handle) = STATUS.get() else { return };
    let status = SERVICE_STATUS {
        dwWin32ExitCode: if failed {
            ERROR_SERVICE_SPECIFIC_ERROR.0
        } else {
            0
        },
        dwServiceSpecificExitCode: u32::from(failed),
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        dwWaitHint: 10_000,
        ..Default::default()
    };
    let _ = unsafe { SetServiceStatus(SERVICE_STATUS_HANDLE(handle as *mut c_void), &status) };
}

unsafe extern "system" fn handler(
    control: u32,
    _event: u32,
    _data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    if control == SERVICE_CONTROL_STOP || control == SERVICE_CONTROL_SHUTDOWN {
        set_state(SERVICE_STOP_PENDING);
        STOP.notify_one();
    }
    0
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    let handle =
        match unsafe { RegisterServiceCtrlHandlerExW(w!("HextRemote"), Some(handler), None) } {
            Ok(handle) => handle,
            Err(e) => {
                warn!("RegisterServiceCtrlHandlerExW: {e}");
                return;
            }
        };
    let _ = STATUS.set(handle.0 as usize);
    set_state(SERVICE_START_PENDING);
    let result = tokio::runtime::Runtime::new()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| {
            let result = runtime.block_on(serve());
            // Not waiting for a question still on screen (it times out).
            runtime.shutdown_timeout(Duration::from_secs(2));
            result
        });
    match &result {
        Ok(()) => info!("serviço parado"),
        Err(e) => warn!("serviço parado por erro: {e:#}"),
    }
    report(SERVICE_STOPPED, result.is_err());
}

/// The host, as a service: by ID only (no access code), the user asked
/// through the interface, the screen through the agents.
async fn serve() -> Result<()> {
    info!("serviço iniciado (v{APP_VERSION})");
    check_machine_dir()?;
    prepare_virtual_display();
    let key =
        Arc::new(load_or_create(&FileKeyStore::machine(machine_dir()?.join("device.key")))?.0);
    let name = crate::name::in_dir(&machine_dir()?);
    // Registered before the relay sees this computer (it serves only
    // registered ones); the ID goes to `service status`.
    match tokio::time::timeout(
        Duration::from_secs(10),
        crate::signaling::login(DEFAULT_SERVER, &key, &name),
    )
    .await
    {
        Ok(Ok(client)) => {
            write_id(client.id());
            let _ = client.close().await;
        }
        Ok(Err(e)) => warn!("{e:#}"),
        Err(_) => warn!("servidor de sinalização: sem resposta"),
    }
    let relay = crate::relay(DEFAULT_RELAY)?;
    let endpoint = hext_net::bind(
        &key,
        &EndpointConfig {
            relay: relay.clone(),
            bind: None,
            accept: true,
        },
    )
    .await?;
    set_state(SERVICE_RUNNING);

    // (An install from before milestone 2 did not turn the policy on.)
    if let Err(e) = allow_secure_attention() {
        warn!("Ctrl+Alt+Del remoto: {e:#}");
    }
    let ui = UiHub::default();
    ui.set_key(&key.public_key());
    // (Changed with `service name`, as an administrator.)
    ui.set_name(name.clone(), None);
    let ending = ui.clone();
    let hub = AgentHub::new(
        || {
            if sas_policy().is_some_and(|v| v & 1 != 0) {
                match send_sas() {
                    Ok(()) => info!("Ctrl+Alt+Del enviado"),
                    Err(e) => warn!("Ctrl+Alt+Del: {e:#}"),
                }
            } else {
                warn!("Ctrl+Alt+Del recusado: a política SoftwareSASGeneration não permite");
            }
        },
        move || ending.end_session(),
    );
    let agents = hub.clone();
    tokio::spawn(supervise("agent", move |session| {
        let hub = agents.clone();
        async move { launch(session, &hub).await }
    }));
    let interfaces = ui.clone();
    tokio::spawn(supervise("interface", move |session| {
        let ui = interfaces.clone();
        async move { launch_ui(session, &ui).await }
    }));
    let encoder = EncoderSettings::default();
    let session_codes = SessionCodes::default();
    let resumes = ResumeTokens::default();
    let host_key = key.public_key();
    let access = access()?;
    restore_resume(&resumes, &access);
    tokio::spawn(drop_revoked_tokens(access.clone(), resumes.clone()));
    let owners = access.owners(key.clone());
    let (activity, activity_out) = tokio::sync::mpsc::unbounded_channel();
    let _ = ACTIVITY.set(activity);
    tokio::spawn(crate::owners::report_activity(owners.clone(), activity_out));
    // The technicians, the password and the account, as the command line
    // (or the app's buttons) change them.
    access.show_on(&ui, &owners);
    let (attempts, attempts_out) = tokio::sync::mpsc::unbounded_channel();
    let config = Arc::new(HostConfig {
        owners: Some(Arc::new(owners.clone())),
        peer: PeerInfo::local(APP_VERSION, name.clone()),
        // Only viewers the user accepted, with their one-time codes.
        access_code: None,
        session_codes: session_codes.clone(),
        desktop: Arc::new(hub.desktop(Stream {
            display: 0,
            test_pattern: false,
            encoder,
            virtual_display: false,
        })),
        presence: Some(Arc::new(Indicators {
            ui: ui.clone(),
            agents: hub.clone(),
        })),
        unattended: Some(UnattendedAccess {
            key: host_key,
            // (On disk: restarting the service must not clear the waits.)
            lockout: Lockout::kept_in(machine_dir()?.join("lockout.txt")),
            password: Arc::new(Password {
                access: access.clone(),
                ui: ui.clone(),
                owners: Some(owners.clone()),
            }),
            attempts,
        }),
        resumes: resumes.clone(),
        restart: Some(Arc::new(MachineRestart(access.clone()))),
        files: Some(Arc::new(UserFiles)),
    });
    let slot = Arc::new(Semaphore::new(1));
    tokio::spawn(keep_updated(slot.clone()));
    tokio::spawn(crate::signaling::host(
        DEFAULT_SERVER.to_owned(),
        key,
        name,
        (endpoint.clone(), relay),
        (session_codes, resumes),
        attempts_out,
        slot.clone(),
        Box::new(ConsoleAsk(ui, owners, access)),
    ));
    crate::host::accept(&endpoint, config, slot, STOP.notified()).await;
    // A session still on: the agent stops streaming and releases any held
    // key before the service goes.
    hub.stop();
    tokio::time::sleep(Duration::from_millis(500)).await;
    endpoint.close().await;
    Ok(())
}

/// How often the tokens are checked against the password and the
/// technicians, which the command line changes.
const TOKEN_CHECK: Duration = Duration::from_secs(3);

/// Drops the tokens whose way in no longer holds: a new password (or none)
/// takes those of everyone who came in with the old one; a revoked
/// technician, theirs. Nobody comes back by a way that was closed.
async fn drop_revoked_tokens(access: Access, resumes: ResumeTokens) {
    let mut password = access.verifier().map(|verifier| *verifier.salt());
    let mut tick = tokio::time::interval(TOKEN_CHECK);
    loop {
        tick.tick().await;
        let now = access.verifier().map(|verifier| *verifier.salt());
        let changed = now != password;
        password = now;
        resumes.retain(|kept| {
            let grant = &kept.grant;
            let by_password = grant.needs_password && !grant.bound && grant.via.is_none();
            !(changed && by_password) && access.still(&kept.viewer, grant, None)
        });
    }
}

fn write_id(id: DeviceId) {
    if let Ok(path) = id_file()
        && let Err(e) = std::fs::write(&path, id.to_string())
    {
        warn!("{}: {e}", path.display());
    }
    info!("ID deste computador: {id}");
}

/// Asks whoever is at the console: in the interface, or, where no window of
/// the user can be seen (the sign-in screen, a locked computer), in a
/// message box.
struct ConsoleAsk(UiHub, crate::owners::Owners, Access);

/// How often the service looks for a newer version.
const UPDATE_CHECK: Duration = Duration::from_secs(60 * 60);
/// After a failed install, before the next try.
const UPDATE_RETRY: Duration = Duration::from_secs(6 * 60 * 60);

/// Installs newer versions by itself (F6): the signed `.msi`, between
/// sessions (`slot` held meanwhile: nobody comes in while it installs).
async fn keep_updated(slot: Arc<Semaphore>) {
    if !crate::update::enabled() {
        return;
    }
    tokio::time::sleep(Duration::from_secs(120)).await;
    let mut every = tokio::time::interval(UPDATE_CHECK);
    loop {
        every.tick().await;
        let manifest = match crate::update::check().await {
            Ok(Some(manifest)) => manifest,
            Ok(None) => continue,
            Err(e) => {
                warn!("atualização: {e:#}");
                continue;
            }
        };
        let Ok(_free) = slot.clone().try_acquire_owned() else {
            info!("versão {}: depois da sessão", manifest.version);
            continue;
        };
        // Done, the installer stops this service and starts the new one;
        // failed, this one goes on, and tries again later.
        if let Err(e) = install_update(&manifest).await {
            warn!("atualização para {}: {e:#}", manifest.version);
            audit(&format!(
                "atualização automática para {} falhou: {e:#}",
                manifest.version
            ));
            if let Ok(dir) = machine_dir() {
                let _ = std::fs::remove_dir_all(dir.join("update"));
            }
            drop(_free);
            tokio::time::sleep(UPDATE_RETRY).await;
        }
    }
}

async fn install_update(manifest: &crate::update::Manifest) -> Result<()> {
    // In the computer's folder: only the system and administrators write
    // there, from the download to the installer.
    let dir = machine_dir()?.join("update");
    let msi = crate::update::download(manifest, "windows-msi", &dir).await?;
    audit(&format!(
        "atualização automática: {APP_VERSION} → {}",
        manifest.version
    ));
    let log = dir.join("install.log");
    // On its own, out of this service's job: it stops the service.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let mut command = Command::new(system_tool("msiexec.exe"));
    command
        .arg("/i")
        .arg(&msi)
        .args(["/qn", "/norestart", "/l*v"])
        .arg(&log);
    let mut child = match command
        .creation_flags(DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB)
        .spawn()
    {
        Ok(child) => child,
        Err(_) => command
            .creation_flags(DETACHED_PROCESS)
            .spawn()
            .context("msiexec")?,
    };
    // A success stops this service before the wait ends.
    let waited = tokio::time::timeout(
        Duration::from_secs(15 * 60),
        tokio::task::spawn_blocking(move || child.wait()),
    )
    .await
    .context("msiexec: sem terminar em 15 minutos")???;
    if waited.success() {
        Ok(())
    } else {
        bail!("msiexec: {waited} (veja {})", log.display())
    }
}

impl Ask for ConsoleAsk {
    fn ask<'a>(&'a mut self, name: &'a str, id: DeviceId) -> BoxFuture<'a, Option<Permissions>> {
        Box::pin(async move {
            let session = unsafe { WTSGetActiveConsoleSessionId() };
            let answer = if is_locked(session) {
                None
            } else {
                self.0.ask(name, id)
            };
            match answer {
                Some(answer) => answer.wait().await,
                None => message_box(session, name, id)
                    .await
                    .then_some(Permissions::CONTROL),
            }
        })
    }

    fn show_id(&mut self, id: DeviceId) {
        write_id(id);
        self.0.set_id(id);
    }

    fn unattended(&self) -> bool {
        self.2.verifier().is_some()
    }

    fn bound(&self, key: &hext_crypto::PublicKey) -> bool {
        self.2.technicians().bound(key)
    }

    fn owner<'a>(
        &'a mut self,
        key: &'a hext_crypto::PublicKey,
    ) -> BoxFuture<'a, Option<crate::owners::Way>> {
        Box::pin(super::access::owner_way(&self.1, key))
    }

    fn locked(&self) -> bool {
        console_locked()
    }
}

/// Whether the computer's screen is locked.
pub(super) fn console_locked() -> bool {
    is_locked(unsafe { WTSGetActiveConsoleSessionId() })
}

/// Whether `session` is locked: the user's windows are out of sight, behind
/// the sign-in screen.
fn is_locked(session: u32) -> bool {
    let mut info = PWSTR::null();
    let mut size = 0;
    let found = unsafe {
        WTSQuerySessionInformationW(
            Some(WTS_CURRENT_SERVER_HANDLE),
            session,
            WTSSessionInfoEx,
            &mut info,
            &mut size,
        )
    };
    if found.is_err() {
        return false; // no such session: nothing there is locked
    }
    unsafe {
        let details = &*info.0.cast::<WTSINFOEXW>();
        let locked = details.Level == 1
            && details.Data.WTSInfoExLevel1.SessionFlags as u32 == WTS_SESSIONSTATE_LOCK;
        WTSFreeMemory(info.0.cast());
        locked
    }
}

/// Asks in a message box on `session` (the system draws it, so it needs no
/// process there).
async fn message_box(session: u32, name: &str, id: DeviceId) -> bool {
    let title: Vec<u16> = "Hext Remote".encode_utf16().collect();
    let text: Vec<u16> = format!("\"{name}\" (ID {id}) quer acessar este computador.\n\nAceitar?")
        .encode_utf16()
        .collect();
    let asked = tokio::task::spawn_blocking(move || {
        let mut answer = MESSAGEBOX_RESULT(0);
        // Waits for the answer, or `ANSWER_SECS` (then it is a no). "Não"
        // is the button already chosen: an Enter or a space pressed by
        // someone typing must not hand over the computer.
        let sent = unsafe {
            WTSSendMessageW(
                Some(WTS_CURRENT_SERVER_HANDLE),
                session,
                PCWSTR(title.as_ptr()),
                (title.len() * 2) as u32,
                PCWSTR(text.as_ptr()),
                (text.len() * 2) as u32,
                MB_YESNO | MB_DEFBUTTON2 | MB_ICONQUESTION | MB_SETFOREGROUND | MB_TOPMOST,
                ANSWER_SECS,
                &mut answer,
                true,
            )
        };
        match sent {
            Ok(()) => answer == IDYES,
            Err(e) => {
                warn!("perguntar na sessão {session}: {e}");
                false
            }
        }
    });
    asked.await.unwrap_or(false)
}

// ---------------------------------------------------------------- agents

/// A process handle, closed with its owner.
struct Process(HANDLE);

// The handle is only a number to the kernel.
unsafe impl Send for Process {}

impl Process {
    fn alive(&self) -> bool {
        (unsafe { WaitForSingleObject(self.0, 0) }) != WAIT_OBJECT_0
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            let _ = TerminateProcess(self.0, 0);
            let _ = CloseHandle(self.0);
        }
    }
}

/// A process started in a session, and the connection serving it.
type Running = (Process, tokio::task::JoinHandle<()>);

/// Keeps a process (`what`) running in the console session: a new one when
/// the session changes (a logon, switching users) or the process ends.
/// `launch` starts it; `None`: that session has nothing to run for now.
async fn supervise<F, L>(what: &'static str, launch: L)
where
    L: Fn(u32) -> F,
    F: Future<Output = Result<Option<Running>>>,
{
    let mut current: Option<(u32, Running)> = None;
    // After a failure, waits longer each time (and logs less).
    let mut retry = Duration::from_secs(1);
    loop {
        let session = unsafe { WTSGetActiveConsoleSessionId() };
        let alive = current
            .as_ref()
            .is_some_and(|(s, (process, _))| *s == session && process.alive());
        // u32::MAX: no session attached to the console (switching).
        if !alive && session != u32::MAX {
            if let Some((old, (process, attached))) = current.take() {
                info!("{what} da sessão {old} encerrado");
                drop(process);
                attached.abort();
            }
            match launch(session).await {
                Ok(Some(running)) => {
                    info!("{what} iniciado na sessão {session}");
                    current = Some((session, running));
                    retry = Duration::from_secs(1);
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("iniciar {what} na sessão {session}: {e:#} (de novo em {retry:?})");
                    tokio::time::sleep(retry).await;
                    retry = (retry * 2).min(Duration::from_secs(60));
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// A new pipe name (nobody can have it ready), for `what`.
fn pipe_name(what: &str) -> Result<String> {
    let mut nonce = [0u8; 16];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| anyhow::anyhow!("sem números aleatórios"))?;
    Ok(format!(r"\\.\pipe\hext-{what}-{}", hex(&nonce)))
}

/// Waits for process `pid` on `pipe`, and for nobody else.
async fn wait_for(pipe: &NamedPipeServer, pid: u32) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(15), pipe.connect())
        .await
        .context("o processo não se conectou")??;
    let mut client = 0;
    unsafe { GetNamedPipeClientProcessId(HANDLE(pipe.as_raw_handle()), &mut client) }?;
    if client != pid {
        bail!("processo {client} no lugar do {pid}");
    }
    Ok(())
}

/// Starts an agent in `session`, connected to `hub` through a new pipe
/// that only the system can open, and checks it is that agent on the line.
async fn launch(session: u32, hub: &AgentHub) -> Result<Option<Running>> {
    let name = pipe_name("agent")?;
    let pipe = restricted_pipe(&name, "D:P(A;;GA;;;SY)")?;
    let (process, pid) = spawn_agent(session, &name)?;
    wait_for(&pipe, pid).await?;
    let hub = hub.clone();
    let attached = tokio::spawn(async move { hub.attach(pipe).await });
    Ok(Some((process, attached)))
}

/// Starts the interface in `session`, as the user logged on there, through
/// a pipe that only the system and that user can open. Nobody logged on:
/// there is no one to run it as.
async fn launch_ui(session: u32, ui: &UiHub) -> Result<Option<Running>> {
    let Some(user) = UserToken::of(session) else {
        return Ok(None);
    };
    let (pipe, process, pid) = user.start_ui()?;
    wait_for(&pipe, pid).await?;
    let ui = ui.clone();
    let attached = tokio::spawn(async move { ui.attach(pipe).await });
    Ok(Some((process, attached)))
}

/// The token of the user logged on to a session.
struct UserToken(HANDLE);

// A kernel handle, good on any thread, and only closed on drop.
unsafe impl Send for UserToken {}
unsafe impl Sync for UserToken {}

impl Drop for UserToken {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

impl UserToken {
    fn of(session: u32) -> Option<Self> {
        let mut token = HANDLE::default();
        unsafe { WTSQueryUserToken(session, &mut token) }.ok()?;
        Some(Self(token))
    }

    /// The user's SID, as text.
    fn sid(&self) -> Result<String> {
        unsafe {
            let mut size = 0;
            // (Fails, saying how much it needs.)
            let _ = GetTokenInformation(self.0, TokenUser, None, 0, &mut size);
            let mut buffer = vec![0u64; (size as usize).div_ceil(size_of::<u64>())];
            GetTokenInformation(
                self.0,
                TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                size,
                &mut size,
            )?;
            let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
            let mut text = PWSTR::null();
            ConvertSidToStringSidW(user.User.Sid, &mut text)?;
            let sid = text.to_string();
            let _ = LocalFree(Some(HLOCAL(text.0.cast())));
            Ok(sid?)
        }
    }

    /// Whether this user has privilege `name` (enabled or not: a program of
    /// theirs could enable it).
    fn holds(&self, name: PCWSTR) -> Result<bool> {
        unsafe {
            let mut wanted = LUID::default();
            LookupPrivilegeValueW(PCWSTR::null(), name, &mut wanted)?;
            let mut size = 0;
            // (Fails, saying how much it needs.)
            let _ = GetTokenInformation(self.0, TokenPrivileges, None, 0, &mut size);
            let mut buffer = vec![0u64; (size as usize).div_ceil(size_of::<u64>())];
            GetTokenInformation(
                self.0,
                TokenPrivileges,
                Some(buffer.as_mut_ptr().cast()),
                size,
                &mut size,
            )?;
            // (The list runs past the one entry the type declares: read
            // from the buffer itself.)
            let privileges = buffer.as_ptr().cast::<TOKEN_PRIVILEGES>();
            let count = (*privileges).PrivilegeCount as usize;
            let first = (&raw const (*privileges).Privileges).cast::<LUID_AND_ATTRIBUTES>();
            let fits = (buffer.len() * size_of::<u64>())
                .saturating_sub(std::mem::offset_of!(TOKEN_PRIVILEGES, Privileges))
                / size_of::<LUID_AND_ATTRIBUTES>();
            let all = std::slice::from_raw_parts(first, count.min(fits));
            Ok(all
                .iter()
                .any(|p| p.Luid.LowPart == wanted.LowPart && p.Luid.HighPart == wanted.HighPart))
        }
    }

    /// Starts the interface as this user, with a new pipe open to the
    /// system and this user only. Returns the pipe, the process and its id.
    fn start_ui(self) -> Result<(NamedPipeServer, Process, u32)> {
        let name = pipe_name("ui")?;
        let sddl = format!("D:P(A;;GA;;;SY)(A;;GRGW;;;{})", self.sid()?);
        let pipe = restricted_pipe(&name, &sddl)?;
        let (process, pid) = self.spawn_ui(&name)?;
        Ok((pipe, process, pid))
    }

    /// Starts `<this exe> ui --pipe <name>` as this user, with their
    /// environment (their folders, not the system's).
    fn spawn_ui(&self, pipe: &str) -> Result<(Process, u32)> {
        let exe = std::env::current_exe()?;
        let command = format!("\"{}\" ui --pipe {pipe}", exe.display());
        unsafe {
            let mut environment = std::ptr::null_mut();
            CreateEnvironmentBlock(&mut environment, Some(self.0), false)?;
            let started = create_process(self.0, &command, Some(environment));
            let _ = DestroyEnvironmentBlock(environment);
            started
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A named pipe open only to those in `sddl`.
fn restricted_pipe(name: &str, sddl: &str) -> Result<NamedPipeServer> {
    let sddl: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    let pipe = unsafe {
        ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .max_instances(1)
            .create_with_security_attributes_raw(name, (&raw mut attributes).cast())
    };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
    }
    Ok(pipe?)
}

/// Starts `<this exe> agent --pipe <name>` as SYSTEM in `session`.
fn spawn_agent(session: u32, pipe: &str) -> Result<(Process, u32)> {
    let exe = std::env::current_exe()?;
    unsafe {
        let mut own = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut own)?;
        let mut token = HANDLE::default();
        let duplicated = DuplicateTokenEx(
            own,
            TOKEN_ALL_ACCESS,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut token,
        );
        let _ = CloseHandle(own);
        duplicated?;
        let result = (|| -> Result<(Process, u32)> {
            SetTokenInformation(
                token,
                TokenSessionId,
                (&raw const session).cast(),
                size_of::<u32>() as u32,
            )?;
            create_process(
                token,
                &format!("\"{}\" agent --pipe {pipe}", exe.display()),
                None,
            )
        })();
        let _ = CloseHandle(token);
        result
    }
}

/// Starts `command` with `token`, on the session's default desktop, with
/// `environment` (a block made for that token) if given.
unsafe fn create_process(
    token: HANDLE,
    command: &str,
    environment: Option<*const c_void>,
) -> Result<(Process, u32)> {
    let mut command: Vec<u16> = command.encode_utf16().chain([0]).collect();
    let mut desktop: Vec<u16> = "winsta0\\default".encode_utf16().chain([0]).collect();
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        lpDesktop: PWSTR(desktop.as_mut_ptr()),
        ..Default::default()
    };
    let mut flags = CREATE_NO_WINDOW;
    if environment.is_some() {
        flags |= CREATE_UNICODE_ENVIRONMENT;
    }
    let mut info = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessAsUserW(
            Some(token),
            PCWSTR::null(),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            flags,
            environment,
            PCWSTR::null(),
            &startup,
            &mut info,
        )?;
        let _ = CloseHandle(info.hThread);
    }
    Ok((Process(info.hProcess), info.dwProcessId))
}

// ---------------------------------------------------------------- agent

/// The agent: connects to the service's pipe and streams this session's
/// screen while the service asks.
pub fn agent(pipe: &str) -> Result<()> {
    info!("agent iniciado (sessão {})", unsafe {
        WTSGetActiveConsoleSessionId()
    });
    tokio::runtime::Runtime::new()?.block_on(async {
        let client = ClientOptions::new()
            .open(pipe)
            .with_context(|| format!("abrir {pipe}"))?;
        crate::agent::serve(client).await?;
        info!("o serviço encerrou o agent");
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;

    use super::*;

    #[tokio::test]
    async fn the_password_goes_through_its_pipe_and_only_ours_is_read() {
        use tokio::io::AsyncWriteExt;
        let name = pipe_name("password").unwrap();
        // (Open to everyone here: the test is the way through, not who opens it.)
        let mut pipe = restricted_pipe(&name, "D:P(A;;GA;;;WD)").unwrap();
        let reader = {
            let name = name.clone();
            tokio::task::spawn_blocking(move || password_from_pipe(&name))
        };
        if let Err(e) = wait_for(&pipe, std::process::id()).await {
            panic!("{e:#}; leitor: {:?}", reader.await);
        }
        pipe.write_all("uma senha bem longa".as_bytes())
            .await
            .unwrap();
        pipe.flush().await.unwrap();
        drop(pipe);
        assert_eq!(
            reader.await.unwrap().unwrap().as_str(),
            "uma senha bem longa"
        );

        assert!(password_from_pipe(r"\\.\pipe\outro").is_err());
        assert!(password_from_pipe(&format!(r"{PASSWORD_PIPE}..\x")).is_err());
    }

    #[test]
    fn an_update_gets_past_a_program_still_running() {
        let dir = std::env::temp_dir().join(format!("hext-replace-exe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("HextRemote.exe");
        let new = dir.join("new.exe");
        std::fs::copy(std::env::var("ComSpec").unwrap(), &exe).unwrap();
        std::fs::write(&new, b"the new version").unwrap();
        let mut running = Command::new(&exe)
            .args(["/c", "ping -n 30 127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap();

        let replaced = replace_exe(&new, &exe);

        let _ = running.kill();
        let _ = running.wait();
        let contents = std::fs::read(&exe);
        let _ = std::fs::remove_dir_all(&dir);
        replaced.unwrap();
        assert_eq!(contents.unwrap(), b"the new version");
    }
}
