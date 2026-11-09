//! Accounts (F5, marco 1): the calls on real signaling connections, with the
//! e-mails caught instead of sent.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hext_crypto::DeviceKey;
use hext_server::accounts::{Mailer, totp_code};
use hext_server::{Config, Db, State};
use hext_signal::{AccountCall, AccountError, AccountReply, AccountView, Client, Role};

const EMAIL: &str = "dono@example.com";
const PASSWORD: &str = "cavalo correto bateria";

/// The e-mails "sent": (to, subject, body).
#[derive(Default)]
struct Inbox(Mutex<Vec<(String, String, String)>>);

impl Mailer for Inbox {
    fn send(&self, to: &str, subject: &str, body: &str) {
        let mail = (to.to_owned(), subject.to_owned(), body.to_owned());
        self.0.lock().unwrap().push(mail);
    }
}

impl Inbox {
    fn count(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    /// Waits for e-mail number `n` (from 0) and returns it (they go in the
    /// background).
    async fn nth(&self, n: usize) -> (String, String, String) {
        for _ in 0..500 {
            if let Some(mail) = self.0.lock().unwrap().get(n).cloned() {
                return mail;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no e-mail number {n}");
    }

    /// The 6-digit code in e-mail number `n`.
    async fn code(&self, n: usize) -> String {
        let (_, _, body) = self.nth(n).await;
        body.split(|c: char| !c.is_ascii_digit())
            .find(|word| word.len() == 6)
            .expect("a code")
            .to_owned()
    }
}

async fn server() -> (String, Arc<Inbox>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let inbox = Arc::new(Inbox::default());
    let state = Arc::new(State::with_mailer(Db::in_memory().unwrap(), inbox.clone()));
    tokio::spawn(hext_server::serve(listener, state, Config::default()));
    (url, inbox)
}

async fn device(url: &str, key: &DeviceKey) -> Client {
    let client = Client::connect(url, key, "PC", "0.1.0", Role::Viewer)
        .await
        .unwrap();
    assert!(client.takes_accounts());
    client
}

async fn call(client: &mut Client, call: AccountCall) -> AccountReply {
    client.account(call).await.unwrap()
}

fn error(error: AccountError) -> AccountReply {
    AccountReply::error(error)
}

fn register(email: &str, password: &str) -> AccountCall {
    AccountCall::Register {
        email: email.into(),
        password: password.into(),
    }
}

fn verify(code: &str) -> AccountCall {
    AccountCall::Verify {
        email: EMAIL.into(),
        code: code.into(),
        password: PASSWORD.into(),
    }
}

fn login(password: &str, second: Option<&str>) -> AccountCall {
    AccountCall::Login {
        email: EMAIL.into(),
        password: password.into(),
        second: second.map(Into::into),
    }
}

fn token(reply: AccountReply) -> String {
    match reply {
        AccountReply::LoggedIn { token, .. } => token,
        other => panic!("not logged in: {other:?}"),
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A confirmed account; the e-mails it took.
async fn account(client: &mut Client, inbox: &Inbox) {
    let before = inbox.count();
    assert_eq!(
        call(client, register(EMAIL, PASSWORD)).await,
        AccountReply::Ok
    );
    let code = inbox.code(before).await;
    assert_eq!(call(client, verify(&code)).await, AccountReply::Ok);
}

#[tokio::test]
async fn register_confirm_log_in_and_out() {
    let (url, inbox) = server().await;
    let key = DeviceKey::generate().unwrap();
    let mut pc = device(&url, &key).await;

    assert_eq!(
        call(&mut pc, register("not an address", PASSWORD)).await,
        error(AccountError::InvalidEmail)
    );
    assert_eq!(
        call(&mut pc, register(EMAIL, "curta")).await,
        error(AccountError::WeakPassword)
    );
    assert_eq!(
        call(&mut pc, register(" Dono@Example.com ", PASSWORD)).await,
        AccountReply::Ok
    );
    let (to, _, _) = inbox.nth(0).await;
    assert_eq!(to, EMAIL);
    let code = inbox.code(0).await;

    assert_eq!(
        call(&mut pc, login(PASSWORD, None)).await,
        error(AccountError::NotVerified)
    );
    assert_eq!(
        call(&mut pc, verify("000000")).await,
        error(AccountError::WrongCode)
    );
    assert_eq!(call(&mut pc, verify(&code)).await, AccountReply::Ok);

    assert_eq!(
        call(&mut pc, login("senha errada errada", None)).await,
        error(AccountError::WrongCredentials)
    );
    assert_eq!(
        call(&mut pc, AccountCall::Me).await,
        error(AccountError::NotLoggedIn)
    );
    let reply = call(&mut pc, login(PASSWORD, None)).await;
    let view = AccountView {
        email: EMAIL.into(),
        totp: false,
    };
    assert!(matches!(&reply, AccountReply::LoggedIn { account, .. } if *account == view));
    let token = token(reply);
    assert_eq!(
        call(&mut pc, AccountCall::Me).await,
        AccountReply::Account(view.clone())
    );

    // On a new connection of the same device, the token is enough...
    pc.close().await.unwrap();
    let mut pc = device(&url, &key).await;
    assert_eq!(
        call(
            &mut pc,
            AccountCall::Resume {
                token: token.clone()
            }
        )
        .await,
        AccountReply::Account(view)
    );
    // ...but not on another device.
    let mut other = device(&url, &DeviceKey::generate().unwrap()).await;
    assert_eq!(
        call(
            &mut other,
            AccountCall::Resume {
                token: token.clone()
            }
        )
        .await,
        error(AccountError::NotLoggedIn)
    );
    assert_eq!(
        call(&mut other, AccountCall::Me).await,
        error(AccountError::NotLoggedIn)
    );

    assert_eq!(call(&mut pc, AccountCall::Logout).await, AccountReply::Ok);
    assert_eq!(
        call(&mut pc, AccountCall::Me).await,
        error(AccountError::NotLoggedIn)
    );
    assert_eq!(
        call(&mut pc, AccountCall::Resume { token }).await,
        error(AccountError::NotLoggedIn)
    );
}

#[tokio::test]
async fn registering_again_does_not_tell_or_take_the_account() {
    let (url, inbox) = server().await;
    let mut pc = device(&url, &DeviceKey::generate().unwrap()).await;
    account(&mut pc, &inbox).await;

    // Same answer as a new address; the owner hears of it by e-mail.
    let mut stranger = device(&url, &DeviceKey::generate().unwrap()).await;
    assert_eq!(
        call(&mut stranger, register(EMAIL, "outra senha qualquer")).await,
        AccountReply::Ok
    );
    let (to, subject, body) = inbox.nth(1).await;
    assert_eq!(to, EMAIL);
    assert!(subject.contains("já tem uma conta"), "{subject}");
    assert!(!body.chars().any(|c| c.is_ascii_digit()), "no code: {body}");
    // The password is still the owner's.
    assert_eq!(
        call(&mut stranger, login("outra senha qualquer", None)).await,
        error(AccountError::WrongCredentials)
    );
    token(call(&mut pc, login(PASSWORD, None)).await);
    // Confirming again says nothing either.
    let code = inbox.code(0).await;
    assert_eq!(
        call(&mut stranger, verify(&code)).await,
        error(AccountError::WrongCode)
    );
}

#[tokio::test]
async fn whoever_has_the_code_sets_the_password() {
    let (url, inbox) = server().await;
    // Someone registers the owner's address first, and cannot confirm it.
    let mut stranger = device(&url, &DeviceKey::generate().unwrap()).await;
    let theirs = "senha do estranho";
    assert_eq!(
        call(&mut stranger, register(EMAIL, theirs)).await,
        AccountReply::Ok
    );
    inbox.nth(0).await;
    // The owner registers too and confirms with the newest code.
    let mut pc = device(&url, &DeviceKey::generate().unwrap()).await;
    account(&mut pc, &inbox).await;
    assert_eq!(
        call(&mut stranger, login(theirs, None)).await,
        error(AccountError::WrongCredentials)
    );
    token(call(&mut pc, login(PASSWORD, None)).await);
}

#[tokio::test]
async fn two_factor_and_recovery_codes() {
    let (url, inbox) = server().await;
    let mut pc = device(&url, &DeviceKey::generate().unwrap()).await;
    account(&mut pc, &inbox).await;
    assert_eq!(
        call(&mut pc, AccountCall::TotpSetup).await,
        error(AccountError::NotLoggedIn)
    );
    token(call(&mut pc, login(PASSWORD, None)).await);

    assert_eq!(
        call(
            &mut pc,
            AccountCall::TotpEnable {
                code: "123456".into()
            }
        )
        .await,
        error(AccountError::NoSetup)
    );
    let AccountReply::Totp { secret, uri } = call(&mut pc, AccountCall::TotpSetup).await else {
        panic!("no secret");
    };
    assert!(
        uri.starts_with("otpauth://totp/Hext%20Remote:dono%40example.com?secret="),
        "{uri}"
    );
    assert!(uri.contains(&secret));
    let now = now();
    let code = totp_code(&secret, now).unwrap();
    let AccountReply::RecoveryCodes { codes } =
        call(&mut pc, AccountCall::TotpEnable { code: code.clone() }).await
    else {
        panic!("no recovery codes");
    };
    assert_eq!(codes.len(), 10);
    assert_eq!(
        call(&mut pc, AccountCall::TotpSetup).await,
        error(AccountError::AlreadyEnabled)
    );
    assert_eq!(
        call(&mut pc, AccountCall::Me).await,
        AccountReply::Account(AccountView {
            email: EMAIL.into(),
            totp: true,
        })
    );

    // Now the password alone is not enough.
    assert_eq!(
        call(&mut pc, login(PASSWORD, None)).await,
        AccountReply::NeedsSecondFactor
    );
    assert_eq!(
        call(&mut pc, login(PASSWORD, Some("000000"))).await,
        error(AccountError::WrongCode)
    );
    // The code that turned it on is used.
    assert_eq!(
        call(&mut pc, login(PASSWORD, Some(&code))).await,
        error(AccountError::WrongCode)
    );
    // The next one (one step of drift) works, once.
    let next = totp_code(&secret, now + 30).unwrap();
    token(call(&mut pc, login(PASSWORD, Some(&next))).await);
    assert_eq!(
        call(&mut pc, login(PASSWORD, Some(&next))).await,
        error(AccountError::WrongCode)
    );
    // A recovery code works once too, typed any way.
    let typed = codes[3].to_lowercase().replace('-', " ");
    token(call(&mut pc, login(PASSWORD, Some(&typed))).await);
    assert_eq!(
        call(&mut pc, login(PASSWORD, Some(&codes[3]))).await,
        error(AccountError::WrongCode)
    );
    // A wrong password with a right code says only that the password is wrong.
    assert_eq!(
        call(&mut pc, login("senha errada errada", Some(&codes[4]))).await,
        error(AccountError::WrongCredentials)
    );
    token(call(&mut pc, login(PASSWORD, Some(&codes[4]))).await);
}

#[tokio::test]
async fn forgot_password_logs_every_device_out() {
    let (url, inbox) = server().await;
    let mut pc = device(&url, &DeviceKey::generate().unwrap()).await;
    account(&mut pc, &inbox).await;
    let old = token(call(&mut pc, login(PASSWORD, None)).await);

    // An unknown address gets the same answer, and no e-mail.
    let forgot = |email: &str| AccountCall::ForgotPassword {
        email: email.into(),
    };
    assert_eq!(
        call(&mut pc, forgot("ninguem@example.com")).await,
        AccountReply::Ok
    );
    assert_eq!(call(&mut pc, forgot(EMAIL)).await, AccountReply::Ok);
    let code = inbox.code(1).await;
    let reset = |code: &str, password: &str| AccountCall::ResetPassword {
        email: EMAIL.into(),
        code: code.into(),
        password: password.into(),
    };
    assert_eq!(
        call(&mut pc, reset(&code, "curta")).await,
        error(AccountError::WeakPassword)
    );
    assert_eq!(
        call(&mut pc, reset("000000", "senha nova e longa")).await,
        error(AccountError::WrongCode)
    );
    assert_eq!(
        call(&mut pc, reset(&code, "senha nova e longa")).await,
        AccountReply::Ok
    );
    assert_eq!(inbox.count(), 2, "nothing for the unknown address");
    assert_eq!(
        call(&mut pc, AccountCall::Me).await,
        error(AccountError::NotLoggedIn)
    );
    assert_eq!(
        call(&mut pc, AccountCall::Resume { token: old }).await,
        error(AccountError::NotLoggedIn)
    );
    assert_eq!(
        call(&mut pc, login(PASSWORD, None)).await,
        error(AccountError::WrongCredentials)
    );
    token(call(&mut pc, login("senha nova e longa", None)).await);
    // The code was used.
    assert_eq!(
        call(&mut pc, reset(&code, "mais uma senha nova")).await,
        error(AccountError::WrongCode)
    );
}

#[tokio::test]
async fn changing_the_password_logs_the_other_devices_out() {
    let (url, inbox) = server().await;
    let key = DeviceKey::generate().unwrap();
    let mut pc = device(&url, &key).await;
    let mut notebook = device(&url, &DeviceKey::generate().unwrap()).await;
    account(&mut pc, &inbox).await;
    let mine = token(call(&mut pc, login(PASSWORD, None)).await);
    let theirs = token(call(&mut notebook, login(PASSWORD, None)).await);

    let change = |old: &str, new: &str| AccountCall::ChangePassword {
        old: old.into(),
        new: new.into(),
    };
    assert_eq!(
        call(&mut pc, change("senha errada errada", "senha nova e longa")).await,
        error(AccountError::WrongCredentials)
    );
    assert_eq!(
        call(&mut pc, change(PASSWORD, "senha nova e longa")).await,
        AccountReply::Ok
    );
    // The notebook's open connection is out too, and its token is worthless.
    assert_eq!(
        call(&mut notebook, AccountCall::Me).await,
        error(AccountError::NotLoggedIn)
    );
    assert_eq!(
        call(&mut notebook, AccountCall::Resume { token: theirs }).await,
        error(AccountError::NotLoggedIn)
    );
    pc.close().await.unwrap();
    let mut pc = device(&url, &key).await;
    assert!(matches!(
        call(&mut pc, AccountCall::Resume { token: mine }).await,
        AccountReply::Account(_)
    ));
}

#[tokio::test]
async fn password_guesses_are_limited() {
    let (url, inbox) = server().await;
    let mut pc = device(&url, &DeviceKey::generate().unwrap()).await;
    account(&mut pc, &inbox).await;
    // Registering took 1 of the 10 for this address (it sent an e-mail);
    // confirming did not (it was right).
    let mut refused = 0;
    for _ in 0..10 {
        if call(&mut pc, login("senha errada errada", None)).await
            == error(AccountError::TooManyAttempts)
        {
            refused += 1;
        }
    }
    assert_eq!(refused, 1);
    // Even the right password, for a while.
    assert_eq!(
        call(&mut pc, login(PASSWORD, None)).await,
        error(AccountError::TooManyAttempts)
    );
}

#[tokio::test]
async fn the_server_keeps_only_a_chain_that_holds() {
    use hext_signal::chain::{Entry, Op, recovery_key};
    let (url, inbox) = server().await;
    let key = DeviceKey::generate().unwrap();
    let mut pc = device(&url, &key).await;
    account(&mut pc, &inbox).await;
    token(call(&mut pc, login(PASSWORD, None)).await);
    let recovery = recovery_key("ABCD-EFGH-JKLM-NPQR-STUV").public_key();
    let first = Entry::create(&key, "PC", recovery);
    let append = |entry: &Entry| AccountCall::ChainAppend {
        entry: entry.clone(),
    };

    // Without 2FA, no chain.
    assert_eq!(
        call(&mut pc, append(&first)).await,
        error(AccountError::NeedsTotp)
    );
    let AccountReply::Totp { secret, .. } = call(&mut pc, AccountCall::TotpSetup).await else {
        panic!("no secret");
    };
    let code = totp_code(&secret, now()).unwrap();
    call(&mut pc, AccountCall::TotpEnable { code }).await;

    // Only this device starts it, by itself.
    let other = DeviceKey::generate().unwrap();
    assert_eq!(
        call(&mut pc, append(&Entry::create(&other, "Outro", recovery))).await,
        error(AccountError::ChainInvalid)
    );
    assert_eq!(call(&mut pc, append(&first)).await, AccountReply::Ok);
    assert_eq!(
        call(&mut pc, append(&first)).await,
        error(AccountError::ChainConflict)
    );
    let AccountReply::Chain { entries } = call(&mut pc, AccountCall::ChainGet).await else {
        panic!("no chain");
    };
    let chain = hext_signal::chain::Chain::verify(&entries).unwrap();
    // An entry signed by a key not in the account is refused.
    let intruder = chain.sign(
        Op::AddDevice {
            device: other.public_key(),
            name: "Intruso".into(),
        },
        &other,
    );
    assert_eq!(
        call(&mut pc, append(&intruder)).await,
        error(AccountError::ChainInvalid)
    );
    // A device asks to join only for itself.
    let join = AccountCall::ChainJoin {
        device: other.public_key(),
        name: "Outro".into(),
        proof: [0; 32],
    };
    assert_eq!(call(&mut pc, join).await, error(AccountError::ChainInvalid));
    let own = AccountCall::ChainJoin {
        device: key.public_key(),
        name: "PC".into(),
        proof: [1; 32],
    };
    assert_eq!(call(&mut pc, own).await, AccountReply::Ok);
    let AccountReply::Joins { joins } = call(&mut pc, AccountCall::ChainPending).await else {
        panic!("no joins");
    };
    assert_eq!(joins.len(), 1);
    // Another account cannot see it.
    let mut stranger = device(&url, &DeviceKey::generate().unwrap()).await;
    assert_eq!(
        call(&mut stranger, AccountCall::ChainGet).await,
        error(AccountError::NotLoggedIn)
    );
}

#[tokio::test]
async fn deleting_the_account_takes_everything_with_it() {
    use hext_signal::chain::{Entry, recovery_key};
    let (url, inbox) = server().await;
    let key = DeviceKey::generate().unwrap();
    let mut pc = device(&url, &key).await;
    let mut notebook = device(&url, &DeviceKey::generate().unwrap()).await;
    account(&mut pc, &inbox).await;
    token(call(&mut pc, login(PASSWORD, None)).await);
    let AccountReply::Totp { secret, .. } = call(&mut pc, AccountCall::TotpSetup).await else {
        panic!("no secret");
    };
    let code = totp_code(&secret, now()).unwrap();
    let AccountReply::RecoveryCodes { codes } =
        call(&mut pc, AccountCall::TotpEnable { code }).await
    else {
        panic!("no recovery codes");
    };
    let recovery = recovery_key("ABCD-EFGH-JKLM-NPQR-STUV").public_key();
    let first = Entry::create(&key, "PC", recovery);
    assert_eq!(
        call(&mut pc, AccountCall::ChainAppend { entry: first }).await,
        AccountReply::Ok
    );
    let theirs = token(call(&mut notebook, login(PASSWORD, Some(&codes[0]))).await);

    let delete = |password: &str, second: Option<&str>| AccountCall::DeleteAccount {
        password: password.into(),
        second: second.map(Into::into),
    };
    assert_eq!(
        call(&mut pc, delete("senha errada errada", Some(&codes[1]))).await,
        error(AccountError::WrongCredentials)
    );
    assert_eq!(
        call(&mut pc, delete(PASSWORD, None)).await,
        AccountReply::NeedsSecondFactor
    );
    assert_eq!(
        call(&mut pc, delete(PASSWORD, Some("000000"))).await,
        error(AccountError::WrongCode)
    );
    assert_eq!(
        call(&mut pc, delete(PASSWORD, Some(&codes[1]))).await,
        AccountReply::Ok
    );

    // Nobody is logged in to it any more, and it does not log in.
    assert_eq!(
        call(&mut pc, AccountCall::Me).await,
        error(AccountError::NotLoggedIn)
    );
    assert_eq!(
        call(&mut notebook, AccountCall::Me).await,
        error(AccountError::NotLoggedIn)
    );
    assert_eq!(
        call(&mut notebook, AccountCall::Resume { token: theirs }).await,
        error(AccountError::NotLoggedIn)
    );
    assert_eq!(
        call(&mut pc, login(PASSWORD, Some(&codes[2]))).await,
        error(AccountError::WrongCredentials)
    );
    // The address is free: a new account starts with no chain.
    account(&mut pc, &inbox).await;
    token(call(&mut pc, login(PASSWORD, None)).await);
    assert_eq!(
        call(&mut pc, AccountCall::ChainGet).await,
        AccountReply::Chain { entries: vec![] }
    );
}
