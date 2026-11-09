//! The installed host on macOS (F4, milestone 9): a LaunchAgent of the user
//! who installs it, in their graphical session. At each login it starts the
//! app with no window (`HextRemote ui`), online with the same ID as the app
//! opened by hand, and the app stays running when its windows close.
//!
//! Everything is the user's: the program, the key and what is kept for
//! after a restart, all in `~/Library/Application Support/HextRemote`. So is
//! the unattended access (F6 marco 3): the password, the technicians and the
//! account binding, set by this user (`service password`, or the app's
//! Segurança tab), and good once they are signed in. With nothing running
//! as root, the sign-in screen is out of reach: after a restart the viewer
//! gets back in once someone signs in (decision 3 of F4).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use hext_crypto::DeviceKey;
use hext_session::host::{BoxFuture, Restart, UnattendedAccess};
use hext_session::{KeptResume, Lockout, ResumeTokens};
use zeroize::Zeroizing;

use super::access::{Access, Password};
use crate::signaling::Ask;
use crate::ui_link::{UiAsk, UiHub};

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

/// The app `exe` runs from, if it came signed with a Developer ID (the
/// `.dmg` of F6): copied whole, it keeps its signature and notarization.
fn signed_bundle(exe: &Path) -> Option<PathBuf> {
    let app = exe.parent()?.parent()?.parent()?;
    if app.extension().and_then(|e| e.to_str()) != Some("app") {
        return None;
    }
    let verified = Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict"])
        .arg(app)
        .output()
        .ok()?;
    let details = Command::new("/usr/bin/codesign")
        .arg("-dv")
        .arg(app)
        .output()
        .ok()?;
    // (codesign describes it on stderr; an ad hoc one names no authority.)
    let signed = String::from_utf8_lossy(&details.stderr).contains("Authority=Developer ID");
    (verified.status.success() && signed).then(|| app.to_owned())
}

/// Puts `new` in the bundle, and signs the bundle (ad hoc, with its
/// identifier) for the privacy settings to know it. A signed app is copied
/// as it is.
fn place_bundle(new: &Path) -> Result<()> {
    let app = bundle()?;
    if let Some(signed) = signed_bundle(new) {
        let next = app.with_extension("new");
        let _ = std::fs::remove_dir_all(&next);
        let copied = Command::new("/usr/bin/ditto")
            .arg(&signed)
            .arg(&next)
            .status()
            .context("ditto")?;
        if !copied.success() {
            bail!("ditto: {copied}");
        }
        // The one running keeps its files: the new one takes the name.
        let _ = std::fs::remove_dir_all(&app);
        std::fs::rename(&next, &app).with_context(|| app.display().to_string())?;
        return Ok(());
    }
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

/// Whether another copy of Hext Remote is running: one opened by hand keeps
/// the installed one from staying open.
fn other_open() -> bool {
    let Ok(out) = Command::new("/usr/bin/pgrep")
        .args(["-f", "HextRemote"])
        .output()
    else {
        return false;
    };
    let me = std::process::id().to_string();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .any(|pid| !pid.is_empty() && pid != me)
}

/// Whether the installed one is running now.
fn running() -> bool {
    launchctl(&["print", &format!("{}/{LABEL}", domain())])
        .is_ok_and(|state| state.contains("state = running"))
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

pub fn install(name: Option<String>) -> Result<()> {
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
    if let Some(name) = name {
        crate::name::write(&dir, Some(&name))?;
    }
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
        // The installed one did not stay open: most often, a copy opened
        // before keeps its place.
        None if !running() && other_open() => println!(
            "Outra cópia do Hext Remote está aberta neste Mac e ficou no lugar do instalado. Feche-a \
             (no Terminal: pkill -f HextRemote) e rode `service install` de novo."
        ),
        None => println!(
            "Ainda sem ID. Veja o que houve em {}",
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
        println!("Não instalado (HextRemote service install)");
        return Ok(());
    }
    match launchctl(&["print", &format!("{}/{LABEL}", domain())]) {
        Ok(state) if state.contains("state = running") => println!("Hext Remote: rodando"),
        Ok(_) if other_open() => println!(
            "Hext Remote: parado, porque outra cópia aberta está no lugar dele (feche-a com \
             pkill -f HextRemote e rode `service install` de novo)"
        ),
        Ok(_) => println!("Hext Remote: parado"),
        Err(_) => println!("Hext Remote: instalado, mas fora desta sessão (abre no próximo login)"),
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
        println!("Acesso não assistido: desligado (HextRemote service password)");
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
        if installed() && launchctl(&["kickstart", "-k", &format!("{}/{LABEL}", domain())]).is_ok()
        {
            println!(
                "O Hext Remote instalado reabriu com o nome novo. Se um aberto à mão estiver no lugar \
                 dele, feche e abra esse de novo."
            );
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

/// A line in the audit log, for the owner of the computer; also for the
/// account's Atividade screen, while bound to one.
fn audit(entry: &str) {
    super::kept::audit_to(super::user_log_file("audit"), entry);
    if let Some(activity) = ACTIVITY.get() {
        let _ = activity.send(entry.to_owned());
    }
}

/// Where the installed app sends what the audit writes (unset elsewhere).
static ACTIVITY: std::sync::OnceLock<tokio::sync::mpsc::UnboundedSender<String>> =
    std::sync::OnceLock::new();

// ------------------------------------------------------- unattended access

/// The unattended access, in this user's folder.
fn access() -> Result<Access> {
    Ok(Access::new(data_dir()?, audit))
}

/// The binding to an account, as this computer (the app's key) sees it.
fn owners() -> Result<crate::owners::Owners> {
    Ok(access()?.owners(Arc::new(crate::device_key(None)?)))
}

/// Only for the installed app, run by its user: the one that opens at
/// login reads these files.
fn require_installed() -> Result<()> {
    if getuid() == 0 {
        bail!("no Mac, rode sem sudo: o acesso é do usuário que instalou");
    }
    if !installed() {
        bail!("instale primeiro (HextRemote service install)");
    }
    Ok(())
}

/// `service password`: sets (or changes) the unattended password, or turns
/// it off.
pub fn password(off: bool) -> Result<()> {
    require_installed()?;
    let (access, owners) = (access()?, owners()?);
    if off {
        access.password_off(&owners)?;
        println!("Acesso não assistido desligado.");
        return Ok(());
    }
    let password = Zeroizing::new(rpassword::prompt_password("Nova senha: ")?);
    let again = Zeroizing::new(rpassword::prompt_password("Repita a senha: ")?);
    if *password != *again {
        bail!("as senhas não são iguais");
    }
    access.set_password(&password, &owners)?;
    println!("Acesso não assistido ligado (depois do login deste usuário).");
    println!("Durante a sessão, este Mac mostra quem está conectado.");
    Ok(())
}

/// Asks with the system's dialog, the answer hidden; `None`: cancelled.
fn ask_hidden(prompt: &str) -> Result<Option<Zeroizing<String>>> {
    let script = format!(
        "text returned of (display dialog \"{prompt}\" default answer \"\" with hidden answer \
         with title \"Hext Remote\" buttons {{\"Cancelar\", \"OK\"}} default button \"OK\" \
         cancel button \"Cancelar\")"
    );
    let out = Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .stderr(std::process::Stdio::null())
        .output()
        .context("osascript")?;
    if !out.status.success() {
        return Ok(None);
    }
    let text = Zeroizing::new(String::from_utf8(out.stdout).context("senha")?);
    Ok(Some(Zeroizing::new(
        text.trim_end_matches(['\r', '\n']).to_owned(),
    )))
}

/// The Segurança tab's buttons: the password typed in the system's dialog
/// (never in the page), or turned off. `Ok(false)`: cancelled.
pub fn set_password_from_app(off: bool) -> Result<bool> {
    require_installed()?;
    let (access, owners) = (access()?, owners()?);
    if off {
        access.password_off(&owners)?;
        return Ok(true);
    }
    let min = super::access::MIN_PASSWORD;
    let Some(password) = ask_hidden(&format!(
        "Nova senha de acesso (ao menos {min} caracteres):"
    ))?
    else {
        return Ok(false);
    };
    if password.chars().count() < min {
        bail!("a senha precisa de ao menos {min} caracteres");
    }
    let Some(again) = ask_hidden("Repita a senha de acesso:")? else {
        return Ok(false);
    };
    if *password != *again {
        bail!("as senhas não são iguais");
    }
    access.set_password(&password, &owners)?;
    Ok(true)
}

/// `service technicians`.
pub fn technicians(revoke: Option<&str>, revoke_all: bool) -> Result<()> {
    if revoke.is_some() || revoke_all {
        require_installed()?;
    }
    access()?.technicians_command(revoke, revoke_all)
}

/// `service account`: binds this Mac to the account with list `chain`,
/// unbinds it (`off`), or shows the binding.
pub fn account(chain: Option<&str>, off: bool) -> Result<()> {
    let owners = owners()?;
    if chain.is_none() && !off {
        Access::show_account(&owners);
        return Ok(());
    }
    require_installed()?;
    if off {
        access()?.unbind_account(&owners)?;
        println!("Conta desvinculada: todo acesso volta a pedir aceite.");
        return Ok(());
    }
    access()?.bind_account(&owners, chain.unwrap_or_default())
}

/// The installed app's unattended access, shown on `hub`.
pub fn login_access(key: &Arc<DeviceKey>, hub: &UiHub) -> Option<super::LoginAccess> {
    if !installed() {
        return None;
    }
    let access = access().ok()?;
    let owners = access.owners(key.clone());
    access.show_on(hub, &owners);
    let (activity, activity_out) = tokio::sync::mpsc::unbounded_channel();
    let _ = ACTIVITY.set(activity);
    tokio::spawn(crate::owners::report_activity(owners.clone(), activity_out));
    let (attempts, attempts_out) = tokio::sync::mpsc::unbounded_channel();
    Some(super::LoginAccess {
        unattended: UnattendedAccess {
            key: key.public_key(),
            lockout: Lockout::default(),
            password: Arc::new(Password {
                access: access.clone(),
                ui: hub.clone(),
                owners: Some(owners.clone()),
            }),
            attempts,
        },
        owners: Arc::new(owners.clone()),
        attempts: attempts_out,
        ask: Box::new(LoginAsk(UiAsk(hub.clone()), owners, access)),
    })
}

/// Asks on the app's window; lets in, without asking, the technicians and
/// the bound account's devices (while the password is on, for the
/// technicians).
struct LoginAsk(UiAsk, crate::owners::Owners, Access);

impl Ask for LoginAsk {
    fn ask<'a>(
        &'a mut self,
        name: &'a str,
        id: hext_signal::DeviceId,
    ) -> BoxFuture<'a, Option<hext_proto::Permissions>> {
        self.0.ask(name, id)
    }

    fn show_id(&mut self, id: hext_signal::DeviceId) {
        self.0.show_id(id);
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
