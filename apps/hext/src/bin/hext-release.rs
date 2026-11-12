//! The release pipeline's signer: writes the signed list the apps update
//! from. It is the only program that runs next to the signing key, so it is
//! kept small: no interface, no network, none of the app's libraries.
//!
//!   hext-release keygen
//!   UPDATE_SIGNING_KEY=… hext-release manifest <pasta> <versão>

#[path = "../release.rs"]
mod release;

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--version"] => println!("hext-release {}", env!("CARGO_PKG_VERSION")),
        ["keygen"] => {
            let (seed, public) = release::keygen()?;
            println!("Segredo UPDATE_SIGNING_KEY (não compartilhe): {seed}");
            println!("Variável HEXT_UPDATE_KEY (pública):          {public}");
        }
        ["manifest", dir, version] => {
            let seed = std::env::var("UPDATE_SIGNING_KEY")
                .context("UPDATE_SIGNING_KEY: a chave que assina as atualizações")?;
            let manifest = release::write_signed(dir.as_ref(), version, &seed)?;
            for (kind, asset) in &manifest.files {
                println!("{kind}: {}", asset.name);
            }
        }
        _ => bail!("uso: hext-release keygen | hext-release manifest <pasta> <versão>"),
    }
    Ok(())
}
