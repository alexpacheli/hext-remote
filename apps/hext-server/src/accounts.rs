//! Accounts (F5, marco 1): e-mail and password (Argon2id), the address
//! confirmed by a code, 2FA by TOTP with recovery codes, and logins held by
//! one device each. The calls come on the device's signaling connection
//! ([`hext_signal::AccountCall`]): it already proved its key, so a login is
//! that device's alone.
//!
//! Nothing secret is kept as it is: the password as an Argon2id hash, codes
//! and tokens as SHA-256 (they are random and long, so that is enough).

use std::net::IpAddr;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use hext_signal::account::{MAX_EMAIL, MAX_PASSWORD, MIN_PASSWORD};
use hext_signal::chain::{Chain, Entry, Op};
use hext_signal::{AccountCall, AccountError, AccountReply, AccountView, DeviceId, Join};
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{Connection, OptionalExtension, params};
use tracing::{info, warn};

use crate::db::Db;
use crate::signal::State;

/// How long an e-mailed code is good, and how many guesses it takes.
const CODE_LIFETIME: i64 = 30 * 60;
const CODE_TRIES: i64 = 5;
/// Recovery codes handed out with 2FA.
const RECOVERY_CODES: usize = 10;
/// TOTP: 30-second steps, 6 digits, one step of clock drift either way.
const TOTP_STEP: u64 = 30;
/// Wrong guesses (a password, a code) and e-mails sent, per address and per
/// e-mail address. A right guess does not count.
pub(crate) const GUESSES_PER_IP: usize = 30;
pub(crate) const GUESSES_PER_EMAIL: usize = 10;
pub(crate) const GUESS_WINDOW: Duration = Duration::from_secs(15 * 60);
/// Longest code or token taken (anything longer is wrong anyway).
const MAX_CODE: usize = 128;

/// Events kept for each account (the latest).
const KEPT_EVENTS: i64 = 500;

/// Where e-mails go.
pub trait Mailer: Send + Sync {
    fn send(&self, to: &str, subject: &str, body: &str);
}

/// E-mails written to the log, until a real sender is set up (F5 decision
/// 3). The codes show there: for tests and trials only.
pub struct LogMailer;

impl Mailer for LogMailer {
    fn send(&self, to: &str, subject: &str, body: &str) {
        info!("e-mail para {to}: {subject}\n{body}");
    }
}

pub(crate) fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS accounts (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             email TEXT NOT NULL UNIQUE,
             password TEXT NOT NULL,
             verified INTEGER NOT NULL DEFAULT 0,
             totp_secret BLOB,
             totp_pending BLOB,
             totp_last_step INTEGER NOT NULL DEFAULT 0,
             created_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS account_codes (
             account_id INTEGER NOT NULL,
             purpose TEXT NOT NULL,
             code_hash BLOB NOT NULL,
             expires INTEGER NOT NULL,
             tries INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY (account_id, purpose)
         ) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS recovery_codes (
             account_id INTEGER NOT NULL,
             code_hash BLOB NOT NULL,
             PRIMARY KEY (account_id, code_hash)
         ) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS account_chain (
             account_id INTEGER NOT NULL,
             seq INTEGER NOT NULL,
             entry TEXT NOT NULL,
             PRIMARY KEY (account_id, seq)
         ) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS account_members (
             account_id INTEGER NOT NULL,
             public_key BLOB NOT NULL,
             PRIMARY KEY (account_id, public_key)
         ) WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS account_members_by_key ON account_members (public_key);
         CREATE TABLE IF NOT EXISTS account_computers (
             account_id INTEGER NOT NULL,
             device_id INTEGER NOT NULL,
             PRIMARY KEY (account_id, device_id)
         ) WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS account_computers_by_id ON account_computers (device_id);
         CREATE TABLE IF NOT EXISTS account_chain_ids (
             account_id INTEGER PRIMARY KEY,
             chain_id BLOB NOT NULL UNIQUE
         );
         CREATE TABLE IF NOT EXISTS account_shares (
             account_id INTEGER NOT NULL,
             device_id INTEGER NOT NULL,
             with_chain BLOB NOT NULL,
             PRIMARY KEY (account_id, device_id, with_chain)
         ) WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS account_shares_by_id ON account_shares (device_id);
         CREATE TABLE IF NOT EXISTS account_events (
             account_id INTEGER NOT NULL,
             at INTEGER NOT NULL,
             device_id INTEGER NOT NULL,
             text TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS account_events_by_account ON account_events (account_id, at);
         CREATE TABLE IF NOT EXISTS account_sessions (
             token_hash BLOB PRIMARY KEY,
             account_id INTEGER NOT NULL,
             device_id INTEGER NOT NULL,
             created_at INTEGER NOT NULL,
             last_used INTEGER NOT NULL
         ) WITHOUT ROWID;",
    )
}

/// An account as stored.
struct Account {
    id: i64,
    email: String,
    password: String,
    verified: bool,
    totp: Option<Vec<u8>>,
    totp_pending: Option<Vec<u8>>,
    totp_last_step: u64,
}

impl Account {
    fn view(&self) -> AccountView {
        AccountView {
            email: self.email.clone(),
            totp: self.totp.is_some(),
        }
    }
}

/// How a code went.
#[derive(Debug, PartialEq, Eq)]
enum Code {
    Right,
    Wrong,
}

impl Db {
    fn account_where(
        &self,
        column: &str,
        value: &dyn rusqlite::ToSql,
    ) -> rusqlite::Result<Option<Account>> {
        let conn = self.conn();
        conn.query_row(
            &format!(
                "SELECT id, email, password, verified, totp_secret, totp_pending, totp_last_step
                 FROM accounts WHERE {column} = ?1"
            ),
            [value],
            |row| {
                Ok(Account {
                    id: row.get(0)?,
                    email: row.get(1)?,
                    password: row.get(2)?,
                    verified: row.get(3)?,
                    totp: row.get(4)?,
                    totp_pending: row.get(5)?,
                    totp_last_step: row.get::<_, i64>(6)?.try_into().unwrap_or(0),
                })
            },
        )
        .optional()
    }

    fn account_by_email(&self, email: &str) -> rusqlite::Result<Option<Account>> {
        self.account_where("email", &email)
    }

    fn account_by_id(&self, id: i64) -> rusqlite::Result<Option<Account>> {
        self.account_where("id", &id)
    }

    /// A new account, not confirmed; `None` if the address has one already.
    fn create_account(&self, email: &str, password: &str) -> rusqlite::Result<Option<i64>> {
        let conn = self.conn();
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO accounts (email, password, created_at) VALUES (?1, ?2, ?3)",
            params![email, password, crate::db::now()],
        )?;
        Ok((inserted == 1).then(|| conn.last_insert_rowid()))
    }

    fn set_code(&self, account: i64, purpose: &str, code: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO account_codes (account_id, purpose, code_hash, expires, tries)
             VALUES (?1, ?2, ?3, ?4, 0)",
            params![
                account,
                purpose,
                sha256(code.as_bytes()),
                crate::db::now() + CODE_LIFETIME
            ],
        )?;
        Ok(())
    }

    /// Checks `code` for `purpose`: used up when right, and after
    /// [`CODE_TRIES`] wrong ones.
    fn check_code(&self, account: i64, purpose: &str, code: &str) -> rusqlite::Result<Code> {
        let conn = self.conn();
        let stored: Option<(Vec<u8>, i64, i64)> = conn
            .query_row(
                "SELECT code_hash, expires, tries FROM account_codes
                 WHERE account_id = ?1 AND purpose = ?2",
                params![account, purpose],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((hash, expires, tries)) = stored else {
            return Ok(Code::Wrong);
        };
        let right = hext_crypto::constant_time_eq(&hash, &sha256(code.as_bytes()));
        if right && expires > crate::db::now() && tries < CODE_TRIES {
            conn.execute(
                "DELETE FROM account_codes WHERE account_id = ?1 AND purpose = ?2",
                params![account, purpose],
            )?;
            return Ok(Code::Right);
        }
        if tries + 1 >= CODE_TRIES || expires <= crate::db::now() {
            conn.execute(
                "DELETE FROM account_codes WHERE account_id = ?1 AND purpose = ?2",
                params![account, purpose],
            )?;
        } else {
            conn.execute(
                "UPDATE account_codes SET tries = tries + 1 WHERE account_id = ?1 AND purpose = ?2",
                params![account, purpose],
            )?;
        }
        Ok(Code::Wrong)
    }

    fn set_verified(&self, account: i64) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE accounts SET verified = 1 WHERE id = ?1",
            params![account],
        )?;
        Ok(())
    }

    fn set_password(&self, account: i64, password: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE accounts SET password = ?2 WHERE id = ?1",
            params![account, password],
        )?;
        Ok(())
    }

    /// A login of `device`, for `token`: its only one (an earlier login of
    /// the device ends).
    fn open_session(&self, token: &str, account: i64, device: DeviceId) -> rusqlite::Result<()> {
        let now = crate::db::now();
        let conn = self.conn();
        conn.execute(
            "DELETE FROM account_sessions WHERE device_id = ?1",
            params![device.get()],
        )?;
        conn.execute(
            "INSERT INTO account_sessions (token_hash, account_id, device_id, created_at, last_used)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![sha256(token.as_bytes()), account, device.get(), now],
        )?;
        Ok(())
    }

    /// The account of `token`, if it is a login of `device`.
    fn session(&self, token: &str, device: DeviceId) -> rusqlite::Result<Option<i64>> {
        let conn = self.conn();
        let hash = sha256(token.as_bytes());
        let account: Option<i64> = conn
            .query_row(
                "SELECT account_id FROM account_sessions WHERE token_hash = ?1 AND device_id = ?2",
                params![hash, device.get()],
                |row| row.get(0),
            )
            .optional()?;
        if account.is_some() {
            conn.execute(
                "UPDATE account_sessions SET last_used = ?2 WHERE token_hash = ?1",
                params![hash, crate::db::now()],
            )?;
        }
        Ok(account)
    }

    /// Whether `device` is still logged in to `account` (another device may
    /// have ended it: a new password).
    fn has_session(&self, account: i64, device: DeviceId) -> rusqlite::Result<bool> {
        self.conn()
            .query_row(
                "SELECT 1 FROM account_sessions WHERE account_id = ?1 AND device_id = ?2",
                params![account, device.get()],
                |_| Ok(()),
            )
            .optional()
            .map(|found| found.is_some())
    }

    /// Logs out `account` everywhere but `keep` (or everywhere).
    fn close_sessions(&self, account: i64, keep: Option<DeviceId>) -> rusqlite::Result<()> {
        let keep = keep.map_or(0, DeviceId::get);
        self.conn().execute(
            "DELETE FROM account_sessions WHERE account_id = ?1 AND device_id != ?2",
            params![account, keep],
        )?;
        Ok(())
    }

    fn close_session(&self, account: i64, device: DeviceId) -> rusqlite::Result<()> {
        self.conn().execute(
            "DELETE FROM account_sessions WHERE account_id = ?1 AND device_id = ?2",
            params![account, device.get()],
        )?;
        Ok(())
    }

    fn set_totp_pending(&self, account: i64, secret: &[u8]) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE accounts SET totp_pending = ?2 WHERE id = ?1",
            params![account, secret],
        )?;
        Ok(())
    }

    /// Turns 2FA on with `secret` (its code for `step` just used), and
    /// replaces the recovery codes.
    fn enable_totp(
        &self,
        account: i64,
        secret: &[u8],
        step: u64,
        recovery: &[String],
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE accounts SET totp_secret = ?2, totp_pending = NULL, totp_last_step = ?3
             WHERE id = ?1",
            params![account, secret, step as i64],
        )?;
        tx.execute(
            "DELETE FROM recovery_codes WHERE account_id = ?1",
            params![account],
        )?;
        for code in recovery {
            tx.execute(
                "INSERT INTO recovery_codes (account_id, code_hash) VALUES (?1, ?2)",
                params![account, sha256(code.as_bytes())],
            )?;
        }
        tx.commit()
    }

    /// The TOTP code of `step` was used: none of that step or before is good
    /// anymore.
    fn set_totp_step(&self, account: i64, step: u64) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE accounts SET totp_last_step = ?2 WHERE id = ?1",
            params![account, step as i64],
        )?;
        Ok(())
    }

    /// Uses up recovery code `code`; whether it was one.
    fn use_recovery(&self, account: i64, code: &str) -> rusqlite::Result<bool> {
        let removed = self.conn().execute(
            "DELETE FROM recovery_codes WHERE account_id = ?1 AND code_hash = ?2",
            params![account, sha256(code.as_bytes())],
        )?;
        Ok(removed == 1)
    }
}

/// The account's chain (marco 3).
impl Db {
    fn chain(&self, account: i64) -> rusqlite::Result<Vec<Entry>> {
        let conn = self.conn();
        let mut query =
            conn.prepare("SELECT entry FROM account_chain WHERE account_id = ?1 ORDER BY seq")?;
        let rows = query.query_map(params![account], |row| row.get::<_, String>(0))?;
        let mut entries = Vec::new();
        for row in rows {
            let entry = serde_json::from_str(&row?).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
            })?;
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Who is in the account now, as its chain says: for presence (which
    /// computers a device may see online). Only that: access is the
    /// computers' own call, on the chain itself.
    fn index(&self, account: i64, chain: &Chain) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM account_members WHERE account_id = ?1",
            params![account],
        )?;
        tx.execute(
            "DELETE FROM account_computers WHERE account_id = ?1",
            params![account],
        )?;
        for device in chain.devices() {
            tx.execute(
                "INSERT OR IGNORE INTO account_members (account_id, public_key) VALUES (?1, ?2)",
                params![account, device.key.as_bytes().as_slice()],
            )?;
        }
        // Only a computer whose ID is its key's: a list naming somebody
        // else's ID gets neither its presence nor its activity.
        for computer in chain.computers() {
            tx.execute(
                "INSERT OR IGNORE INTO account_computers (account_id, device_id)
                 SELECT ?1, id FROM devices WHERE id = ?2 AND public_key = ?3",
                params![
                    account,
                    computer.id.get(),
                    computer.host.as_bytes().as_slice()
                ],
            )?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO account_chain_ids (account_id, chain_id) VALUES (?1, ?2)",
            params![account, chain.id.as_slice()],
        )?;
        tx.execute(
            "DELETE FROM account_shares WHERE account_id = ?1",
            params![account],
        )?;
        for share in chain.shares() {
            let Some(computer) = chain.computer(&share.host) else {
                continue;
            };
            tx.execute(
                "INSERT OR IGNORE INTO account_shares (account_id, device_id, with_chain)
                 SELECT ?1, id, ?3 FROM devices WHERE id = ?2 AND public_key = ?4",
                params![
                    account,
                    computer.id.get(),
                    share.with.as_slice(),
                    computer.host.as_bytes().as_slice()
                ],
            )?;
        }
        tx.commit()
    }

    /// Keeps `events` of computer `host` for the account of chain `chain`,
    /// if it lists the computer (the latest [`KEPT_EVENTS`]).
    fn record_events(
        &self,
        host: DeviceId,
        chain: &[u8; 32],
        at: i64,
        events: &[String],
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let accounts: Vec<i64> = {
            let mut query = tx.prepare(
                "SELECT c.account_id FROM account_computers c
                 JOIN account_chain_ids i ON i.account_id = c.account_id
                 WHERE c.device_id = ?1 AND i.chain_id = ?2",
            )?;
            let rows = query.query_map(params![host.get(), chain.as_slice()], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for account in accounts {
            for text in events {
                tx.execute(
                    "INSERT INTO account_events (account_id, at, device_id, text)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![account, at, host.get(), text],
                )?;
            }
            tx.execute(
                "DELETE FROM account_events WHERE account_id = ?1 AND rowid NOT IN (
                     SELECT rowid FROM account_events WHERE account_id = ?1
                     ORDER BY at DESC, rowid DESC LIMIT ?2)",
                params![account, KEPT_EVENTS],
            )?;
        }
        tx.commit()
    }

    /// The latest events of `account`'s computers, the latest first.
    fn events(&self, account: i64) -> rusqlite::Result<Vec<hext_signal::account::ActivityEvent>> {
        let conn = self.conn();
        let mut query = conn.prepare(
            "SELECT at, device_id, text FROM account_events WHERE account_id = ?1
             ORDER BY at DESC, rowid DESC LIMIT ?2",
        )?;
        let rows = query.query_map(params![account, KEPT_EVENTS], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (at, device, text) = row?;
            if let Some(computer) = DeviceId::new(device) {
                events.push(hext_signal::account::ActivityEvent {
                    at: at as u64,
                    computer,
                    text,
                });
            }
        }
        Ok(events)
    }

    /// The accounts that share a computer with `account`.
    fn sharing_with(&self, account: i64) -> rusqlite::Result<Vec<i64>> {
        let conn = self.conn();
        let mut query = conn.prepare(
            "SELECT DISTINCT s.account_id FROM account_shares s
             JOIN account_chain_ids i ON i.chain_id = s.with_chain
             WHERE i.account_id = ?1",
        )?;
        let rows = query.query_map(params![account], |row| row.get(0))?;
        rows.collect()
    }

    fn account_of_chain(&self, id: &[u8; 32]) -> rusqlite::Result<Option<i64>> {
        self.conn()
            .query_row(
                "SELECT account_id FROM account_chain_ids WHERE chain_id = ?1",
                params![id.as_slice()],
                |row| row.get(0),
            )
            .optional()
    }

    /// The accounts `account` shares computer `host` (by ID) with.
    fn shared_by(&self, account: i64, host: DeviceId) -> rusqlite::Result<Vec<i64>> {
        let conn = self.conn();
        let mut query = conn.prepare(
            "SELECT DISTINCT i.account_id FROM account_shares s
             JOIN account_chain_ids i ON i.chain_id = s.with_chain
             WHERE s.account_id = ?1 AND s.device_id = ?2",
        )?;
        let rows = query.query_map(params![account, host.get()], |row| row.get(0))?;
        rows.collect()
    }

    /// Whether `viewer` is a device of an account that has computer `host`.
    pub(crate) fn same_account(&self, host: DeviceId, viewer: DeviceId) -> rusqlite::Result<bool> {
        self.conn()
            .query_row(
                "SELECT 1 FROM account_computers c
                 JOIN account_members m ON m.account_id = c.account_id
                 JOIN devices v ON v.public_key = m.public_key
                 WHERE c.device_id = ?1 AND v.id = ?2
                 UNION
                 SELECT 1 FROM account_shares s
                 JOIN account_chain_ids i ON i.chain_id = s.with_chain
                 JOIN account_members m ON m.account_id = i.account_id
                 JOIN devices v ON v.public_key = m.public_key
                 WHERE s.device_id = ?1 AND v.id = ?2",
                params![host.get(), viewer.get()],
                |_| Ok(()),
            )
            .optional()
            .map(|found| found.is_some())
    }

    /// Adds `entry` as number `seq`; false if that place is taken (someone
    /// else added one meanwhile).
    fn append(&self, account: i64, entry: &Entry) -> rusqlite::Result<bool> {
        let json = serde_json::to_string(entry).expect("entries serialize");
        let added = self.conn().execute(
            "INSERT OR IGNORE INTO account_chain (account_id, seq, entry) VALUES (?1, ?2, ?3)",
            params![account, entry.seq as i64, json],
        )?;
        Ok(added == 1)
    }
}

/// Devices asking to join one account's chain, for a while.
#[derive(Default)]
pub(crate) struct Joins(std::sync::Mutex<std::collections::HashMap<i64, Vec<(Join, Instant)>>>);

/// How long a request to join waits, and how many one account takes.
const JOIN_LIFETIME: Duration = Duration::from_secs(10 * 60);
const MAX_JOINS: usize = 5;

impl Joins {
    fn add(&self, account: i64, join: Join, now: Instant) {
        let mut all = self.0.lock().expect("lock");
        let list = all.entry(account).or_default();
        list.retain(|(j, at)| j.device != join.device && now - *at < JOIN_LIFETIME);
        if list.len() >= MAX_JOINS {
            list.remove(0);
        }
        list.push((join, now));
    }

    fn of(&self, account: i64, now: Instant) -> Vec<Join> {
        let mut all = self.0.lock().expect("lock");
        let Some(list) = all.get_mut(&account) else {
            return Vec::new();
        };
        list.retain(|(_, at)| now - *at < JOIN_LIFETIME);
        list.iter().map(|(j, _)| j.clone()).collect()
    }

    fn joined(&self, account: i64, device: &hext_crypto::PublicKey) {
        let mut all = self.0.lock().expect("lock");
        if let Some(list) = all.get_mut(&account) {
            list.retain(|(j, _)| j.device != *device);
            if list.is_empty() {
                all.remove(&account);
            }
        }
    }

    /// Forgets old requests (call now and then: memory).
    pub(crate) fn prune(&self, now: Instant) {
        let mut all = self.0.lock().expect("lock");
        all.retain(|_, list| {
            list.retain(|(_, at)| now - *at < JOIN_LIFETIME);
            !list.is_empty()
        });
    }
}

/// Why a call failed.
enum Fail {
    Reply(AccountError),
    Db(rusqlite::Error),
}

impl From<rusqlite::Error> for Fail {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e)
    }
}

impl From<AccountError> for Fail {
    fn from(e: AccountError) -> Self {
        Self::Reply(e)
    }
}

type Answer = Result<AccountReply, Fail>;

/// The connection a call came on: which device, from where, and the account
/// it is logged in to.
pub(crate) struct Caller<'a> {
    pub device: DeviceId,
    /// Its key (what it signs the chain with, as a technician).
    pub key: hext_crypto::PublicKey,
    pub ip: IpAddr,
    pub login: &'a mut Option<i64>,
}

/// Answers `call`.
pub(crate) async fn handle(state: &State, caller: Caller<'_>, call: AccountCall) -> AccountReply {
    let ip = caller.ip;
    // The address a password or code was tried for: counted if wrong.
    let mut guessed = None;
    let answer = run(state, caller, call, &mut guessed).await;
    if let (
        Some(email),
        Err(Fail::Reply(AccountError::WrongCredentials | AccountError::WrongCode)),
    ) = (&guessed, &answer)
    {
        count(state, ip, email);
    }
    match answer {
        Ok(reply) => reply,
        Err(Fail::Reply(error)) => AccountReply::error(error),
        Err(Fail::Db(e)) => {
            warn!("contas: banco de dados: {e}");
            AccountReply::error(AccountError::ServerError)
        }
    }
}

async fn run(
    state: &State,
    caller: Caller<'_>,
    call: AccountCall,
    guessed: &mut Option<String>,
) -> Answer {
    let Caller {
        device,
        key,
        ip,
        login,
    } = caller;
    match call {
        AccountCall::Register { email, password } => {
            let email = clean_email(&email).ok_or(AccountError::InvalidEmail)?;
            check_password(&password)?;
            sends(state, ip, &email)?;
            let hash = blocking(move || hash_password(&password)).await;
            let created = {
                let email = email.clone();
                state.db(move |db| db.create_account(&email, &hash)).await?
            };
            let account = match created {
                Some(id) => Some(id),
                None => {
                    let found = {
                        let email = email.clone();
                        state.db(move |db| db.account_by_email(&email)).await?
                    };
                    match found {
                        // Created and never confirmed: a new code (the
                        // password is set when it is confirmed).
                        Some(account) if !account.verified => Some(account.id),
                        Some(_) => {
                            state.mail(
                                &email,
                                "Hext Remote: você já tem uma conta",
                                "Alguém tentou criar uma conta do Hext Remote com este e-mail, que \
                                 já tem uma. Se foi você e esqueceu a senha, use \"Esqueci a \
                                 senha\" no app. Se não foi, ignore este e-mail.",
                            );
                            None
                        }
                        None => None,
                    }
                }
            };
            if let Some(account) = account {
                let code = email_code();
                {
                    let code = code.clone();
                    state
                        .db(move |db| db.set_code(account, "verify", &code))
                        .await?;
                }
                state.mail(
                    &email,
                    "Hext Remote: confirme o seu e-mail",
                    &format!(
                        "O código para confirmar a sua conta do Hext Remote é {code}. Ele vale por \
                         30 minutos. Se você não criou uma conta, ignore este e-mail."
                    ),
                );
            }
            Ok(AccountReply::Ok)
        }
        AccountCall::Verify {
            email,
            code,
            password,
        } => {
            let email = clean_email(&email).ok_or(AccountError::WrongCode)?;
            check_password(&password)?;
            *guessed = Some(limited(state, ip, &email)?);
            let code = clean_code(&code)?;
            let found = state.db(move |db| db.account_by_email(&email)).await?;
            // A confirmed account has no code to confirm: wrong, like an
            // unknown address.
            let account = found
                .filter(|a| !a.verified)
                .ok_or(AccountError::WrongCode)?;
            let id = account.id;
            let checked = state
                .db(move |db| db.check_code(id, "verify", &code))
                .await?;
            if checked != Code::Right {
                return Err(AccountError::WrongCode.into());
            }
            // The password is the one given with the code: whoever registered
            // the address first (without it) does not choose it.
            let hash = blocking(move || hash_password(&password)).await;
            state
                .db(move |db| {
                    db.set_password(id, &hash)?;
                    db.set_verified(id)
                })
                .await?;
            Ok(AccountReply::Ok)
        }
        AccountCall::Login {
            email,
            password,
            second,
        } => {
            let email = clean_email(&email).ok_or(AccountError::WrongCredentials)?;
            *guessed = Some(limited(state, ip, &email)?);
            let found = state.db(move |db| db.account_by_email(&email)).await?;
            // The same work whether or not the account exists.
            let stored = found
                .as_ref()
                .map_or_else(|| dummy_hash().to_owned(), |a| a.password.clone());
            let right = blocking(move || verify_password(&password, &stored)).await;
            let account = found
                .filter(|_| right)
                .ok_or(AccountError::WrongCredentials)?;
            if !account.verified {
                return Err(AccountError::NotVerified.into());
            }
            if let Some(secret) = &account.totp {
                let Some(second) = second else {
                    return Ok(AccountReply::NeedsSecondFactor);
                };
                second_factor(state, &account, secret, &second).await?;
            }
            let token = token();
            {
                let (token, id) = (token.clone(), account.id);
                state
                    .db(move |db| db.open_session(&token, id, device))
                    .await?;
            }
            *login = Some(account.id);
            info!("conta {}: login do dispositivo {device}", account.id);
            Ok(AccountReply::LoggedIn {
                token,
                account: account.view(),
            })
        }
        AccountCall::Resume { token } => {
            let token = clean_code(&token).map_err(|_| AccountError::NotLoggedIn)?;
            let found = state.db(move |db| db.session(&token, device)).await?;
            let account = found.ok_or(AccountError::NotLoggedIn)?;
            let account = state
                .db(move |db| db.account_by_id(account))
                .await?
                .ok_or(AccountError::NotLoggedIn)?;
            *login = Some(account.id);
            Ok(AccountReply::Account(account.view()))
        }
        AccountCall::Logout => {
            if let Some(account) = login.take() {
                state
                    .db(move |db| db.close_session(account, device))
                    .await?;
            }
            Ok(AccountReply::Ok)
        }
        AccountCall::Me => {
            let account = logged_in(state, login, device).await?;
            Ok(AccountReply::Account(account.view()))
        }
        AccountCall::TotpSetup => {
            let account = logged_in(state, login, device).await?;
            if account.totp.is_some() {
                return Err(AccountError::AlreadyEnabled.into());
            }
            let secret = random::<20>().to_vec();
            {
                let (id, secret) = (account.id, secret.clone());
                state.db(move |db| db.set_totp_pending(id, &secret)).await?;
            }
            let encoded = base32(&secret);
            let uri = format!(
                "otpauth://totp/Hext%20Remote:{}?secret={encoded}&issuer=Hext%20Remote",
                percent(&account.email)
            );
            Ok(AccountReply::Totp {
                secret: encoded,
                uri,
            })
        }
        AccountCall::TotpEnable { code } => {
            let account = logged_in(state, login, device).await?;
            if account.totp.is_some() {
                return Err(AccountError::AlreadyEnabled.into());
            }
            *guessed = Some(limited(state, ip, &account.email)?);
            let secret = account.totp_pending.clone().ok_or(AccountError::NoSetup)?;
            let step = totp_check(&secret, &code, unix_now(), 0).ok_or(AccountError::WrongCode)?;
            let codes: Vec<String> = (0..RECOVERY_CODES).map(|_| recovery_code()).collect();
            {
                let (id, stored) = (
                    account.id,
                    codes.iter().map(|c| clean_recovery(c)).collect::<Vec<_>>(),
                );
                state
                    .db(move |db| db.enable_totp(id, &secret, step, &stored))
                    .await?;
            }
            info!("conta {}: 2FA ligado", account.id);
            Ok(AccountReply::RecoveryCodes { codes })
        }
        AccountCall::ForgotPassword { email } => {
            let Some(email) = clean_email(&email) else {
                return Ok(AccountReply::Ok);
            };
            sends(state, ip, &email)?;
            let found = {
                let email = email.clone();
                state.db(move |db| db.account_by_email(&email)).await?
            };
            if let Some(account) = found.filter(|a| a.verified) {
                let code = email_code();
                {
                    let code = code.clone();
                    state
                        .db(move |db| db.set_code(account.id, "reset", &code))
                        .await?;
                }
                state.mail(
                    &email,
                    "Hext Remote: trocar a senha",
                    &format!(
                        "O código para trocar a senha da sua conta do Hext Remote é {code}. Ele \
                         vale por 30 minutos. Se você não pediu, ignore este e-mail: a senha \
                         continua a mesma."
                    ),
                );
            }
            Ok(AccountReply::Ok)
        }
        AccountCall::ResetPassword {
            email,
            code,
            password,
        } => {
            let email = clean_email(&email).ok_or(AccountError::WrongCode)?;
            check_password(&password)?;
            *guessed = Some(limited(state, ip, &email)?);
            let code = clean_code(&code)?;
            let found = state.db(move |db| db.account_by_email(&email)).await?;
            let account = found.ok_or(AccountError::WrongCode)?;
            let id = account.id;
            let checked = state
                .db(move |db| db.check_code(id, "reset", &code))
                .await?;
            if checked != Code::Right {
                return Err(AccountError::WrongCode.into());
            }
            let hash = blocking(move || hash_password(&password)).await;
            state
                .db(move |db| {
                    db.set_password(id, &hash)?;
                    db.close_sessions(id, None)
                })
                .await?;
            if *login == Some(id) {
                *login = None;
            }
            info!("conta {id}: senha trocada pelo e-mail");
            Ok(AccountReply::Ok)
        }
        AccountCall::ChainHost { known } => {
            // At most a handful; each is checked by the computer.
            let chains = state
                .db(move |db| {
                    // The chains asked for first, then the ones they share
                    // this computer with: nobody else's list crowds them out.
                    let mut accounts = Vec::new();
                    for id in known.iter().take(4) {
                        if let Some(account) = db.account_of_chain(id)?
                            && !accounts.contains(&account)
                        {
                            accounts.push(account);
                        }
                    }
                    for account in accounts.clone() {
                        for other in db.shared_by(account, device)? {
                            if !accounts.contains(&other) {
                                accounts.push(other);
                            }
                        }
                    }
                    let mut chains = Vec::new();
                    for account in accounts.into_iter().take(16) {
                        chains.push(db.chain(account)?);
                    }
                    Ok(chains)
                })
                .await?;
            Ok(AccountReply::HostChains { chains })
        }
        AccountCall::HostEvents { events, chain } => {
            // Kept only for the accounts that list this computer.
            let events: Vec<String> = events
                .into_iter()
                .take(hext_signal::account::MAX_EVENTS)
                .map(|e| {
                    e.chars()
                        .take(hext_signal::account::MAX_EVENT_LEN)
                        .collect()
                })
                .collect();
            let at = crate::db::now();
            if let Some(chain) = chain {
                state
                    .db(move |db| db.record_events(device, &chain, at, &events))
                    .await?;
            }
            Ok(AccountReply::Ok)
        }
        AccountCall::Activity => {
            let account = logged_in(state, login, device).await?;
            let id = account.id;
            let events = state.db(move |db| db.events(id)).await?;
            Ok(AccountReply::Activity { events })
        }
        AccountCall::ChainShared => {
            let account = logged_in(state, login, device).await?;
            let id = account.id;
            let chains = state
                .db(move |db| {
                    let mut chains = Vec::new();
                    for other in db.sharing_with(id)?.into_iter().take(16) {
                        chains.push(db.chain(other)?);
                    }
                    Ok(chains)
                })
                .await?;
            Ok(AccountReply::HostChains { chains })
        }
        AccountCall::ChainGet => {
            let account = logged_in(state, login, device).await?;
            let id = account.id;
            let entries = state.db(move |db| db.chain(id)).await?;
            Ok(AccountReply::Chain { entries })
        }
        AccountCall::ChainAppend { entry } => {
            let account = logged_in(state, login, device).await?;
            let id = account.id;
            let entries = state.db(move |db| db.chain(id)).await?;
            if entry.seq != entries.len() as u64 {
                return Err(AccountError::ChainConflict.into());
            }
            if entries.is_empty() {
                // The first: this device, by itself, with 2FA on.
                if account.totp.is_none() {
                    return Err(AccountError::NeedsTotp.into());
                }
                if entry.signer != key {
                    return Err(AccountError::ChainInvalid.into());
                }
            }
            // Checked here too, so the account keeps a chain its devices
            // take (they check it all again anyway).
            let mut all = entries;
            all.push(entry.clone());
            let chain = blocking(move || Chain::verify(&all)).await;
            let chain = chain.map_err(|_| AccountError::ChainInvalid)?;
            let appended = {
                let entry = entry.clone();
                state.db(move |db| db.append(id, &entry)).await?
            };
            if !appended {
                return Err(AccountError::ChainConflict.into());
            }
            {
                let chain = chain.clone();
                state.db(move |db| db.index(id, &chain)).await?;
            }
            if let Op::AddDevice { device: added, .. } = &entry.op {
                state.joins.joined(id, added);
            }
            info!(
                "conta {id}: lista de dispositivos com {} entradas",
                chain.len
            );
            Ok(AccountReply::Ok)
        }
        AccountCall::ChainJoin {
            device: joining,
            name,
            proof,
        } => {
            let account = logged_in(state, login, device).await?;
            // Only for itself: the device logged in on this connection.
            if joining != key || name.chars().count() > hext_signal::chain::MAX_NAME {
                return Err(AccountError::ChainInvalid.into());
            }
            let join = Join {
                device: joining,
                name,
                proof,
            };
            state.joins.add(account.id, join, Instant::now());
            Ok(AccountReply::Ok)
        }
        AccountCall::ChainPending => {
            let account = logged_in(state, login, device).await?;
            let joins = state.joins.of(account.id, Instant::now());
            Ok(AccountReply::Joins { joins })
        }
        AccountCall::ChangePassword { old, new } => {
            let account = logged_in(state, login, device).await?;
            check_password(&new)?;
            *guessed = Some(limited(state, ip, &account.email)?);
            let stored = account.password.clone();
            if !blocking(move || verify_password(&old, &stored)).await {
                return Err(AccountError::WrongCredentials.into());
            }
            let hash = blocking(move || hash_password(&new)).await;
            let id = account.id;
            state
                .db(move |db| {
                    db.set_password(id, &hash)?;
                    db.close_sessions(id, Some(device))
                })
                .await?;
            info!("conta {id}: senha trocada");
            Ok(AccountReply::Ok)
        }
    }
}

/// Checks the 2FA code (or a recovery code) of a login.
async fn second_factor(
    state: &State,
    account: &Account,
    secret: &[u8],
    given: &str,
) -> Result<(), Fail> {
    let id = account.id;
    if let Some(step) = totp_check(secret, given, unix_now(), account.totp_last_step) {
        state.db(move |db| db.set_totp_step(id, step)).await?;
        return Ok(());
    }
    let code = clean_recovery(given);
    if code.len() == RECOVERY_LEN && state.db(move |db| db.use_recovery(id, &code)).await? {
        info!("conta {id}: entrou com um código de recuperação");
        return Ok(());
    }
    Err(AccountError::WrongCode.into())
}

/// The account this connection is logged in to.
/// The account this connection is logged in to, if the login still holds;
/// forgotten if not.
async fn logged_in(
    state: &State,
    login: &mut Option<i64>,
    device: DeviceId,
) -> Result<Account, Fail> {
    let id = login.ok_or(AccountError::NotLoggedIn)?;
    let found = state
        .db(move |db| {
            if db.has_session(id, device)? {
                db.account_by_id(id)
            } else {
                Ok(None)
            }
        })
        .await?;
    if found.is_none() {
        *login = None;
    }
    found.ok_or_else(|| AccountError::NotLoggedIn.into())
}

/// Refused once `ip` or `email` used up its wrong guesses; else `email`,
/// to count if this guess is wrong too.
fn limited(state: &State, ip: IpAddr, email: &str) -> Result<String, Fail> {
    let now = Instant::now();
    let email = email.to_owned();
    if state.account_guesses_by_ip.full(&ip, now)
        || state.account_guesses_by_email.full(&email, now)
    {
        Err(AccountError::TooManyAttempts.into())
    } else {
        Ok(email)
    }
}

/// For calls that send an e-mail: within the limits, and counted.
fn sends(state: &State, ip: IpAddr, email: &str) -> Result<(), Fail> {
    let email = limited(state, ip, email)?;
    count(state, ip, &email);
    Ok(())
}

fn count(state: &State, ip: IpAddr, email: &str) {
    let now = Instant::now();
    state.account_guesses_by_ip.record(&ip, now);
    state
        .account_guesses_by_email
        .record(&email.to_owned(), now);
}

/// Runs heavy work (Argon2) off the async threads.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(work)
        .await
        .expect("blocking task")
}

impl State {
    /// Sends an e-mail in the background: the answer takes as long whether
    /// or not one goes.
    fn mail(&self, to: &str, subject: &str, body: &str) {
        let mailer: Arc<dyn Mailer> = self.mailer.clone();
        let (to, subject, body) = (to.to_owned(), subject.to_owned(), body.to_owned());
        tokio::task::spawn_blocking(move || mailer.send(&to, &subject, &body));
    }
}

/// An e-mail address as kept: trimmed and in lower case; `None` if it is
/// not one we take.
fn clean_email(given: &str) -> Option<String> {
    let email = given.trim().to_lowercase();
    if email.len() > MAX_EMAIL || !email.is_ascii() {
        return None;
    }
    let (local, domain) = email.split_once('@')?;
    let fine = |c: char| c.is_ascii_graphic() && !"@<>()[]\\,;:\"".contains(c);
    let ok = !local.is_empty()
        && local.len() <= 64
        && local.chars().all(fine)
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains("..")
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    ok.then_some(email)
}

fn check_password(password: &str) -> Result<(), Fail> {
    let length = password.chars().count();
    if (MIN_PASSWORD..=MAX_PASSWORD).contains(&length) {
        Ok(())
    } else {
        Err(AccountError::WeakPassword.into())
    }
}

/// A code or token as given, without spaces; too long is wrong.
fn clean_code(given: &str) -> Result<String, Fail> {
    let code: String = given.chars().filter(|c| !c.is_whitespace()).collect();
    if code.is_empty() || code.len() > MAX_CODE {
        return Err(AccountError::WrongCode.into());
    }
    Ok(code)
}

fn hash_password(password: &str) -> String {
    let salt = SaltString::encode_b64(&random::<16>()).expect("salt");
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2")
        .to_string()
}

fn verify_password(password: &str, stored: &str) -> bool {
    PasswordHash::new(stored).is_ok_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

/// A hash no password matches, checked when the account does not exist.
fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| hash_password(&hex::encode(random::<32>())))
}

fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .expect("system random numbers");
    bytes
}

fn sha256(data: &[u8]) -> Vec<u8> {
    ring::digest::digest(&ring::digest::SHA256, data)
        .as_ref()
        .to_vec()
}

/// A login's token: 32 random bytes, in hex.
fn token() -> String {
    hex::encode(random::<32>())
}

/// A code to e-mail: 6 digits, uniform.
fn email_code() -> String {
    let limit = u32::MAX - u32::MAX % 1_000_000;
    loop {
        let n = u32::from_le_bytes(random::<4>());
        if n < limit {
            return format!("{:06}", n % 1_000_000);
        }
    }
}

/// Letters and digits that do not look alike (no 0/O, 1/I/L).
const RECOVERY_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const RECOVERY_LEN: usize = 10;

/// A recovery code, as shown: `XXXXX-XXXXX`.
fn recovery_code() -> String {
    let mut code = String::with_capacity(RECOVERY_LEN + 1);
    let limit = 256 - 256 % RECOVERY_ALPHABET.len();
    while code.len() < RECOVERY_LEN + 1 {
        let [b] = random::<1>();
        if usize::from(b) >= limit {
            continue;
        }
        if code.len() == RECOVERY_LEN / 2 {
            code.push('-');
        }
        code.push(RECOVERY_ALPHABET[usize::from(b) % RECOVERY_ALPHABET.len()] as char);
    }
    code
}

/// A recovery code as kept: upper case, without the dash or spaces.
fn clean_recovery(given: &str) -> String {
    given
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// RFC 4648 base 32, without padding (what authenticator apps take).
fn base32(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let (mut buffer, mut bits) = (0u32, 0);
    for &byte in data {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// The reverse of [`base32`]; `None` if it is not that.
fn unbase32(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut buffer, mut bits) = (0u32, 0);
    for c in text.chars().filter(|c| *c != '=') {
        let value = match c.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u32 - 'A' as u32,
            c @ '2'..='7' => c as u32 - '2' as u32 + 26,
            _ => return None,
        };
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

/// For the authenticator link: anything but letters, digits and `.-_` as
/// `%XX`.
fn percent(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The TOTP code (RFC 6238: HMAC-SHA1, 6 digits) of `step`.
fn totp(secret: &[u8], step: u64) -> u32 {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let mac = ring::hmac::sign(&key, &step.to_be_bytes());
    let mac = mac.as_ref();
    let offset = usize::from(mac[mac.len() - 1] & 0x0f);
    let value = u32::from_be_bytes([
        mac[offset],
        mac[offset + 1],
        mac[offset + 2],
        mac[offset + 3],
    ]) & 0x7fff_ffff;
    value % 1_000_000
}

/// The step `code` belongs to, if it is right for the time `now` (one step
/// of drift either way) and newer than `last_step`: a code is good once.
fn totp_check(secret: &[u8], code: &str, now: u64, last_step: u64) -> Option<u64> {
    let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let given: u32 = code.parse().ok()?;
    let current = now / TOTP_STEP;
    [current.saturating_sub(1), current, current + 1]
        .into_iter()
        .filter(|&step| step > last_step)
        .find(|&step| totp(secret, step) == given)
}

/// The code an authenticator app shows for `secret` (base 32) at `unix_time`:
/// for tests, the app's own and the server's.
pub fn totp_code(secret: &str, unix_time: u64) -> Option<String> {
    let secret = unbase32(secret)?;
    Some(format!("{:06}", totp(&secret, unix_time / TOTP_STEP)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e_mail_addresses_are_cleaned_or_refused() {
        assert_eq!(
            clean_email("  Alex@Example.COM ").as_deref(),
            Some("alex@example.com")
        );
        for bad in [
            "",
            "alex",
            "@example.com",
            "alex@",
            "alex@example",
            "a@b@c.com",
            "alex@.com",
            "alex@example.com.",
            "alex@exa..mple.com",
            "al ex@example.com",
            "álex@example.com",
            "alex@exam_ple.com",
        ] {
            assert_eq!(clean_email(bad), None, "{bad}");
        }
        assert_eq!(
            clean_email(&format!("{}@example.com", "a".repeat(65))),
            None
        );
    }

    #[test]
    fn passwords_have_a_size() {
        assert!(check_password("curta").is_err());
        assert!(check_password("cavalo correto").is_ok());
        assert!(check_password(&"x".repeat(MAX_PASSWORD + 1)).is_err());
    }

    #[test]
    fn a_password_matches_its_hash_only() {
        let hash = hash_password("cavalo correto");
        assert!(hash.starts_with("$argon2id$"), "{hash}");
        assert!(verify_password("cavalo correto", &hash));
        assert!(!verify_password("cavalo errado", &hash));
        assert!(!verify_password("cavalo correto", "not a hash"));
        assert!(!verify_password("", dummy_hash()));
        assert_ne!(hash, hash_password("cavalo correto"), "salted");
    }

    #[test]
    fn totp_matches_the_rfc_vectors() {
        // RFC 6238, appendix B (SHA-1), last 6 digits.
        let secret = b"12345678901234567890";
        assert_eq!(totp(secret, 59 / 30), 287_082);
        assert_eq!(totp(secret, 1_111_111_109 / 30), 81_804);
        assert_eq!(totp(secret, 1_234_567_890 / 30), 5_924);
        assert_eq!(totp(secret, 2_000_000_000 / 30), 279_037);
        assert_eq!(
            totp_code(&base32(secret), 1_234_567_890).as_deref(),
            Some("005924")
        );
    }

    #[test]
    fn a_totp_code_is_good_once_and_for_a_short_while() {
        let secret = b"12345678901234567890";
        let now = 1_234_567_890;
        let code = format!("{:06}", totp(secret, now / 30));
        let step = totp_check(secret, &code, now, 0).unwrap();
        assert_eq!(totp_check(secret, &code, now, step), None, "used");
        assert!(totp_check(secret, &code, now + 30, 0).is_some(), "drift");
        assert_eq!(totp_check(secret, &code, now + 90, 0), None, "too late");
        assert_eq!(totp_check(secret, "12345", now, 0), None);
        assert_eq!(totp_check(secret, "abcdef", now, 0), None);
    }

    #[test]
    fn base32_goes_both_ways() {
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
        assert_eq!(unbase32("MZXW6YTBOI").unwrap(), b"foobar");
        assert_eq!(unbase32("mzxw6ytboi======").unwrap(), b"foobar");
        assert_eq!(unbase32("not base32!"), None);
    }

    #[test]
    fn codes_look_right() {
        let code = email_code();
        assert!(code.len() == 6 && code.chars().all(|c| c.is_ascii_digit()));
        let recovery = recovery_code();
        assert_eq!(recovery.len(), RECOVERY_LEN + 1);
        assert_eq!(recovery.as_bytes()[5], b'-');
        assert_eq!(clean_recovery(&recovery.to_lowercase()).len(), RECOVERY_LEN);
        assert_eq!(token().len(), 64);
        assert_eq!(percent("a b@c.com"), "a%20b%40c.com");
    }

    #[test]
    fn a_code_is_used_up_and_wears_out() {
        let db = Db::in_memory().unwrap();
        let id = db.create_account("a@b.com", "x").unwrap().unwrap();
        assert_eq!(db.create_account("a@b.com", "y").unwrap(), None);
        db.set_code(id, "verify", "123456").unwrap();
        assert_eq!(db.check_code(id, "verify", "123456").unwrap(), Code::Right);
        assert_eq!(
            db.check_code(id, "verify", "123456").unwrap(),
            Code::Wrong,
            "used"
        );
        db.set_code(id, "verify", "123456").unwrap();
        for _ in 0..CODE_TRIES {
            assert_eq!(db.check_code(id, "verify", "000000").unwrap(), Code::Wrong);
        }
        assert_eq!(
            db.check_code(id, "verify", "123456").unwrap(),
            Code::Wrong,
            "worn out"
        );
        db.set_code(id, "reset", "654321").unwrap();
        assert_eq!(
            db.check_code(id, "verify", "654321").unwrap(),
            Code::Wrong,
            "another purpose"
        );
    }

    #[test]
    fn a_login_holds_for_its_device_only() {
        let db = Db::in_memory().unwrap();
        let id = db.create_account("a@b.com", "x").unwrap().unwrap();
        let (mine, other) = (
            DeviceId::new(482_913_557).unwrap(),
            DeviceId::new(111_222_333).unwrap(),
        );
        db.open_session("token", id, mine).unwrap();
        assert_eq!(db.session("token", mine).unwrap(), Some(id));
        assert_eq!(db.session("token", other).unwrap(), None);
        db.open_session("token2", id, other).unwrap();
        db.close_sessions(id, Some(mine)).unwrap();
        assert_eq!(db.session("token", mine).unwrap(), Some(id));
        assert_eq!(db.session("token2", other).unwrap(), None);
        db.close_session(id, mine).unwrap();
        assert_eq!(db.session("token", mine).unwrap(), None);
    }
}
