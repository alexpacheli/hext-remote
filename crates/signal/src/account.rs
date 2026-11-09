//! Accounts (F5): calls an app makes on its signaling connection, and the
//! server's replies. The connection is already the device's own (it signed
//! the challenge), so a login holds only for the device that made it: a
//! token copied to another device is worth nothing.

use hext_crypto::PublicKey;
use serde::{Deserialize, Serialize};

use crate::chain::Entry;

/// Shortest and longest password taken.
pub const MIN_PASSWORD: usize = 10;
pub const MAX_PASSWORD: usize = 128;
/// Longest e-mail address taken.
pub const MAX_EMAIL: usize = 254;

/// What an app asks about accounts. (No `Debug`: passwords.)
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "call", rename_all = "snake_case")]
pub enum AccountCall {
    /// A new account; a code goes to `email` to confirm it. The answer is
    /// [`AccountReply::Ok`] whether or not the address already has one, so
    /// accounts cannot be probed.
    Register { email: String, password: String },
    /// The code sent to confirm `email`, and the password: the one set is
    /// this, given with the code, so whoever registered the address before
    /// its owner does not choose it.
    Verify {
        email: String,
        code: String,
        password: String,
    },
    /// Logs this device in. `second`: the 2FA code, or a recovery code, when
    /// the account has 2FA (without it, the answer is
    /// [`AccountReply::NeedsSecondFactor`]).
    Login {
        email: String,
        password: String,
        #[serde(default)]
        second: Option<String>,
    },
    /// Back on a new connection, with the token of an earlier login of this
    /// same device.
    Resume { token: String },
    /// Logs this device out.
    Logout,
    /// The account this device is logged in to.
    Me,
    /// Starts turning on 2FA: a new secret for the authenticator app.
    TotpSetup,
    /// Finishes it with a code from the app; the answer is the recovery
    /// codes, shown once.
    TotpEnable { code: String },
    /// Forgot the password: a code goes to `email` (answered the same either
    /// way).
    ForgotPassword { email: String },
    /// A new password, with the code sent by [`AccountCall::ForgotPassword`].
    /// Every device is logged out.
    ResetPassword {
        email: String,
        code: String,
        password: String,
    },
    /// A new password, knowing the old one. The other devices are logged
    /// out.
    ChangePassword { old: String, new: String },
    /// The account's chain ([`crate::chain`]); empty if it has none yet.
    ChainGet,
    /// One more entry at the end of the chain. The first ([`crate::chain::Op::Create`])
    /// takes 2FA on, and this device signing it.
    ChainAppend { entry: Entry },
    /// This device asks to join the chain, with the proof made with the code
    /// shown on a device already in it ([`crate::chain::join_proof`]).
    ChainJoin {
        #[serde(with = "crate::hex_key")]
        device: PublicKey,
        name: String,
        #[serde(with = "crate::hex_array")]
        proof: [u8; 32],
    },
    /// The devices asking to join (for the one showing the code).
    ChainPending,
    /// For a computer (by its host key, the one logged in; no account
    /// login): the chains it is in, and those of the accounts they share it
    /// with. It checks them itself. `known`: chains it is bound to, wanted
    /// even if it is no longer on their list (to see that it is not).
    ChainHost {
        #[serde(default)]
        known: Vec<crate::chain::ChainId>,
    },
    /// The chains of the accounts that share a computer with this one (the
    /// one logged in), as [`AccountReply::HostChains`]. Only for listing:
    /// the computer decides who comes in.
    ChainShared,
    /// For a computer (no account login): entries of its audit, for the
    /// Atividade screen of the accounts that list it. At most
    /// [`MAX_EVENTS`] a call, each up to [`MAX_EVENT_LEN`] characters.
    HostEvents {
        events: Vec<String>,
        /// The account this computer is bound to: only it gets them.
        #[serde(default)]
        chain: Option<crate::chain::ChainId>,
    },
    /// The latest entries of the account's computers, the latest first.
    Activity,
    /// Deletes the account this device is logged in to, with everything the
    /// server keeps of it: its chain, its computers' activity, its logins.
    /// Takes the password and, with 2FA on, `second` (the 2FA code or a
    /// recovery code; without it, the answer is
    /// [`AccountReply::NeedsSecondFactor`]). The computers bound to it stop
    /// letting anyone in by it: its chain is gone.
    DeleteAccount {
        password: String,
        #[serde(default)]
        second: Option<String>,
    },
}

/// A device asking to join the chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Join {
    #[serde(with = "crate::hex_key")]
    pub device: PublicKey,
    pub name: String,
    #[serde(with = "crate::hex_array")]
    pub proof: [u8; 32],
}

/// The account as its owner sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountView {
    pub email: String,
    /// 2FA is on.
    pub totp: bool,
}

/// Entries a computer sends at once, and how long each may be.
pub const MAX_EVENTS: usize = 20;
pub const MAX_EVENT_LEN: usize = 300;

/// An entry of a computer's audit, as the server keeps it for the account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityEvent {
    /// When the server got it (Unix time).
    pub at: u64,
    /// The computer that sent it.
    pub computer: crate::DeviceId,
    pub text: String,
}

/// The server's answer to an [`AccountCall`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum AccountReply {
    Ok,
    /// Logged in: keep `token` to [`AccountCall::Resume`] later (it is good
    /// only for this device).
    LoggedIn {
        token: String,
        account: AccountView,
    },
    /// The password was right; the 2FA code (or a recovery code) is next.
    NeedsSecondFactor,
    Account(AccountView),
    /// For the authenticator app: the secret (base 32) and the same as a
    /// link (for a QR code).
    Totp {
        secret: String,
        uri: String,
    },
    /// Each good once; shown only now.
    RecoveryCodes {
        codes: Vec<String>,
    },
    /// The chain's entries, from the first (none: no chain yet).
    Chain {
        entries: Vec<Entry>,
    },
    /// Devices asking to join.
    Joins {
        joins: Vec<Join>,
    },
    /// The chains of [`AccountCall::ChainHost`].
    HostChains {
        chains: Vec<Vec<Entry>>,
    },
    /// The entries of [`AccountCall::Activity`].
    Activity {
        events: Vec<ActivityEvent>,
    },
    Error {
        error: AccountError,
    },
}

/// Why an [`AccountCall`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountError {
    /// Not an e-mail address we take.
    InvalidEmail,
    /// Too short or too long (see [`MIN_PASSWORD`]).
    WeakPassword,
    /// The e-mail or the password is wrong (which one is not said).
    WrongCredentials,
    /// The e-mail address is not confirmed yet.
    NotVerified,
    /// The code is wrong, used or expired.
    WrongCode,
    /// Too many attempts: wait a while.
    TooManyAttempts,
    /// This device is not logged in.
    NotLoggedIn,
    /// 2FA is already on.
    AlreadyEnabled,
    /// [`AccountCall::TotpEnable`] without [`AccountCall::TotpSetup`] first.
    NoSetup,
    /// Something went wrong on the server.
    ServerError,
    /// The chain changed meanwhile: get it again ([`AccountCall::ChainGet`]).
    ChainConflict,
    /// That entry does not belong at the end of the chain.
    ChainInvalid,
    /// Computers in the account take 2FA on first.
    NeedsTotp,
}

/// Which call, without what it carries (passwords, codes).
impl std::fmt::Debug for AccountCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Register { .. } => "Register",
            Self::Verify { .. } => "Verify",
            Self::Login { .. } => "Login",
            Self::Resume { .. } => "Resume",
            Self::Logout => "Logout",
            Self::Me => "Me",
            Self::TotpSetup => "TotpSetup",
            Self::TotpEnable { .. } => "TotpEnable",
            Self::ForgotPassword { .. } => "ForgotPassword",
            Self::ResetPassword { .. } => "ResetPassword",
            Self::ChangePassword { .. } => "ChangePassword",
            Self::ChainGet => "ChainGet",
            Self::ChainAppend { .. } => "ChainAppend",
            Self::ChainJoin { .. } => "ChainJoin",
            Self::ChainPending => "ChainPending",
            Self::ChainHost { .. } => "ChainHost",
            Self::ChainShared => "ChainShared",
            Self::HostEvents { .. } => "HostEvents",
            Self::Activity => "Activity",
            Self::DeleteAccount { .. } => "DeleteAccount",
        })
    }
}

impl AccountReply {
    pub fn error(error: AccountError) -> Self {
        Self::Error { error }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_and_replies_go_as_tagged_json() {
        let call = AccountCall::Login {
            email: "a@b.com".into(),
            password: "segredo segredo".into(),
            second: None,
        };
        let json = serde_json::to_string(&call).unwrap();
        assert!(json.contains(r#""call":"login""#), "{json}");
        assert!(serde_json::from_str::<AccountCall>(&json).unwrap() == call);
        // `second` may be left out.
        let short = r#"{"call":"login","email":"a@b.com","password":"x"}"#;
        assert!(matches!(
            serde_json::from_str::<AccountCall>(short).unwrap(),
            AccountCall::Login { second: None, .. }
        ));
        let reply = AccountReply::error(AccountError::TooManyAttempts);
        let json = serde_json::to_string(&reply).unwrap();
        assert_eq!(json, r#"{"reply":"error","error":"too_many_attempts"}"#);
        assert_eq!(serde_json::from_str::<AccountReply>(&json).unwrap(), reply);
        let me = AccountReply::Account(AccountView {
            email: "a@b.com".into(),
            totp: true,
        });
        let json = serde_json::to_string(&me).unwrap();
        assert_eq!(serde_json::from_str::<AccountReply>(&json).unwrap(), me);
    }
}
