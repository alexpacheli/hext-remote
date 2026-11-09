//! Automatic updates (F6, marco 2). Each release carries `latest.json`, the
//! list of its files with their SHA-256, and `latest.json.sig`, an Ed25519
//! signature of it by the release pipeline's key. The app takes a version
//! only with that signature, made by the key built into it
//! (`HEXT_UPDATE_KEY`, at build time): without the key (a build of the CI,
//! or a developer's), there are no updates. GitHub only serves the files.
//!
//! The Windows service installs the new `.msi` by itself, between sessions;
//! elsewhere the app says there is a new version and opens its page.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde::{Deserialize, Serialize};

/// Where the releases are (`HEXT_RELEASES` when compiling, for a copy of the
/// project that publishes its own).
const RELEASES: &str = crate::built_or(
    option_env!("HEXT_RELEASES"),
    "https://github.com/alexpacheli/remote-control/releases",
);

/// The public half of the key that signs the releases (64 hex digits).
const KEY: Option<&str> = option_env!("HEXT_UPDATE_KEY");

/// What a release has, as `latest.json` says it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    /// By kind ("windows-msi", "macos-dmg", ...).
    pub files: BTreeMap<String, Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub sha256: String,
}

impl Manifest {
    /// Where a file of this release is downloaded from.
    #[cfg_attr(not(windows), allow(dead_code))] // the Windows service downloads
    pub fn url(&self, asset: &Asset) -> String {
        format!("{RELEASES}/download/v{}/{}", self.version, asset.name)
    }

    /// The release's page.
    pub fn page(&self) -> String {
        format!("{RELEASES}/tag/v{}", self.version)
    }
}

/// Whether this build takes updates at all.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))] // the interface and the service ask
pub fn enabled() -> bool {
    KEY.is_some_and(|key| public_key(key).is_ok())
}

fn public_key(text: &str) -> Result<[u8; 32]> {
    let mut key = [0; 32];
    hex::decode_to_slice(text.trim(), &mut key).context("chave de atualização inválida")?;
    Ok(key)
}

/// Whether version `a` comes after `b` (numbers separated by dots; anything
/// after a `-` is left out).
pub fn newer(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.trim_start_matches('v')
            .split('-')
            .next()
            .unwrap_or_default()
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    }
    let (a, b) = (parts(a), parts(b));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    false
}

/// The manifest in `bytes`, if `signature` (hex) is `key`'s.
pub fn verify(bytes: &[u8], signature: &str, key: &[u8; 32]) -> Result<Manifest> {
    let signature = hex::decode(signature.trim()).context("assinatura da atualização inválida")?;
    UnparsedPublicKey::new(&ED25519, key)
        .verify(bytes, &signature)
        .map_err(|_| anyhow::anyhow!("a assinatura da atualização não confere"))?;
    serde_json::from_slice(bytes).context("lista da atualização inválida")
}

/// A new signing key: the seed (secret, for the pipeline) and the public
/// half (built into the app), both in hex.
pub fn keygen() -> Result<(String, String)> {
    let mut seed = [0u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut seed)
        .map_err(|_| anyhow::anyhow!("sem números aleatórios"))?;
    let pair = Ed25519KeyPair::from_seed_unchecked(&seed)
        .map_err(|_| anyhow::anyhow!("chave inválida"))?;
    Ok((hex::encode(seed), hex::encode(pair.public_key().as_ref())))
}

/// `bytes` signed with `seed` (hex), in hex.
pub fn sign(bytes: &[u8], seed: &str) -> Result<String> {
    let mut raw = [0u8; 32];
    hex::decode_to_slice(seed.trim(), &mut raw).context("chave de assinatura inválida")?;
    let pair =
        Ed25519KeyPair::from_seed_unchecked(&raw).map_err(|_| anyhow::anyhow!("chave inválida"))?;
    Ok(hex::encode(pair.sign(bytes).as_ref()))
}

/// The kind of a release file, by its name.
fn kind(name: &str) -> Option<&'static str> {
    [
        ("-x64.msi", "windows-msi"),
        ("-portable.exe", "windows-portable"),
        ("-macos.app.tar.gz", "macos-app"),
        (".dmg", "macos-dmg"),
        (".deb", "linux-deb"),
        (".rpm", "linux-rpm"),
        (".AppImage", "linux-appimage"),
    ]
    .into_iter()
    .find(|(end, _)| name.ends_with(end))
    .map(|(_, kind)| kind)
}

/// The manifest of the files in `dir`, for `version`.
pub fn manifest_of(dir: &Path, version: &str) -> Result<Manifest> {
    let mut files = BTreeMap::new();
    for entry in std::fs::read_dir(dir).with_context(|| dir.display().to_string())? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(kind) = kind(name) else {
            continue;
        };
        let bytes = std::fs::read(&path).with_context(|| path.display().to_string())?;
        files.insert(
            kind.to_owned(),
            Asset {
                name: name.to_owned(),
                sha256: sha256(&bytes),
            },
        );
    }
    if files.is_empty() {
        bail!("nenhum instalador em {}", dir.display());
    }
    Ok(Manifest {
        version: version.to_owned(),
        files,
    })
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(ring::digest::digest(&ring::digest::SHA256, bytes))
}

fn http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .use_preconfigured_tls((*hext_signal::tls_config()).clone())
        .user_agent(concat!("HextRemote/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(300))
        .build()
        .context("cliente HTTP")
}

async fn get(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let reply = client
        .get(url)
        .send()
        .await
        .with_context(|| url.to_owned())?;
    if !reply.status().is_success() {
        bail!("{url}: {}", reply.status());
    }
    Ok(reply.bytes().await?.to_vec())
}

/// The latest release, if it is newer than this program and signed by the
/// key built in. `None`: up to date.
pub async fn check() -> Result<Option<Manifest>> {
    let manifest = latest().await?;
    Ok(newer(&manifest.version, crate::APP_VERSION).then_some(manifest))
}

/// The latest release's list, signed by the key built in.
pub async fn latest() -> Result<Manifest> {
    let Some(key) = KEY else {
        bail!("esta versão não se atualiza sozinha (feita fora do pipeline de release)");
    };
    let key = public_key(key)?;
    let client = http()?;
    let manifest_url = format!("{RELEASES}/latest/download/latest.json");
    let bytes = get(&client, &manifest_url).await?;
    let signature = get(&client, &format!("{manifest_url}.sig")).await?;
    verify(&bytes, &String::from_utf8_lossy(&signature), &key)
}

/// Downloads the file of `kind` into `dir`, checked against the manifest.
#[cfg_attr(not(windows), allow(dead_code))] // the Windows service installs it
pub async fn download(manifest: &Manifest, kind: &str, dir: &Path) -> Result<PathBuf> {
    let asset = manifest
        .files
        .get(kind)
        .with_context(|| format!("a versão {} não tem {kind}", manifest.version))?;
    // (Only a plain name: the manifest is signed, but still.)
    if asset.name.contains(['/', '\\']) || asset.name.starts_with('.') {
        bail!("nome de arquivo inválido: {}", asset.name);
    }
    let bytes = get(&http()?, &manifest.url(asset)).await?;
    if sha256(&bytes) != asset.sha256 {
        bail!("{}: o arquivo baixado não confere com a lista", asset.name);
    }
    std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
    let path = dir.join(&asset.name);
    std::fs::write(&path, bytes).with_context(|| path.display().to_string())?;
    Ok(path)
}

/// `update`: the command line.
pub fn command(action: crate::UpdateAction) -> Result<()> {
    match action {
        crate::UpdateAction::Check => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            match runtime.block_on(check())? {
                Some(manifest) => println!(
                    "Versão nova: {} (esta é a {}). Baixe em {}",
                    manifest.version,
                    crate::APP_VERSION,
                    manifest.page()
                ),
                None => println!("Atualizado: {}.", crate::APP_VERSION),
            }
        }
        crate::UpdateAction::Keygen => {
            let (seed, public) = keygen()?;
            println!("Segredo UPDATE_SIGNING_KEY (não compartilhe): {seed}");
            println!("Variável HEXT_UPDATE_KEY (pública):          {public}");
        }
        crate::UpdateAction::Manifest { dir, version } => {
            let seed = std::env::var("UPDATE_SIGNING_KEY")
                .context("UPDATE_SIGNING_KEY: a chave que assina as atualizações")?;
            let manifest = manifest_of(&dir, &version)?;
            let bytes = serde_json::to_vec_pretty(&manifest)?;
            let signature = sign(&bytes, &seed)?;
            std::fs::write(dir.join("latest.json"), &bytes)?;
            std::fs::write(dir.join("latest.json.sig"), &signature)?;
            for (kind, asset) in &manifest.files {
                println!("{kind}: {}", asset.name);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.6.1", "0.6.0"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("v1.0", "0.99.99"));
        assert!(!newer("0.6.0", "0.6.0"));
        assert!(!newer("0.6.0-beta", "0.6.0"));
        assert!(!newer("0.5.9", "0.6"));
    }

    #[test]
    fn only_the_keys_signature_is_taken() {
        let (seed, public) = keygen().unwrap();
        let key = public_key(&public).unwrap();
        let manifest = Manifest {
            version: "9.9.9".into(),
            files: BTreeMap::from([(
                "windows-msi".to_owned(),
                Asset {
                    name: "HextRemote-9.9.9-x64.msi".into(),
                    sha256: "00".into(),
                },
            )]),
        };
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let signature = sign(&bytes, &seed).unwrap();
        assert_eq!(verify(&bytes, &signature, &key).unwrap(), manifest);
        // Changed by one byte, or signed by another key: refused.
        let mut changed = bytes.clone();
        changed[5] ^= 1;
        assert!(verify(&changed, &signature, &key).is_err());
        let (other, _) = keygen().unwrap();
        assert!(verify(&bytes, &sign(&bytes, &other).unwrap(), &key).is_err());
        assert!(verify(&bytes, "zz", &key).is_err());
    }

    #[test]
    fn the_manifest_lists_each_installer_with_its_hash() {
        let dir = std::env::temp_dir().join(format!("hext-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "HextRemote-0.6.0-x64.msi",
            "HextRemote-0.6.0-portable.exe",
            "HextRemote-0.6.0.dmg",
            "HextRemote-0.6.0-macos.app.tar.gz",
            "hext-remote_0.6.0-1_amd64.deb",
            "hext-remote-0.6.0-1.x86_64.rpm",
            "HextRemote-0.6.0-x86_64.AppImage",
            "SHA256SUMS.txt",
        ] {
            std::fs::write(dir.join(name), name).unwrap();
        }
        let manifest = manifest_of(&dir, "0.6.0").unwrap();
        assert_eq!(manifest.files.len(), 7, "{manifest:?}");
        let msi = &manifest.files["windows-msi"];
        assert_eq!(msi.sha256, sha256(b"HextRemote-0.6.0-x64.msi"));
        assert_eq!(
            manifest.url(msi),
            format!("{RELEASES}/download/v0.6.0/HextRemote-0.6.0-x64.msi")
        );
        assert_eq!(
            manifest.files["macos-app"].name,
            "HextRemote-0.6.0-macos.app.tar.gz"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
