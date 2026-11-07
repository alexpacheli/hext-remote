//! The installed host on macOS (F4, milestone 9): a LaunchAgent of the user
//! who installs it, in their graphical session. At each login it starts the
//! app with no window (`HextRemote ui`), online with the same ID as the app
//! opened by hand, and the app stays running when its windows close.
//!
//! Everything is the user's: the program, the key and what is kept for
//! after a restart, all in `~/Library/Application Support/HextRemote`. With
//! nothing running as root, there is no unattended password here, and the
//! sign-in screen is out of reach: after a restart the viewer gets back in
//! once someone signs in (decision 3 of F4).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use hext_session::host::Restart;
use hext_session::{KeptResume, ResumeTokens};

/// The LaunchAgent's name (the app's identifier).
const LABEL: &str = "br.com.hext.remote";
/// How long `osascript` gets to hand over the restart: past this, it waits
/// for the user to allow it, which the viewer can see and click.
const RESTART_ASKED: Duration = Duration::from_secs(10);

#[allow(unsafe_code)] // a libc call that cannot fail
unsafe extern "C" {
    safe fn getuid() -> u32;
}

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .context("sem $HOME")
}

fn plist_file() -> Result<PathBuf> {
    Ok(home()?.join(format!("Library/LaunchAgents/{LABEL}.plist")))
}

/// This user's folder of the app.
fn data_dir() -> Result<PathBuf> {
    Ok(hext_crypto::config_dir()?)
}

/// The installed app: a place of its own, so the copy the user downloaded
/// can go. A bundle with its identifier, signed here: the system's privacy
/// settings do not hold for a bare program started at login.
fn bundle() -> Result<PathBuf> {
    Ok(data_dir()?.join("HextRemote.app"))
}

/// The program inside [`bundle`].
fn program() -> Result<PathBuf> {
    Ok(bundle()?.join("Contents/MacOS/HextRemote"))
}

/// The bundle's `Info.plist`. No Dock icon: it runs in the background, and
/// its windows come and go.
fn info_plist() -> String {
    let version = crate::APP_VERSION;
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>{LABEL}</string>
	<key>CFBundleExecutable</key>
	<string>HextRemote</string>
	<key>CFBundleName</key>
	<string>Hext Remote</string>
	<key>CFBundleDisplayName</key>
	<string>Hext Remote</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
	<key>LSUIElement</key>
	<true/>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
"#
    )
}

/// Puts `new` in the bundle, and signs the bundle (ad hoc, with its
/// identifier) for the privacy settings to know it.
fn place_bundle(new: &Path) -> Result<()> {
    let app = bundle()?;
    let exe = program()?;
    if let Some(dir) = exe.parent() {
        std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
    }
    let info = app.join("Contents/Info.plist");
    std::fs::write(&info, info_plist()).with_context(|| info.display().to_string())?;
    replace_exe(new, &exe)?;
    let signed = Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-", "--identifier", LABEL])
        .arg(&app)
        .output()
        .context("codesign")?;
    if !signed.status.success() {
        bail!(
            "codesign: {}",
            String::from_utf8_lossy(&signed.stderr).trim()
        );
    }
    // Downloaded, it would ask on each login whether to open it.
    let _ = Command::new("/usr/bin/xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(&app)
        .output();
    Ok(())
}

/// Where the ID is written for `service status`.
fn id_file() -> Result<PathBuf> {
    Ok(data_dir()?.join("id.txt"))
}

/// This user's graphical session, as `launchctl` names it.
fn domain() -> String {
    format!("gui/{}", getuid())
}

fn launchctl(args: &[&str]) -> Result<String> {
    let out = Command::new("/bin/launchctl")
        .args(args)
        .output()
        .context("launchctl")?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(text)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("launchctl {}: {}", args.join(" "), err.trim())
    }
}

pub fn installed() -> bool {
    plist_file().is_ok_and(|p| p.exists())
}

/// The LaunchAgent, running `exe`.
fn plist(exe: &Path) -> String {
    let exe = xml_escape(&exe.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>ui</string>
	</array>
	<key>AssociatedBundleIdentifiers</key>
	<string>{LABEL}</string>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>ProcessType</key>
	<string>Interactive</string>
</dict>
</plist>
"#
    )
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Puts the program at `exe`. The one running there keeps its file: the new
/// one takes the name, not the place.
fn replace_exe(new: &Path, exe: &Path) -> Result<()> {
    let next = exe.with_extension("new");
    let _ = std::fs::remove_file(&next);
    std::fs::copy(new, &next).with_context(|| format!("copiar para {}", next.display()))?;
    std::fs::rename(&next, exe).with_context(|| format!("mover para {}", exe.display()))?;
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Takes the LaunchAgent out of this session, if it is in: the app it
/// started closes.
fn unload() {
    let _ = launchctl(&["bootout", &format!("{}/{LABEL}", domain())]);
}

pub fn install() -> Result<()> {
    if getuid() == 0 {
        bail!(
            "no Mac, rode sem sudo: o Hext Remote passa a abrir no login de quem rodar este comando"
        );
    }
    let dir = data_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| dir.display().to_string())?;
    let exe = program()?;
    let current = std::env::current_exe()?;
    if !same_file(&current, &exe) {
        place_bundle(&current)?;
    }
    // The bare program of the first test builds of this milestone.
    let old = data_dir()?.join("HextRemote");
    if old.is_file() {
        let _ = std::fs::remove_file(old);
    }

    let file = plist_file()?;
    if installed() {
        println!("Atualizando...");
    }
    unload();
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).with_context(|| parent.display().to_string())?;
    }
    std::fs::write(&file, plist(&exe)).with_context(|| file.display().to_string())?;
    let _ = std::fs::remove_file(id_file()?);
    // The one just taken out may still be on its way.
    let path = file.to_string_lossy();
    let mut loaded = launchctl(&["bootstrap", &domain(), &path]);
    for _ in 0..10 {
        if loaded.is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
        loaded = launchctl(&["bootstrap", &domain(), &path]);
    }
    loaded?;

    println!("Instalado: o Hext Remote abre sozinho a cada login, sem janela.");
    println!("App: {}", bundle()?.display());
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
        None => println!(
            "Ainda sem ID. Se o Hext Remote já estava aberto, ele continua no lugar do instalado até o \
             próximo login. Veja {}",
            dir.join("ui.log").display()
        ),
    }
    println!(
        "Na primeira vez, o macOS pede as permissões de Gravação de Tela e Acessibilidade para o \
         Hext Remote (não para o Terminal). Depois de liberar as duas, rode `service install` de \
         novo para ele reabrir com elas."
    );
    Ok(())
}

pub fn uninstall(purge: bool) -> Result<()> {
    if installed() {
        unload();
        let file = plist_file()?;
        std::fs::remove_file(&file).with_context(|| file.display().to_string())?;
        println!("Removido: o Hext Remote não abre mais no login.");
    } else {
        println!("O Hext Remote não está instalado.");
    }
    let _ = std::fs::remove_dir_all(bundle()?);
    let dir = data_dir()?;
    super::kept::revoke(&dir);
    let _ = std::fs::remove_file(id_file()?);
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
        println!("Não instalado (HextRemote service install)");
        return Ok(());
    }
    match launchctl(&["print", &format!("{}/{LABEL}", domain())]) {
        Ok(state) if state.contains("state = running") => println!("Hext Remote: rodando"),
        Ok(_) => println!("Hext Remote: parado"),
        Err(_) => println!("Hext Remote: instalado, mas fora desta sessão (abre no próximo login)"),
    }
    match std::fs::read_to_string(id_file()?) {
        Ok(id) => println!("ID deste computador: {}", id.trim()),
        Err(_) => println!("ID: ainda não"),
    }
    println!("Acesso não assistido: não existe no Mac por enquanto (cada acesso pede aceite)");
    Ok(())
}

/// Notes the ID for `service status`.
pub fn remember_id(id: &str) {
    if let Ok(path) = id_file() {
        let _ = std::fs::write(path, id);
    }
}

fn audit(entry: &str) {
    super::kept::audit_to(super::user_log_file("audit"), entry);
}

/// Takes back the token kept before a restart, if it is still good, and
/// gives what restarts this Mac.
pub fn own_restart(resumes: &ResumeTokens) -> Option<std::sync::Arc<dyn Restart>> {
    if !installed() {
        return None;
    }
    let dir = data_dir().ok()?;
    super::kept::restore(&dir, resumes);
    Some(std::sync::Arc::new(UserRestart(dir)))
}

/// Restarts this Mac as its user would, from the Apple menu: the apps are
/// asked to quit, and one with something unsaved can hold it up. The token
/// waits in the user's folder for the app's next start, at login.
struct UserRestart(PathBuf);

impl Restart for UserRestart {
    fn restart(&self, kept: &KeptResume, who: &str) -> std::result::Result<(), String> {
        super::kept::keep(&self.0, kept)?;
        audit(&format!("reinício pedido: {who}"));
        // kAERestart to loginwindow (Apple's QA1134). From the system's
        // folder, not whatever PATH has.
        let mut child = Command::new("/usr/bin/osascript")
            .args(["-e", "tell application \"loginwindow\" to «event aevtrest»"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                self.revoke();
                format!("osascript: {e}")
            })?;
        let asked = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(done)) if done.success() => return Ok(()),
                Ok(Some(done)) => {
                    self.revoke();
                    let mut err = String::new();
                    if let Some(mut stderr) = child.stderr.take() {
                        use std::io::Read;
                        let _ = stderr.read_to_string(&mut err);
                    }
                    return Err(format!("osascript: {done} {}", err.trim()));
                }
                // Waiting on the screen for the user to allow it: a no
                // there must not leave the way back open.
                Ok(None) if asked.elapsed() >= RESTART_ASKED => {
                    let dir = self.0.clone();
                    std::thread::spawn(move || {
                        if !child.wait().is_ok_and(|done| done.success()) {
                            super::kept::revoke(&dir);
                        }
                    });
                    return Ok(());
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(200)),
                Err(e) => {
                    self.revoke();
                    return Err(format!("osascript: {e}"));
                }
            }
        }
    }

    fn revoke(&self) {
        super::kept::revoke(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_runs_the_installed_program_at_login() {
        let text = plist(Path::new(
            "/Users/ana & bia/Library/Application Support/HextRemote/HextRemote",
        ));
        assert!(text.contains(
            "<string>/Users/ana &amp; bia/Library/Application Support/HextRemote/HextRemote</string>"
        ));
        assert!(text.contains("<string>ui</string>"));
        assert!(text.contains("<string>Aqua</string>"));
        assert!(text.contains(&format!("<string>{LABEL}</string>")));
    }
}
