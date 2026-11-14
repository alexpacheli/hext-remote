//! The installed host on Linux (F7, milestone 4): a systemd *user* unit, as
//! the Mac's LaunchAgent. At each graphical login it starts the app with no
//! window (`hext-remote ui`), online with the same ID as the app opened by
//! hand, and the app stays running when its windows close.
//!
//! Nothing runs as root: an X11 session is its user's, and so are the
//! screen, the keyboard and the mouse that the host takes. Everything is
//! kept in the user's folder (`~/.config/hext-remote`): the key, what is
//! kept for after a restart and the unattended access ([`super::user`]).
//! The sign-in screen is out of reach: after a restart the viewer gets back
//! in once someone signs in (as on the Mac; see DECISIONS).
//!
//! The `.deb` and the `.rpm` bring the unit (`/usr/lib/systemd/user`);
//! `service install`, run by the user, turns it on for them. A program
//! elsewhere (the AppImage) gets a unit of its own in the user's folder.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::access::Access;
use super::user::{access, owners};

/// The unit's name.
const UNIT: &str = "hext-remote.service";
/// The program the packages put on the PATH (and their unit runs).
const PACKAGED: &str = "/usr/bin/hext-remote";
/// Where the packages put the unit.
const PACKAGED_UNIT: &str = "/usr/lib/systemd/user/hext-remote.service";

/// The program, as the user types it.
pub const PROGRAM: &str = "hext-remote";
/// This computer, in messages.
pub const THIS_COMPUTER: &str = "este computador";
/// "On this system", in messages.
pub const ON_THIS_SYSTEM: &str = "no Linux";

#[allow(unsafe_code)] // a libc call that cannot fail
unsafe extern "C" {
    safe fn getuid() -> u32;
}

/// Whether this runs as root (`sudo`).
pub fn is_root() -> bool {
    getuid() == 0
}

/// This user's folder of the app.
pub fn data_dir() -> Result<PathBuf> {
    Ok(hext_crypto::config_dir()?)
}

/// This user's configuration folder (`$XDG_CONFIG_HOME`, or `~/.config`).
fn config_home() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|home| PathBuf::from(home).join(".config"))
        .context("sem $HOME")
}

/// This user's units.
fn unit_dir() -> Result<PathBuf> {
    Ok(config_home()?.join("systemd/user"))
}

/// The link `systemctl --user enable` makes: the unit is on.
fn enabled_link() -> Result<PathBuf> {
    Ok(unit_dir()?
        .join("graphical-session.target.wants")
        .join(UNIT))
}

/// Desktops that start no `graphical-session.target` (Xfce, MATE...)
/// still run what is in autostart: this starts the unit there too, with
/// the session's display in its environment.
fn autostart_file() -> Result<PathBuf> {
    Ok(config_home()?.join("autostart/hext-remote.desktop"))
}

/// Where the ID is written for `service status`.
fn id_file() -> Result<PathBuf> {
    Ok(data_dir()?.join("id.txt"))
}

pub fn installed() -> bool {
    enabled_link().is_ok_and(|link| link.symlink_metadata().is_ok())
}

/// The unit, running `exe`. A program failing at start (a bad update) stops
/// being restarted after five tries in two minutes.
fn unit(exe: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=Hext Remote: acesso remoto\n\
         Documentation=https://github.com/alexpacheli/hext-remote\n\
         PartOf=graphical-session.target\n\
         After=graphical-session.target\n\
         StartLimitIntervalSec=120\n\
         StartLimitBurst=5\n\
         \n\
         [Service]\n\
         ExecStart={} ui\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=graphical-session.target\n",
        systemd_quote(&exe.to_string_lossy())
    )
}

/// A path as one word of `ExecStart` (quoted, with `\`, `"` and `%`
/// escaped).
fn systemd_quote(path: &str) -> String {
    let escaped = path
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{escaped}\"")
}

/// The autostart entry (see [`autostart_file`]).
fn autostart() -> String {
    "[Desktop Entry]\n\
     Type=Application\n\
     Name=Hext Remote\n\
     Comment=Inicia o acesso remoto deste computador\n\
     Exec=sh -c \"systemctl --user import-environment DISPLAY XAUTHORITY WAYLAND_DISPLAY XDG_SESSION_TYPE XDG_CURRENT_DESKTOP; systemctl --user start hext-remote.service\"\n\
     NoDisplay=true\n\
     X-GNOME-Autostart-Phase=Applications\n"
        .to_owned()
}

/// Opens the installed program again after its package was updated, as the
/// unit: restarted by systemd when this process is the unit's (after
/// reading a unit file the package may have changed); otherwise started
/// once this one is gone (a copy opened from the menu, which the unit gave
/// way to). `Ok(false)`: not installed, nothing done.
pub fn reopen_installed() -> Result<bool> {
    if !installed() {
        return Ok(false);
    }
    if std::fs::read_to_string("/proc/self/cgroup").is_ok_and(|c| in_unit(&c)) {
        let _ = systemctl(&["daemon-reload"]);
        // (systemd stops this one and starts the new one.)
        systemctl(&["restart", "--no-block", UNIT])?;
        return Ok(true);
    }
    Command::new("/bin/sh")
        .args(["-c", r#"sleep 2; exec systemctl --user start "$0""#, UNIT])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("systemctl")?;
    std::process::exit(0);
}

/// Whether `/proc/self/cgroup` puts this process in the unit (not just
/// below some other systemd service, whose variables it may have).
fn in_unit(cgroup: &str) -> bool {
    cgroup
        .lines()
        .any(|line| line.trim_end().ends_with(&format!("/{UNIT}")))
}

fn systemctl(args: &[&str]) -> Result<String> {
    let out = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .context("systemctl (o Hext Remote instalado precisa do systemd)")?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if out.status.success() {
        Ok(text)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("systemctl --user {}: {}", args.join(" "), err.trim())
    }
}

/// Whether the installed one is running now.
fn running() -> bool {
    systemctl(&["is-active", UNIT]).is_ok_and(|state| state == "active")
}

/// The program the unit should run: the AppImage this runs from, else this
/// program.
fn program() -> Result<PathBuf> {
    if let Some(image) = std::env::var_os("APPIMAGE").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(image));
    }
    Ok(std::env::current_exe()?)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

pub fn install(name: Option<String>) -> Result<()> {
    if is_root() {
        bail!(
            "rode sem sudo: o Hext Remote passa a abrir no login de quem rodar este comando \
             (o acesso é desse usuário)"
        );
    }
    let dir = data_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| dir.display().to_string())?;
    let exe = program()?;
    let own_unit = unit_dir()?.join(UNIT);
    let packaged = Path::new(PACKAGED_UNIT).exists() && same_file(&exe, Path::new(PACKAGED));
    if installed() {
        println!("Atualizando...");
    }
    if packaged {
        // The package's unit runs this same program: no copy of it.
        let _ = std::fs::remove_file(&own_unit);
    } else {
        let parent = own_unit.parent().context("pasta das unidades")?;
        std::fs::create_dir_all(parent).with_context(|| parent.display().to_string())?;
        std::fs::write(&own_unit, unit(&exe)).with_context(|| own_unit.display().to_string())?;
    }
    let start = autostart_file()?;
    if let Some(parent) = start.parent() {
        std::fs::create_dir_all(parent).with_context(|| parent.display().to_string())?;
    }
    std::fs::write(&start, autostart()).with_context(|| start.display().to_string())?;
    let _ = std::fs::remove_file(id_file()?);
    if let Some(name) = name {
        crate::name::write(&dir, Some(&name))?;
    }
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", UNIT])?;
    // Started now if this session has a display; else at the next login.
    let session =
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
    if session {
        let _ = systemctl(&[
            "import-environment",
            "DISPLAY",
            "XAUTHORITY",
            "WAYLAND_DISPLAY",
            "XDG_SESSION_TYPE",
            "XDG_CURRENT_DESKTOP",
        ]);
        systemctl(&["restart", UNIT])?;
    }

    println!("Instalado: o Hext Remote abre sozinho a cada login deste usuário, sem janela.");
    println!("Programa: {}", exe.display());
    if !session {
        println!("Ele abre no próximo login na área de trabalho.");
        return Ok(());
    }
    let mut id = None;
    for _ in 0..30 {
        if let Ok(text) = std::fs::read_to_string(id_file()?) {
            id = Some(text);
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    match id {
        Some(id) => println!("ID deste computador: {}", id.trim()),
        None if !running() => println!(
            "O Hext Remote instalado não ficou aberto. Veja o que houve com \
             `journalctl --user -u {UNIT}` e em {}",
            dir.join("ui.log").display()
        ),
        None => println!(
            "Ainda sem ID. Se outra cópia do Hext Remote estava aberta, o instalado passa a valer \
             quando ela fechar. Veja também {}",
            dir.join("ui.log").display()
        ),
    }
    Ok(())
}

pub fn uninstall(purge: bool) -> Result<()> {
    if installed() {
        let _ = systemctl(&["disable", "--now", UNIT]);
        println!("Removido: o Hext Remote não abre mais no login.");
    } else {
        println!("O Hext Remote não está instalado.");
    }
    let _ = std::fs::remove_file(unit_dir()?.join(UNIT));
    let _ = std::fs::remove_file(enabled_link()?);
    let _ = std::fs::remove_file(autostart_file()?);
    let _ = systemctl(&["daemon-reload"]);
    let dir = data_dir()?;
    super::kept::revoke(&dir);
    let _ = std::fs::remove_file(id_file()?);
    // Nor the Wayland portal's answer: the next host asks the person again.
    let _ = std::fs::remove_file(dir.join("portal-token"));
    // Unattended access does not come back with a later install.
    let _ = hext_crypto::pake::Verifier::remove(&access()?.password_file());
    if purge {
        let key = dir.join("device.key");
        if key.exists() {
            std::fs::remove_file(&key).with_context(|| key.display().to_string())?;
            println!(
                "Identidade deste computador apagada: o ID dele deixou de valer (também para o app \
                 aberto à mão, que ganha outro)."
            );
        }
    }
    Ok(())
}

pub fn status() -> Result<()> {
    if !installed() {
        println!("Não instalado ({PROGRAM} service install)");
        return Ok(());
    }
    if running() {
        println!("Hext Remote: rodando");
    } else {
        println!("Hext Remote: parado (abre no próximo login; journalctl --user -u {UNIT})");
    }
    match std::fs::read_to_string(id_file()?) {
        Ok(id) => println!("ID deste computador: {}", id.trim()),
        Err(_) => println!("ID: ainda não"),
    }
    println!(
        "Nome deste computador: {}",
        crate::name::described(&data_dir()?)
    );
    let access = access()?;
    if access.verifier().is_some() {
        println!("Acesso não assistido: ligado (depois do login deste usuário)");
        println!("Técnicos vinculados: {}", access.technicians().list().len());
    } else {
        println!("Acesso não assistido: desligado ({PROGRAM} service password)");
    }
    Access::show_account(&owners()?);
    Ok(())
}

/// Shows or changes this computer's name (the user's: the app opened by
/// hand has it too). Installed, the app reopens to take it.
pub fn name(change: super::NameChange) -> Result<()> {
    let dir = data_dir()?;
    let changed = super::apply_name(&dir, change)?;
    println!("Nome deste computador: {}", crate::name::described(&dir));
    if changed {
        if installed() && running() && systemctl(&["restart", UNIT]).is_ok() {
            println!("O Hext Remote instalado reabriu com o nome novo.");
        } else {
            println!("Vale a partir da próxima vez que o Hext Remote abrir.");
        }
    }
    Ok(())
}

/// Notes the ID for `service status`.
pub fn remember_id(id: &str) {
    if let Ok(path) = id_file() {
        let _ = std::fs::write(path, id);
    }
}

/// Restarts the computer through logind, as the user's menu does: allowed
/// to the user at the screen when nobody else is signed in; otherwise it
/// takes an administrator, and this fails rather than wait for one. On a
/// failure, the token kept in `dir` is revoked.
pub fn restart(dir: &Path) -> Result<(), String> {
    let out = Command::new("systemctl")
        .args(["--no-ask-password", "reboot"])
        .output();
    match out {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            super::kept::revoke(dir);
            Err(format!(
                "systemctl reboot: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
        Err(e) => {
            super::kept::revoke(dir);
            Err(format!("systemctl reboot: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_units_own_process_is_the_unit() {
        let slice = "0::/user.slice/user-1000.slice/user@1000.service/app.slice";
        assert!(in_unit(&format!("{slice}/hext-remote.service\n")));
        // Opened from the menu, or below another service.
        assert!(!in_unit(&format!(
            "{slice}/app-gnome-hext-remote-4242.scope\n"
        )));
        assert!(!in_unit(&format!(
            "{slice}/org.gnome.Shell@wayland.service\n"
        )));
    }

    #[test]
    fn the_unit_runs_the_program_at_login() {
        let text = unit(Path::new("/home/ana bia/Apps/Hext 100%.AppImage"));
        assert!(text.contains("ExecStart=\"/home/ana bia/Apps/Hext 100%%.AppImage\" ui\n"));
        assert!(text.contains("WantedBy=graphical-session.target"));
        assert!(text.contains("PartOf=graphical-session.target"));
        assert!(text.contains("Restart=on-failure"));
        // The package's unit says the same as one made here for its program.
        let packaged = include_str!("../../../../installer/linux/hext-remote.service");
        assert_eq!(
            packaged,
            unit(Path::new(PACKAGED)).replace("\"/usr/bin/hext-remote\"", "/usr/bin/hext-remote")
        );
    }

    #[test]
    fn the_autostart_starts_the_unit() {
        let text = autostart();
        assert!(text.contains("systemctl --user start hext-remote.service"));
        assert!(text.contains("import-environment DISPLAY XAUTHORITY"));
    }
}
