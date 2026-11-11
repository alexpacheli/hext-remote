//! The release pipeline's side of the updates: the list of a release's files
//! (`latest.json`) and its Ed25519 signature. Nothing here touches the
//! interface or the network, so `hext-release`, the program that holds the
//! signing key in the pipeline, is built from this alone (see
//! `src/bin/hext-release.rs`); the app checks what this signs
//! ([`crate::update`]).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde::{Deserialize, Serialize};

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

/// A new signing key: the seed (secret, for the pipeline) and the public
/// half (built into the app), both in hex.
pub fn keygen() -> Result<(String, String)> {
    // (Our copies of the seed are wiped; ring's own, inside the pair, are
    // out of reach.)
    let mut seed = zeroize::Zeroizing::new([0u8; 32]);
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), seed.as_mut())
        .map_err(|_| anyhow::anyhow!("sem números aleatórios"))?;
    let pair = Ed25519KeyPair::from_seed_unchecked(seed.as_ref())
        .map_err(|_| anyhow::anyhow!("chave inválida"))?;
    Ok((hex::encode(seed), hex::encode(pair.public_key().as_ref())))
}

/// `bytes` signed with `seed` (hex), in hex.
pub fn sign(bytes: &[u8], seed: &str) -> Result<String> {
    let mut raw = zeroize::Zeroizing::new([0u8; 32]);
    hex::decode_to_slice(seed.trim(), raw.as_mut()).context("chave de assinatura inválida")?;
    let pair = Ed25519KeyPair::from_seed_unchecked(raw.as_ref())
        .map_err(|_| anyhow::anyhow!("chave inválida"))?;
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

/// Writes `latest.json` and `latest.json.sig` for the installers in `dir`,
/// signed with `seed` (hex).
pub fn write_signed(dir: &Path, version: &str, seed: &str) -> Result<Manifest> {
    let manifest = manifest_of(dir, version)?;
    let bytes = serde_json::to_vec_pretty(&manifest)?;
    let signature = sign(&bytes, seed)?;
    std::fs::write(dir.join("latest.json"), &bytes)?;
    std::fs::write(dir.join("latest.json.sig"), &signature)?;
    Ok(manifest)
}

pub fn sha256(bytes: &[u8]) -> String {
    hex::encode(ring::digest::digest(&ring::digest::SHA256, bytes))
}
