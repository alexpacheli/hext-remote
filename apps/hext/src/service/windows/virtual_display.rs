//! Installing the virtual monitor's driver (docs/monitor-virtual.md, marco 2):
//! `service virtual-display install|remove`, which the MSI runs when its
//! option is chosen, and removing the Hext undoes.
//!
//! The driver's files (the Virtual Display Driver, MIT, from the release
//! pinned and checked in release.yml) come with the MSI, in the program's
//! folder: nothing is downloaded here. Installing does what the driver's own
//! script does, without its helper programs:
//!
//! 1. inserts the certificate that signs the driver's catalog (SignPath
//!    Foundation, and only that one) in `LocalMachine\TrustedPublisher`,
//!    for Windows to install it without asking;
//! 2. makes the root-enumerated device `Root\MttVDD` and installs the INF on
//!    it (`UpdateDriverForPlugAndPlayDevices`);
//! 3. leaves the device disabled: the Hext turns it on only for a session.
//!
//! What it inserted is written in the computer's folder ([`RECORD`]), for
//! removing to take away exactly that (a certificate that was there before
//! stays).

use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use hext_capture::virtual_display::{self, device};
use hext_crypto::machine_dir;
use tracing::warn;
use windows::Win32::Foundation::HWND;
use windows::Win32::Security::Cryptography::{
    CERT_CONTEXT, CERT_FIND_SHA1_HASH, CERT_OPEN_STORE_FLAGS, CERT_QUERY_CONTENT_FLAG_CTL,
    CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED, CERT_QUERY_CONTENT_TYPE_FLAGS, CERT_QUERY_ENCODING_TYPE,
    CERT_QUERY_FORMAT_FLAG_BINARY, CERT_QUERY_OBJECT_FILE, CERT_SHA1_HASH_PROP_ID,
    CERT_STORE_ADD_NEW, CERT_STORE_PROV_SYSTEM_W, CERT_SYSTEM_STORE_LOCAL_MACHINE,
    CRYPT_INTEGER_BLOB, CertAddCertificateContextToStore, CertCloseStore,
    CertDeleteCertificateFromStore, CertFindCertificateInStore, CertFreeCertificateContext,
    CertGetCertificateContextProperty, CertOpenStore, CryptQueryObject, HCERTSTORE,
    PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;
use windows::core::{PCWSTR, s, w};

use super::{exists, program_dir, system_tool, trusted_file, write_virtual_display_settings};

/// Where the MSI puts the driver's files, in the program's folder.
const DRIVER_DIR: &str = "driver";
const INF: &str = "MttVDD.inf";
const CATALOG: &str = "mttvdd.cat";
/// All of the driver's package.
const FILES: [&str; 3] = [INF, "MttVDD.dll", CATALOG];
/// The certificate that signs the driver's catalog (SignPath Foundation,
/// valid until 2027-07-09), by its SHA-1 thumbprint: the only one the Hext
/// inserts. A new driver release signed with another one changes this too.
const SIGNER: &str = "3CF8CF26D8BA266C3A483AB7D26D4A818E317D76";
/// What the install inserted, in the computer's folder.
const RECORD: &str = "monitor-virtual-instalado.txt";
/// `UpdateDriverForPlugAndPlayDevices`: even if the device has a driver.
const INSTALLFLAG_FORCE: u32 = 1;

/// What the Hext inserted when it installed the virtual monitor, for
/// removing to take away exactly that.
#[derive(Debug, Default, PartialEq, Eq)]
struct Record {
    /// Certificates inserted in TrustedPublisher (SHA-1 thumbprints).
    certificates: Vec<String>,
    /// Driver packages put in the driver store (`oemNN.inf`).
    drivers: Vec<String>,
}

impl Record {
    /// Reads the record; a line it does not know (or a value that is not
    /// what it should be) is left out.
    fn parse(text: &str) -> Self {
        let mut record = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if let Some(value) = line.strip_prefix("certificado ") {
                let value = value.trim().to_ascii_uppercase();
                if is_thumbprint(&value) && !record.certificates.contains(&value) {
                    record.certificates.push(value);
                }
            } else if let Some(value) = line.strip_prefix("driver ") {
                let value = value.trim().to_ascii_lowercase();
                if is_oem_inf(&value) && !record.drivers.contains(&value) {
                    record.drivers.push(value);
                }
            }
        }
        record
    }

    fn to_text(&self) -> String {
        let mut text = String::from(
            "# Hext Remote: o que a instalação do monitor virtual inseriu, para a remoção desfazer.\n",
        );
        for certificate in &self.certificates {
            text.push_str(&format!("certificado {certificate}\n"));
        }
        for driver in &self.drivers {
            text.push_str(&format!("driver {driver}\n"));
        }
        text
    }

    fn add_driver(&mut self, inf: &str) {
        let inf = inf.to_ascii_lowercase();
        if is_oem_inf(&inf) && !self.drivers.contains(&inf) {
            self.drivers.push(inf);
        }
    }
}

/// A SHA-1 thumbprint, in hexadecimal.
fn is_thumbprint(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The name Windows gives a package in the driver store (`oem12.inf`): the
/// only thing passed to `pnputil /delete-driver`.
fn is_oem_inf(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value
        .strip_prefix("oem")
        .and_then(|rest| rest.strip_suffix(".inf"))
        .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

/// The bytes of a thumbprint in hexadecimal.
fn thumbprint_bytes(value: &str) -> Option<Vec<u8>> {
    if !is_thumbprint(value) {
        return None;
    }
    (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16).ok())
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn record_path() -> Result<PathBuf> {
    Ok(machine_dir()?.join(RECORD))
}

/// The record, if there is one this program may believe (in the closed
/// computer's folder, written by the system or the administrators).
fn read_record(path: &Path) -> Option<Record> {
    if !path.exists() {
        return None;
    }
    if !trusted_file(path) {
        warn!("{}: de origem desconhecida, ignorado", path.display());
        return None;
    }
    std::fs::read_to_string(path)
        .ok()
        .map(|text| Record::parse(&text))
}

fn save(path: &Path, record: &Record) -> Result<()> {
    std::fs::write(path, record.to_text()).with_context(|| path.display().to_string())
}

/// `service virtual-display install`.
pub fn install() -> Result<()> {
    if !exists() {
        bail!("instale o serviço antes (HextRemote service install)");
    }
    if device::present() {
        println!("O monitor virtual já está instalado.");
        write_virtual_display_settings();
        return Ok(());
    }
    let dir = program_dir()?.join(DRIVER_DIR);
    for name in FILES {
        let path = dir.join(name);
        if !path.exists() {
            bail!(
                "{} não existe: os arquivos do driver vêm com o instalador do Hext Remote (.msi)",
                path.display()
            );
        }
        // Read elevated: only what nobody else could have changed.
        if !trusted_file(&path) {
            bail!(
                "{}: outra conta pode ter alterado o arquivo; nada foi instalado",
                path.display()
            );
        }
    }

    let path = record_path()?;
    let mut record = Record::default();
    match install_steps(&dir, &path, &mut record) {
        Ok(reboot) => {
            println!("Monitor virtual instalado (desabilitado: o Hext o liga durante as sessões).");
            if reboot {
                println!("O Windows pede para reiniciar o computador para concluir.");
            }
            Ok(())
        }
        Err(e) => {
            // Nothing half-installed stays.
            match remove_all(&record) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&path);
                }
                Err(undo) => println!("Não deu para desfazer tudo: {undo:#}"),
            }
            Err(e)
        }
    }
}

/// The steps of [`install`], each one written down in `record` (and on
/// disk, at `path`) as soon as it is done. Whether Windows asks to restart.
fn install_steps(dir: &Path, path: &Path, record: &mut Record) -> Result<bool> {
    // The record exists from the start: the device and the driver are the
    // Hext's, to take away when the Hext goes.
    save(path, record)?;

    let signer = catalog_signer(&dir.join(CATALOG))?;
    let publishers = trusted_publishers()?;
    let thumbprint = signer.thumbprint()?;
    if find(&publishers, &thumbprint).is_none() {
        unsafe {
            CertAddCertificateContextToStore(Some(publishers.0), signer.0, CERT_STORE_ADD_NEW, None)
        }
        .context("inserir o certificado do driver em Fornecedores Confiáveis")?;
        record.certificates.push(hex(&thumbprint));
        save(path, record)?;
        println!("Certificado da SignPath Foundation inserido em Fornecedores Confiáveis.");
    }

    device::create_device().context("criar o dispositivo")?;
    // Before the driver first starts: it reads them when it does.
    write_virtual_display_settings();
    let inf = dir.join(INF);
    let reboot = update_driver(&inf)?;
    for inf in device::driver_infs() {
        record.add_driver(&inf);
    }
    save(path, record)?;
    device::disable().context("desabilitar o dispositivo")?;
    Ok(reboot)
}

/// `service virtual-display remove`: the device, the driver and the
/// certificate the Hext inserted, and its settings file.
pub fn remove() -> Result<()> {
    let path = record_path()?;
    let record = read_record(&path).unwrap_or_default();
    remove_all(&record)?;
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| path.display().to_string())?;
    }
    println!("Monitor virtual removido.");
    Ok(())
}

/// For removing the Hext: the virtual monitor too, if the Hext installed it.
pub fn remove_if_ours() -> Result<()> {
    if record_path()?.exists() {
        remove()?;
    }
    Ok(())
}

/// Takes away the devices, their drivers and what `record` says the Hext
/// inserted; goes on after a step that fails, and says which did.
fn remove_all(record: &Record) -> Result<()> {
    let mut failed = Vec::new();
    // The drivers of the devices, asked before the devices go.
    let mut drivers = Record::default();
    for inf in device::driver_infs().iter().chain(&record.drivers) {
        drivers.add_driver(inf);
    }
    match device::remove_devices() {
        Ok(0) => {}
        Ok(count) => println!("Dispositivos do monitor virtual removidos: {count}"),
        Err(e) => failed.push(format!("{e}")),
    }
    for inf in &drivers.drivers {
        match delete_driver(inf) {
            Ok(()) => println!("Driver removido: {inf}"),
            Err(e) => failed.push(format!("{e:#}")),
        }
    }
    if !record.certificates.is_empty() {
        match trusted_publishers() {
            Ok(publishers) => {
                for certificate in &record.certificates {
                    match remove_certificate(&publishers, certificate) {
                        Ok(true) => println!(
                            "Certificado removido de Fornecedores Confiáveis: {certificate}"
                        ),
                        Ok(false) => {}
                        Err(e) => failed.push(format!("{e:#}")),
                    }
                }
            }
            Err(e) => failed.push(format!("{e:#}")),
        }
    }
    if let Err(e) = remove_settings() {
        failed.push(format!("{e:#}"));
    }
    if !failed.is_empty() {
        bail!("{}", failed.join("; "));
    }
    Ok(())
}

/// The settings file, if it is the Hext's (someone who wrote their own
/// keeps it), and its folder, if nothing else is in it.
fn remove_settings() -> Result<()> {
    let Some((path, xml)) = virtual_display::settings() else {
        return Ok(());
    };
    if std::fs::read_to_string(&path).is_ok_and(|text| text == xml) {
        std::fs::remove_file(&path).with_context(|| path.display().to_string())?;
        if let Some(dir) = path.parent() {
            let _ = std::fs::remove_dir(dir); // only when empty
        }
    }
    Ok(())
}

/// Takes package `inf` (`oemNN.inf`) out of the driver store.
fn delete_driver(inf: &str) -> Result<()> {
    if !is_oem_inf(inf) {
        bail!("{inf}: não é um driver do repositório do Windows");
    }
    let out = Command::new(system_tool("pnputil.exe"))
        .args(["/delete-driver", inf, "/uninstall"])
        .creation_flags(CREATE_NO_WINDOW.0)
        .output()
        .context("pnputil.exe")?;
    if !out.status.success() {
        bail!(
            "pnputil /delete-driver {inf}: {}",
            String::from_utf8_lossy(&out.stdout).trim()
        );
    }
    Ok(())
}

/// Installs the INF at `inf` on the driver's devices. Whether Windows asks
/// to restart.
fn update_driver(inf: &Path) -> Result<bool> {
    // From the system's own `newdev.dll`, loaded when needed and from
    // System32 only (see `send_sas`).
    type Update = unsafe extern "system" fn(
        HWND,
        PCWSTR,
        PCWSTR,
        u32,
        *mut windows_core::BOOL,
    ) -> windows_core::BOOL;
    let module = unsafe { LoadLibraryExW(w!("newdev.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
        .context("newdev.dll")?;
    let found = unsafe { GetProcAddress(module, s!("UpdateDriverForPlugAndPlayDevicesW")) }
        .context("UpdateDriverForPlugAndPlayDevicesW")?;
    // (The signature `newdev.h` gives it.)
    let update: Update = unsafe { std::mem::transmute(found) };
    let id = wide_str(device::NEW_HARDWARE_ID);
    let path = wide(inf);
    let mut reboot = windows_core::BOOL(0);
    let done = unsafe {
        update(
            HWND::default(),
            PCWSTR(id.as_ptr()),
            PCWSTR(path.as_ptr()),
            INSTALLFLAG_FORCE,
            &mut reboot,
        )
    };
    if !done.as_bool() {
        return Err(windows::core::Error::from_thread())
            .with_context(|| format!("instalar o driver ({})", inf.display()));
    }
    Ok(reboot.as_bool())
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain([0]).collect()
}

fn wide_str(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// A certificate store, closed with its owner.
struct Store(HCERTSTORE);

impl Drop for Store {
    fn drop(&mut self) {
        let _ = unsafe { CertCloseStore(Some(self.0), 0) };
    }
}

/// A certificate, freed with its owner.
struct Certificate(*mut CERT_CONTEXT);

impl Drop for Certificate {
    fn drop(&mut self) {
        let _ = unsafe { CertFreeCertificateContext(Some(self.0)) };
    }
}

impl Certificate {
    /// Its SHA-1 thumbprint.
    fn thumbprint(&self) -> Result<Vec<u8>> {
        let mut hash = [0u8; 20];
        let mut size = hash.len() as u32;
        unsafe {
            CertGetCertificateContextProperty(
                self.0,
                CERT_SHA1_HASH_PROP_ID,
                Some(hash.as_mut_ptr().cast()),
                &mut size,
            )
        }
        .context("impressão digital do certificado")?;
        Ok(hash[..size as usize].to_vec())
    }
}

/// The machine's TrustedPublisher store ("Fornecedores Confiáveis").
fn trusted_publishers() -> Result<Store> {
    let store = unsafe {
        CertOpenStore(
            CERT_STORE_PROV_SYSTEM_W,
            CERT_QUERY_ENCODING_TYPE(0),
            None,
            CERT_OPEN_STORE_FLAGS(CERT_SYSTEM_STORE_LOCAL_MACHINE),
            Some(w!("TrustedPublisher").as_ptr().cast()),
        )
    }
    .context("abrir Fornecedores Confiáveis")?;
    Ok(Store(store))
}

/// The certificate with SHA-1 thumbprint `thumbprint` in `store`.
fn find(store: &Store, thumbprint: &[u8]) -> Option<Certificate> {
    let blob = CRYPT_INTEGER_BLOB {
        cbData: thumbprint.len() as u32,
        pbData: thumbprint.as_ptr().cast_mut(),
    };
    let found = unsafe {
        CertFindCertificateInStore(
            store.0,
            X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
            0,
            CERT_FIND_SHA1_HASH,
            Some((&raw const blob).cast()),
            None,
        )
    };
    (!found.is_null()).then_some(Certificate(found))
}

/// The certificate that signs the catalog at `path`: the SignPath
/// Foundation's ([`SIGNER`]), or an error.
fn catalog_signer(path: &Path) -> Result<Certificate> {
    let file = wide(path);
    let mut store = HCERTSTORE(std::ptr::null_mut());
    unsafe {
        CryptQueryObject(
            CERT_QUERY_OBJECT_FILE,
            file.as_ptr().cast(),
            CERT_QUERY_CONTENT_TYPE_FLAGS(
                CERT_QUERY_CONTENT_FLAG_CTL.0 | CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED.0,
            ),
            CERT_QUERY_FORMAT_FLAG_BINARY,
            0,
            None,
            None,
            None,
            Some(&mut store),
            None,
            None,
        )
    }
    .with_context(|| format!("{}: não é um catálogo assinado", path.display()))?;
    let store = Store(store);
    let signer = thumbprint_bytes(SIGNER).context("SIGNER")?;
    find(&store, &signer).with_context(|| {
        format!(
            "{}: não vem com o certificado da SignPath Foundation ({SIGNER}); nada foi instalado",
            path.display()
        )
    })
}

/// Takes the certificate with thumbprint `thumbprint` out of `store`;
/// whether it was there.
fn remove_certificate(store: &Store, thumbprint: &str) -> Result<bool> {
    let bytes = thumbprint_bytes(thumbprint).context("impressão digital inválida")?;
    let Some(certificate) = find(store, &bytes) else {
        return Ok(false);
    };
    // Deleting frees it, also when it fails.
    let context = certificate.0;
    std::mem::forget(certificate);
    unsafe { CertDeleteCertificateFromStore(context) }
        .with_context(|| format!("remover o certificado {thumbprint}"))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{Record, SIGNER, hex, is_oem_inf, is_thumbprint, thumbprint_bytes};

    #[test]
    fn the_record_reads_back_what_it_wrote() {
        let record = Record {
            certificates: vec![SIGNER.to_owned()],
            drivers: vec!["oem12.inf".to_owned(), "oem3.inf".to_owned()],
        };
        assert_eq!(Record::parse(&record.to_text()), record);
        assert_eq!(
            Record::parse(&Record::default().to_text()),
            Record::default()
        );
    }

    #[test]
    fn the_record_leaves_out_what_it_does_not_know() {
        let text = "# comentário\r\n\
                    certificado 3cf8cf26d8ba266c3a483ab7d26d4a818e317d76\r\n\
                    certificado 3CF8CF26D8BA266C3A483AB7D26D4A818E317D76\r\n\
                    certificado nada\r\n\
                    driver OEM7.INF\r\n\
                    driver ..\\..\\Windows\\inf\\outro.inf\r\n\
                    driver oem7.inf /force\r\n\
                    outra coisa\r\n";
        assert_eq!(
            Record::parse(text),
            Record {
                certificates: vec![SIGNER.to_owned()],
                drivers: vec!["oem7.inf".to_owned()],
            }
        );
    }

    #[test]
    fn only_a_driver_store_name_goes_to_pnputil() {
        assert!(is_oem_inf("oem0.inf"));
        assert!(is_oem_inf("OEM123.INF"));
        assert!(!is_oem_inf("oem.inf"));
        assert!(!is_oem_inf("MttVDD.inf"));
        assert!(!is_oem_inf("oem1.inf /force"));
        assert!(!is_oem_inf("oem-1.inf"));
        assert!(!is_oem_inf("c:\\windows\\inf\\oem1.inf"));
    }

    #[test]
    fn thumbprints_in_hexadecimal() {
        assert!(is_thumbprint(SIGNER));
        assert!(!is_thumbprint(&SIGNER[1..]));
        assert!(!is_thumbprint(&format!("{}G", &SIGNER[1..])));
        let bytes = thumbprint_bytes(SIGNER).expect("válida");
        assert_eq!(bytes.len(), 20);
        assert_eq!(hex(&bytes), SIGNER);
        assert_eq!(thumbprint_bytes("zz"), None);
    }
}
