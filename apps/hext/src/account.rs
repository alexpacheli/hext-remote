//! The account on this computer (F5, marco 2): optional. The calls go on a
//! signaling connection of the viewer's key, so the login is this
//! computer's technician's: the server ties it to that key, and the token
//! kept here is worth nothing on another computer.
//!
//! The token stays in the user's own folder (`account.json`), never in the
//! pages: they get the answers without it.

use std::path::PathBuf;

use hext_signal::{AccountCall, AccountError, AccountReply, AccountView, Client};
use serde::{Deserialize, Serialize};
use tracing::warn;

const FILE: &str = "account.json";

/// The login kept between runs.
#[derive(Serialize, Deserialize)]
struct Stored {
    email: String,
    token: String,
}

/// Where the login is kept, next to the viewer's key.
pub struct Account {
    file: PathBuf,
    key: PathBuf,
    server: String,
}

impl Account {
    /// The account of the viewer's key `key` (its folder keeps the login).
    pub fn new(key: PathBuf) -> Self {
        let file = key.with_file_name(FILE);
        Self {
            file,
            key,
            server: crate::DEFAULT_SERVER.to_owned(),
        }
    }

    fn stored(&self) -> Option<Stored> {
        let text = std::fs::read_to_string(&self.file).ok()?;
        serde_json::from_str(&text)
            .inspect_err(|e| warn!("{}: {e}", self.file.display()))
            .ok()
    }

    fn store(&self, stored: &Stored) {
        let written = serde_json::to_string(stored)
            .map_err(anyhow::Error::from)
            .and_then(|text| {
                if let Some(dir) = self.file.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&self.file, text)?;
                Ok(())
            });
        if let Err(e) = written {
            warn!("{}: {e:#}", self.file.display());
        }
    }

    fn forget(&self) {
        let _ = std::fs::remove_file(&self.file);
    }

    /// Whether a login is kept (it may have ended on the server).
    pub fn kept(&self) -> bool {
        self.stored().is_some()
    }

    /// Makes `call`, logged in first with the kept login when the call needs
    /// one. A token in the answer stays here: the answer goes without it.
    /// Errors come as the text to show.
    pub async fn call(&self, call: AccountCall) -> Result<AccountReply, String> {
        let needs_login = matches!(
            call,
            AccountCall::Logout
                | AccountCall::Me
                | AccountCall::TotpSetup
                | AccountCall::TotpEnable { .. }
                | AccountCall::ChangePassword { .. }
        );
        let logout = matches!(call, AccountCall::Logout);
        let mut client = self.connect().await.map_err(|e| format!("{e:#}"))?;
        if needs_login {
            let Some(stored) = self.stored() else {
                return if logout {
                    Ok(AccountReply::Ok)
                } else {
                    Err(message(AccountError::NotLoggedIn).to_owned())
                };
            };
            let token = stored.token;
            match client.account(AccountCall::Resume { token }).await {
                Ok(AccountReply::Account(_)) => {}
                Ok(AccountReply::Error {
                    error: AccountError::NotLoggedIn,
                }) => {
                    self.forget();
                    return if logout {
                        Ok(AccountReply::Ok)
                    } else {
                        Err(message(AccountError::NotLoggedIn).to_owned())
                    };
                }
                Ok(other) => return Err(answer_error(&other)),
                Err(e) => return Err(format!("servidor de sinalização: {e}")),
            }
        }
        let reply = client
            .account(call)
            .await
            .map_err(|e| format!("servidor de sinalização: {e}"))?;
        let _ = client.close().await;
        match reply {
            AccountReply::LoggedIn { token, account } => {
                self.store(&Stored {
                    email: account.email.clone(),
                    token,
                });
                Ok(AccountReply::LoggedIn {
                    token: String::new(),
                    account,
                })
            }
            AccountReply::Ok if logout => {
                self.forget();
                Ok(AccountReply::Ok)
            }
            AccountReply::Error { error } => {
                if error == AccountError::NotLoggedIn {
                    self.forget();
                }
                Err(message(error).to_owned())
            }
            other => Ok(other),
        }
    }

    /// The account logged in here, asked of the server; `None` if there is
    /// none (or it ended there).
    pub async fn current(&self) -> Option<AccountView> {
        if !self.kept() {
            return None;
        }
        match self.call(AccountCall::Me).await {
            Ok(AccountReply::Account(view)) => Some(view),
            _ => None,
        }
    }

    async fn connect(&self) -> anyhow::Result<Client> {
        let key = crate::device_key(Some(&self.key))?;
        let client = crate::signaling::login(&self.server, &key, &crate::device_name()).await?;
        if !client.takes_accounts() {
            let _ = client.close().await;
            anyhow::bail!("o servidor ainda não tem contas");
        }
        Ok(client)
    }
}

fn answer_error(reply: &AccountReply) -> String {
    match reply {
        AccountReply::Error { error } => message(*error).to_owned(),
        _ => "resposta inesperada do servidor".to_owned(),
    }
}

/// What to tell the user.
pub fn message(error: AccountError) -> &'static str {
    match error {
        AccountError::InvalidEmail => "Esse e-mail não parece certo.",
        AccountError::WeakPassword => "A senha precisa ter de 10 a 128 caracteres.",
        AccountError::WrongCredentials => "E-mail ou senha errados.",
        AccountError::NotVerified => {
            "Confirme o e-mail antes: use o código que chegou na sua caixa de entrada."
        }
        AccountError::WrongCode => "Código errado ou vencido.",
        AccountError::TooManyAttempts => "Tentativas demais. Espere uns 15 minutos.",
        AccountError::NotLoggedIn => "Você não está mais na conta neste computador. Entre de novo.",
        AccountError::AlreadyEnabled => "A verificação em duas etapas já está ligada.",
        AccountError::NoSetup => "Comece a ligar a verificação em duas etapas de novo.",
        AccountError::ServerError => "O servidor teve um problema. Tente de novo daqui a pouco.",
    }
}

/// A QR code of `text`, as SVG, for the authenticator app.
pub fn qr_svg(text: &str) -> Option<String> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    Some(
        code.render::<qrcode::render::svg::Color>()
            .min_dimensions(200, 200)
            .quiet_zone(true)
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use hext_server::accounts::Mailer;

    use super::*;

    #[derive(Default)]
    struct Inbox(Mutex<Vec<String>>);

    impl Mailer for Inbox {
        fn send(&self, _to: &str, _subject: &str, body: &str) {
            self.0.lock().unwrap().push(body.to_owned());
        }
    }

    async fn code(inbox: &Inbox) -> String {
        for _ in 0..500 {
            if let Some(body) = inbox.0.lock().unwrap().last() {
                return body
                    .split(|c: char| !c.is_ascii_digit())
                    .find(|w| w.len() == 6)
                    .unwrap()
                    .to_owned();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no e-mail");
    }

    #[tokio::test]
    async fn the_login_is_kept_here_and_the_token_never_leaves() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = format!("ws://{}", listener.local_addr().unwrap());
        let inbox = Arc::new(Inbox::default());
        let state = Arc::new(hext_server::State::with_mailer(
            hext_server::Db::in_memory().unwrap(),
            inbox.clone(),
        ));
        tokio::spawn(hext_server::serve(
            listener,
            state,
            hext_server::Config::default(),
        ));

        let dir = std::env::temp_dir().join(format!("hext-account-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut account = Account::new(dir.join("viewer.key"));
        account.server = server;
        let (email, password) = ("dono@example.com".to_owned(), "cavalo correto".to_owned());

        assert_eq!(account.current().await, None);
        assert_eq!(
            account.call(AccountCall::Me).await,
            Err(message(AccountError::NotLoggedIn).to_owned())
        );
        let register = AccountCall::Register {
            email: email.clone(),
            password: password.clone(),
        };
        assert_eq!(account.call(register).await, Ok(AccountReply::Ok));
        let verify = AccountCall::Verify {
            email: email.clone(),
            code: code(&inbox).await,
            password: password.clone(),
        };
        assert_eq!(account.call(verify).await, Ok(AccountReply::Ok));
        let login = AccountCall::Login {
            email: email.clone(),
            password: "senha errada demais".into(),
            second: None,
        };
        assert_eq!(
            account.call(login).await,
            Err(message(AccountError::WrongCredentials).to_owned())
        );
        let login = AccountCall::Login {
            email: email.clone(),
            password,
            second: None,
        };
        let view = AccountView {
            email: email.clone(),
            totp: false,
        };
        assert_eq!(
            account.call(login).await,
            Ok(AccountReply::LoggedIn {
                token: String::new(),
                account: view.clone(),
            })
        );
        assert!(account.kept());
        // A new connection each call: the kept login is resumed.
        assert_eq!(account.current().await, Some(view));
        assert!(matches!(
            account.call(AccountCall::TotpSetup).await,
            Ok(AccountReply::Totp { .. })
        ));
        assert_eq!(
            account.call(AccountCall::Logout).await,
            Ok(AccountReply::Ok)
        );
        assert!(!account.kept());
        assert_eq!(account.current().await, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_authenticator_link_makes_a_qr_code() {
        let svg = qr_svg("otpauth://totp/Hext%20Remote:a%40b.com?secret=ABC&issuer=Hext%20Remote")
            .unwrap();
        assert!(svg.contains("<svg"), "{svg}");
    }
}
