//! Account e-mails sent through an SMTP server (F5 decision 3: Brevo, or
//! any provider with SMTP).

use std::time::Duration;

use anyhow::{Context, bail};
use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use tracing::{info, warn};

use crate::accounts::Mailer;

/// How long one e-mail may take (it goes in the background).
const TIMEOUT: Duration = Duration::from_secs(30);

/// Where and as whom the e-mails go.
pub struct SmtpConfig {
    pub host: String,
    /// 465: TLS from the start; any other (587): STARTTLS, required.
    pub port: u16,
    pub user: String,
    pub password: String,
    /// The sender, as `Hext Remote <nao-responda@exemplo.com.br>`.
    pub from: String,
}

/// Sends the account e-mails through an SMTP server, always encrypted.
pub struct SmtpMailer {
    transport: SmtpTransport,
    from: Mailbox,
}

impl SmtpMailer {
    pub fn new(config: &SmtpConfig) -> anyhow::Result<Self> {
        if config.host.is_empty() || config.user.is_empty() || config.password.is_empty() {
            bail!("SMTP: faltam o servidor, o usuário ou a senha");
        }
        let builder = if config.port == 465 {
            SmtpTransport::relay(&config.host)
        } else {
            SmtpTransport::starttls_relay(&config.host)
        }
        .with_context(|| format!("SMTP: {}", config.host))?;
        let transport = builder
            .port(config.port)
            .credentials(Credentials::new(
                config.user.clone(),
                config.password.clone(),
            ))
            .timeout(Some(TIMEOUT))
            .build();
        Self::with_transport(transport, &config.from)
    }

    fn with_transport(transport: SmtpTransport, from: &str) -> anyhow::Result<Self> {
        let from = from
            .parse()
            .with_context(|| format!("remetente inválido: {from}"))?;
        Ok(Self { transport, from })
    }

    /// Whether the server answers and takes the login (blocking).
    pub fn check(&self) -> anyhow::Result<()> {
        if self.transport.test_connection()? {
            Ok(())
        } else {
            bail!("o servidor não respondeu")
        }
    }

    fn message(&self, to: &str, subject: &str, body: &str) -> anyhow::Result<Message> {
        Ok(Message::builder()
            .from(self.from.clone())
            .to(to.parse()?)
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
            .body(body.to_owned())?)
    }
}

impl Mailer for SmtpMailer {
    fn send(&self, to: &str, subject: &str, body: &str) {
        let sent = self
            .message(to, subject, body)
            .and_then(|message| Ok(self.transport.send(&message)?));
        // (The body has the code, and the address is personal: neither goes
        // to the log as it is.)
        let to = crate::accounts::mask_email(to);
        match sent {
            Ok(_) => info!("e-mail enviado para {to}: {subject}"),
            Err(e) => warn!("e-mail para {to} não foi enviado: {e:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    use super::*;

    /// A one-message SMTP server: answers each command and hands back the
    /// commands and the message it got.
    fn fake_server() -> (u16, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut out = stream.try_clone().unwrap();
            let mut lines = BufReader::new(stream);
            let mut got = String::new();
            out.write_all(b"220 fake ESMTP\r\n").unwrap();
            let mut data = false;
            loop {
                let mut line = String::new();
                if lines.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                got.push_str(&line);
                let command = line.trim_end().to_uppercase();
                let answer: &[u8] = if data {
                    if command == "." {
                        data = false;
                        b"250 queued\r\n"
                    } else {
                        continue;
                    }
                } else if command.starts_with("EHLO") {
                    b"250-fake\r\n250 AUTH PLAIN LOGIN\r\n"
                } else if command.starts_with("AUTH") {
                    b"235 ok\r\n"
                } else if command == "DATA" {
                    data = true;
                    b"354 go on\r\n"
                } else if command == "QUIT" {
                    out.write_all(b"221 bye\r\n").unwrap();
                    break;
                } else {
                    b"250 ok\r\n"
                };
                out.write_all(answer).unwrap();
            }
            got
        });
        (port, server)
    }

    #[test]
    fn an_email_goes_to_the_server_with_its_sender_and_text() {
        let (port, server) = fake_server();
        let transport = SmtpTransport::builder_dangerous("127.0.0.1")
            .port(port)
            .credentials(Credentials::new("user".into(), "secret".into()))
            .timeout(Some(Duration::from_secs(5)))
            .build();
        let mailer =
            SmtpMailer::with_transport(transport, "Hext Remote <nao-responda@hext.test>").unwrap();
        mailer.send("ana@exemplo.com", "Confirm", "Your code: 123456");
        drop(mailer);
        let got = server.join().unwrap();
        assert!(got.contains("AUTH PLAIN"), "{got}");
        assert!(got.contains("MAIL FROM:<nao-responda@hext.test>"), "{got}");
        assert!(got.contains("RCPT TO:<ana@exemplo.com>"), "{got}");
        assert!(got.contains("Subject: Confirm"), "{got}");
        assert!(got.contains("Your code: 123456"), "{got}");
    }

    #[test]
    fn a_wrong_sender_or_missing_login_is_refused_at_start() {
        let config = |from: &str, password: &str| SmtpConfig {
            host: "smtp.exemplo.com".into(),
            port: 587,
            user: "user".into(),
            password: password.into(),
            from: from.into(),
        };
        assert!(SmtpMailer::new(&config("não é e-mail", "x")).is_err());
        assert!(SmtpMailer::new(&config("Hext <a@exemplo.com>", "")).is_err());
        assert!(SmtpMailer::new(&config("Hext <a@exemplo.com>", "x")).is_ok());
    }

    /// Reaches a real provider (`cargo test -p hext-server -- --ignored`):
    /// STARTTLS and the certificate check, without logging in.
    #[test]
    #[ignore = "needs the network"]
    fn starttls_works_against_a_real_provider() {
        let transport = SmtpTransport::starttls_relay("smtp-relay.brevo.com")
            .unwrap()
            .port(587)
            .timeout(Some(Duration::from_secs(15)))
            .build();
        assert!(transport.test_connection().unwrap());
    }
}
