//! Hext Remote.
//!
//! Started without a command, this is the interface (Tauri, Windows and
//! macOS): the ID, the requests and connecting to another computer. The
//! commands remain for what has no screen yet, and for testing: `host` shares
//! this machine's screen; `view` opens a window showing a host's screen and
//! forwards mouse and keyboard.

#[cfg_attr(not(windows), allow(dead_code))] // the service is Windows-only for now
mod agent;
mod bench;
#[cfg(any(windows, target_os = "macos"))]
mod chat;
mod host;
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))] // for the interface
mod recent;
mod service;
mod signaling;
#[cfg(any(windows, target_os = "macos"))]
mod ui;
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))] // for the interface
mod ui_link;
mod viewer;

/// Linux has no interface until F7.
#[cfg(not(any(windows, target_os = "macos")))]
mod ui {
    pub enum Link {
        Portable,
    }

    pub fn run(_link: Link) -> anyhow::Result<()> {
        anyhow::bail!("sem interface neste sistema por enquanto: veja --help")
    }
}

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use hext_proto::DEFAULT_PORT;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Our relay (São Paulo), for when the two computers cannot connect directly.
const DEFAULT_RELAY: &str = "https://relay.hext.com.br";

/// The relay to use: the `--relay` option, except that our relay serves only
/// devices registered with our server, so with another server (or none)
/// the default relay is left out.
fn relay_for(value: &str, server: Option<&str>) -> Result<Option<hext_net::RelayUrl>> {
    if value == DEFAULT_RELAY && server != Some(DEFAULT_SERVER) {
        tracing::info!(
            "sem relay: o relay padrão só atende computadores registrados no servidor padrão (--relay <url> para usar outro)"
        );
        return Ok(None);
    }
    relay(value)
}

/// Our signaling server: IDs, presence and connection requests.
const DEFAULT_SERVER: &str = "wss://signal.hext.com.br";

/// The `--server` option: a URL, or `off` (no IDs; connecting by IP only).
fn server(value: &str) -> Option<&str> {
    (!value.eq_ignore_ascii_case("off")).then_some(value)
}

/// The `--relay` option: a URL, or `off` for direct connections only.
fn relay(value: &str) -> Result<Option<hext_net::RelayUrl>> {
    if value.eq_ignore_ascii_case("off") {
        return Ok(None);
    }
    let url = value
        .parse()
        .with_context(|| format!("relay inválido: {value}"))?;
    Ok(Some(url))
}

#[derive(Parser)]
#[command(
    name = "hext",
    version,
    about = "Hext Remote — acesso e suporte remoto"
)]
struct Cli {
    /// Sem comando: abre a interface.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Compartilha a tela deste computador e aceita controle remoto.
    Host(HostArgs),
    /// Abre a tela de um host e controla mouse e teclado.
    View(ViewArgs),
    /// Mede o tempo de codificação de vídeo com uma imagem da tela (macOS).
    Bench(BenchArgs),
    /// Testa os codificadores de vídeo da GPU com várias configurações
    /// (diagnóstico, Windows).
    Encoders,
    /// Testa se os codificadores da GPU mudam a taxa com o vídeo rodando
    /// (diagnóstico, Windows).
    EncoderRates,
    /// Instala o Hext Remote como serviço do Windows: online com o ID deste
    /// computador desde o boot, antes de alguém entrar.
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// (Interno) Captura e input na sessão do usuário, a pedido do serviço.
    #[command(hide = true)]
    Agent {
        #[arg(long)]
        pipe: String,
    },
    /// (Interno) A interface na sessão do usuário, iniciada pelo serviço.
    #[command(hide = true)]
    Ui {
        #[arg(long)]
        pipe: String,
    },
}

#[derive(Subcommand)]
enum ServiceAction {
    /// Instala (ou atualiza) e inicia o serviço. Precisa de administrador.
    Install,
    /// Para e remove o serviço. Precisa de administrador.
    Uninstall {
        /// Apaga também a identidade deste computador (o ID deixa de valer).
        #[arg(long)]
        purge: bool,
    },
    /// Mostra se o serviço está rodando e o ID deste computador.
    Status,
    /// Liga o acesso não assistido: define a senha (pedida aqui, sem aparecer).
    /// Precisa de administrador.
    Password {
        /// Desliga o acesso não assistido e apaga a senha.
        #[arg(long)]
        off: bool,
    },
    /// (Interno) O próprio serviço, iniciado pelo Windows.
    #[command(hide = true)]
    Run,
}

#[derive(clap::Args)]
struct BenchArgs {
    /// Monitor a capturar (0 = principal).
    #[arg(long, default_value_t = 0)]
    display: usize,
    /// Quadros por configuração.
    #[arg(long, default_value_t = 180, value_parser = clap::value_parser!(u32).range(20..=10_000))]
    frames: u32,
    /// Taxa de bits do vídeo, em Mbps.
    #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u32).range(1..=200))]
    bitrate_mbps: u32,
    /// Quadros por segundo (o ritmo do host).
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(1..=240))]
    fps: u32,
}

#[derive(clap::Args)]
struct HostArgs {
    /// Endereço e porta UDP para escutar.
    #[arg(long, default_value_t = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DEFAULT_PORT))]
    listen: SocketAddr,
    /// Código de acesso. Sem ele, um código aleatório é gerado.
    #[arg(long)]
    code: Option<String>,
    /// Monitor a compartilhar (0 = principal).
    #[arg(long, default_value_t = 0)]
    display: usize,
    /// Transmite uma imagem de teste em vez da tela (padrão no Linux por enquanto).
    #[arg(long, default_value_t = !cfg!(any(windows, target_os = "macos")))]
    test_pattern: bool,
    /// Taxa de bits do vídeo, em Mbps.
    #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u32).range(1..=200))]
    bitrate_mbps: u32,
    /// Limite de quadros por segundo.
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(1..=240))]
    fps: u32,
    /// Codificador de vídeo.
    #[arg(long, value_enum, default_value_t = CodecChoice::Auto)]
    encoder: CodecChoice,
    /// Arquivo da chave deste computador (criado na primeira execução). O padrão
    /// fica na pasta de configurações do usuário.
    #[arg(long)]
    device_key: Option<std::path::PathBuf>,
    /// Servidor de sinalização: mostra o ID deste computador e pergunta aqui se
    /// aceita cada pedido de acesso (`off`: só conexões pelo IP).
    #[arg(long, default_value = DEFAULT_SERVER)]
    server: String,
    /// Relay para quando não dá para conectar direto (`off` desliga).
    #[arg(long, default_value = DEFAULT_RELAY)]
    relay: String,
}

/// Which encoder (host) or decoder (viewer) to use.
#[derive(Clone, Copy, ValueEnum)]
enum CodecChoice {
    /// Placa de vídeo se houver; senão, ou se ela falhar, o processador.
    Auto,
    /// Sempre o processador (OpenH264).
    Software,
    /// Só a placa de vídeo; falha se não houver.
    Hardware,
}

#[derive(clap::Args)]
struct ViewArgs {
    /// ID do host (com --server), ou o IP dele com porta opcional (padrão 7420).
    target: String,
    /// Código de acesso mostrado pelo host (para conectar pelo IP).
    #[arg(long)]
    code: Option<String>,
    /// Servidor de sinalização, para conectar pelo ID.
    #[arg(long, default_value = DEFAULT_SERVER)]
    server: String,
    /// Arquivo da chave deste computador (criado na primeira execução).
    #[arg(long)]
    device_key: Option<std::path::PathBuf>,
    /// Chave mostrada pelo host (para conectar pelo IP).
    #[arg(long)]
    key: Option<hext_crypto::PublicKey>,
    /// Relay para quando não dá para conectar direto (`off` desliga).
    #[arg(long, default_value = DEFAULT_RELAY)]
    relay: String,
    /// Visualizador no Mac, host Windows/Linux: o que a tecla Command envia.
    #[arg(long, value_enum, default_value_t = CmdKey::Ctrl)]
    cmd_key: CmdKey,
    /// Visualizador Windows/Linux, host Mac: o que a tecla Ctrl envia.
    #[arg(long, value_enum, default_value_t = CtrlKey::Cmd)]
    ctrl_key: CtrlKey,
    /// Sem janela: só recebe, decodifica e mostra estatísticas (para testes).
    #[arg(long)]
    headless: bool,
    /// No modo --headless, por quantos segundos receber.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..))]
    seconds: u64,
    /// Decodificador de vídeo.
    #[arg(long, value_enum, default_value_t = CodecChoice::Auto)]
    decoder: CodecChoice,
    /// Entra com a senha de acesso não assistido do host, lida da entrada
    /// padrão (num terminal, é pedida sem aparecer). Só pelo ID.
    #[arg(long)]
    password_stdin: bool,
    /// (Interno) Avisa a interface que abriu este visualizador quando conectar.
    #[arg(long, hide = true)]
    report_to_ui: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CtrlKey {
    /// Ctrl funciona como Command (Ctrl+C copia no Mac); a tecla Windows vira Control.
    Cmd,
    /// Ctrl continua Control.
    Ctrl,
}

#[derive(Clone, Copy, ValueEnum)]
enum CmdKey {
    /// Command funciona como Ctrl (Cmd+C copia no Windows).
    Ctrl,
    /// Command funciona como a tecla Windows.
    Win,
}

fn main() -> Result<()> {
    let command = Cli::parse().command;
    // The service and its agents have no console: they log to a file.
    let log_file = match &command {
        Some(Command::Service {
            action: ServiceAction::Run,
        }) => service::log_file("service"),
        Some(Command::Agent { .. }) => service::log_file("agent"),
        Some(Command::Ui { .. }) | None => service::user_log_file("ui"),
        _ => None,
    };
    // noq_udp (iroh's QUIC) warns on every MTU probe that a link (e.g.
    // Tailscale, MTU 1280) cannot carry; it just keeps the smaller size.
    let logs = tracing_subscriber::fmt().with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info,wgpu_core=warn,wgpu_hal=warn,naga=warn,noq_udp=error".into()),
    );
    match log_file {
        Some(file) => logs
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .init(),
        None => logs.init(),
    }

    match command {
        None => ui::run(ui::Link::Portable),
        Some(Command::Host(args)) => tokio::runtime::Runtime::new()?.block_on(host::run(args)),
        Some(Command::View(args)) => viewer::run(args),
        Some(Command::Bench(args)) => bench::run(&args),
        Some(Command::Encoders) => bench::encoders(),
        Some(Command::EncoderRates) => bench::encoder_rates(),
        Some(Command::Service { action }) => service::command(action),
        Some(Command::Agent { pipe }) => service::agent(&pipe),
        #[cfg(windows)]
        Some(Command::Ui { pipe }) => ui::run(ui::Link::Service(pipe)),
        #[cfg(not(windows))]
        Some(Command::Ui { .. }) => anyhow::bail!("a interface do serviço só existe no Windows"),
    }
}

/// This computer's key, created on first run.
fn device_key(path: Option<&std::path::Path>) -> Result<hext_crypto::DeviceKey> {
    let store = match path {
        Some(path) => hext_crypto::FileKeyStore::new(path),
        None => hext_crypto::FileKeyStore::default_location()?,
    };
    let (key, created) = hext_crypto::load_or_create(&store)?;
    if created {
        tracing::info!(
            "chave deste computador criada em {}",
            store.path().display()
        );
    }
    Ok(key)
}

/// The Downloads folder of the user `token` says, or of this process's.
#[cfg(windows)]
#[allow(unsafe_code)] // Win32 calls
fn downloads_of(token: Option<windows::Win32::Foundation::HANDLE>) -> Option<std::path::PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    let path = unsafe { SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, token) }.ok()?;
    let text = unsafe { path.to_string() };
    unsafe { CoTaskMemFree(Some(path.0.cast())) };
    text.ok().map(Into::into)
}

/// This user's Downloads folder.
#[cfg(windows)]
fn downloads() -> Option<std::path::PathBuf> {
    downloads_of(None)
}

/// This user's Downloads folder.
#[cfg(not(windows))]
fn downloads() -> Option<std::path::PathBuf> {
    Some(std::path::PathBuf::from(std::env::var_os("HOME")?).join("Downloads"))
}

fn device_name() -> String {
    gethostname::gethostname().to_string_lossy().into_owned()
}

/// Name of this executable as the user runs it (`HextRemote.exe`, `hext`…).
fn exe_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "hext".to_owned())
}

fn parse_addr(addr: &str) -> Result<SocketAddr> {
    if let Ok(sock) = addr.parse::<SocketAddr>() {
        return Ok(sock);
    }
    let ip: IpAddr = addr
        .parse()
        .with_context(|| format!("endereço inválido: {addr}"))?;
    Ok(SocketAddr::new(ip, DEFAULT_PORT))
}
