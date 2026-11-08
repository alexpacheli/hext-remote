use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use hext_server::mail::{SmtpConfig, SmtpMailer};
use hext_server::{Config, Db, State};
use tracing::{info, warn};

/// Servidor do Hext Remote: IDs dos computadores, presença e sinalização.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Endereço HTTP para escutar (o TLS fica no proxy na frente, ex.: Caddy).
    #[arg(long, default_value = "127.0.0.1:8080")]
    listen: SocketAddr,
    /// Arquivo do banco de dados SQLite.
    #[arg(long, default_value = "hext-server.db")]
    db: PathBuf,
    /// Atrás de um proxy (Caddy) na mesma máquina: o endereço de cada
    /// computador vem do cabeçalho X-Forwarded-For.
    #[arg(long)]
    behind_proxy: bool,
    /// Segredo que o iroh-relay apresenta para perguntar se um computador pode
    /// usá-lo (o mesmo do `bearer_token` na configuração dele).
    #[arg(long, env = "HEXT_RELAY_TOKEN", hide_env_values = true)]
    relay_token: Option<String>,
    /// Servidor SMTP dos e-mails da conta (ex.: smtp-relay.brevo.com). Sem
    /// ele, os e-mails vão só para o log. A senha vem só de HEXT_SMTP_PASSWORD.
    #[arg(long, env = "HEXT_SMTP_HOST")]
    smtp_host: Option<String>,
    /// Porta SMTP: 587 (STARTTLS) ou 465 (TLS direto).
    #[arg(long, env = "HEXT_SMTP_PORT", default_value_t = 587)]
    smtp_port: u16,
    /// Usuário SMTP.
    #[arg(long, env = "HEXT_SMTP_USER")]
    smtp_user: Option<String>,
    /// Remetente, como "Hext Remote <nao-responda@exemplo.com.br>".
    #[arg(long, env = "HEXT_MAIL_FROM")]
    mail_from: Option<String>,
}

/// The SMTP sender, if one is set up; a half setup is an error, not a
/// silent fallback to the log.
async fn mailer(args: &Args) -> anyhow::Result<Option<SmtpMailer>> {
    let Some(host) = args.smtp_host.clone().filter(|h| !h.is_empty()) else {
        return Ok(None);
    };
    let (Some(user), Some(from)) = (args.smtp_user.clone(), args.mail_from.clone()) else {
        anyhow::bail!("com HEXT_SMTP_HOST, faltam HEXT_SMTP_USER e HEXT_MAIL_FROM");
    };
    let password = std::env::var("HEXT_SMTP_PASSWORD")
        .context("com HEXT_SMTP_HOST, falta HEXT_SMTP_PASSWORD")?;
    let mailer = SmtpMailer::new(&SmtpConfig {
        host: host.clone(),
        port: args.smtp_port,
        user,
        password,
        from: from.clone(),
    })?;
    info!("e-mails por {host}:{}, de {from}", args.smtp_port);
    Ok(Some(mailer))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    let db = Db::open(&args.db).with_context(|| format!("banco {}", args.db.display()))?;
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("escutar em {}", args.listen))?;
    info!(
        "hext-server {} em http://{}",
        env!("CARGO_PKG_VERSION"),
        args.listen
    );
    let state = Arc::new(match mailer(&args).await? {
        Some(mailer) => {
            let mailer = Arc::new(mailer);
            // A wrong password or server shows at start, not at the first sign-up.
            let checking = mailer.clone();
            tokio::task::spawn_blocking(move || match checking.check() {
                Ok(()) => info!("SMTP: conexão e login ok"),
                Err(e) => warn!("SMTP: não deu para entrar: {e:#}"),
            });
            State::with_mailer(db, mailer)
        }
        None => State::new(db),
    });
    tokio::select! {
        served = hext_server::serve(listener, state, Config {
            behind_proxy: args.behind_proxy,
            relay_token: args.relay_token.filter(|t| !t.is_empty()),
        }) => served?,
        _ = tokio::signal::ctrl_c() => info!("encerrando"),
    }
    Ok(())
}
