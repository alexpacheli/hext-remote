//! The account's chain (F5, marco 3): who is in the account, as a list of
//! entries each signed by a device already in it. The server keeps and hands
//! out the list, but cannot add an entry: it has none of the keys. So a
//! computer that trusts an account trusts the list, after checking every
//! signature ([`Chain::verify`]), not the server.
//!
//! - The first entry ([`Op::Create`]) is signed by the account's first
//!   device, by itself, and names the **recovery key**: its private half
//!   comes from a code shown to the user once ([`recovery_key`]), for adding
//!   a device when no other is at hand.
//! - Every other entry names the hash of the one before it ([`Entry::prev`]),
//!   so the server cannot reorder, drop from the middle or fork the list
//!   unseen. It can hold back the latest entries: whoever keeps a chain asks
//!   that the next one it is handed extends it ([`Chain::extends`]).
//! - Devices (the technician's key of each computer) sign. Computers (their
//!   host key and ID) are what the account's devices may enter (marco 4),
//!   and shares give another account's devices access to one (marco 6).

use std::collections::HashMap;

use hext_crypto::{DeviceKey, PublicKey, Signature};
use ring::hmac;
use serde::{Deserialize, Serialize};

use crate::DeviceId;

/// Longest chain taken: it is checked whole each time.
pub const MAX_ENTRIES: usize = 2000;
/// Devices, computers and shares in one account, at most.
pub const MAX_MEMBERS: usize = 200;
/// Longest name (of a device, a computer, an account in a share).
pub const MAX_NAME: usize = 64;

const SIGNED: &[u8] = b"hext-chain-v1\0";
const HASHED: &[u8] = b"hext-chain-entry-v1\0";
const RECOVERY: &[u8] = b"hext-chain-recovery-v1\0";
const JOIN: &[u8] = b"hext-chain-join-v1\0";
const BIND: &[u8] = b"hext-chain-bind-v1\0";

/// A chain's ID: the hash of its first entry.
pub type ChainId = [u8; 32];

/// What a share allows: as the request window's boxes (viewing always).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Allowed {
    pub control: bool,
    pub clipboard: bool,
    pub files: bool,
    pub restart: bool,
}

/// What an entry does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// The first entry: the first device, signing by itself, and the public
    /// half of the recovery key.
    Create {
        #[serde(with = "crate::hex_key")]
        device: PublicKey,
        name: String,
        #[serde(with = "crate::hex_key")]
        recovery: PublicKey,
    },
    /// Another device (its technician's key). Signed by a device in the
    /// account or by the recovery key.
    AddDevice {
        #[serde(with = "crate::hex_key")]
        device: PublicKey,
        name: String,
    },
    /// A device out (lost, sold). Signed by a device or the recovery key;
    /// the last one cannot go.
    RemoveDevice {
        #[serde(with = "crate::hex_key")]
        device: PublicKey,
    },
    /// A computer the account's devices may enter (its host key and ID).
    AddComputer {
        #[serde(with = "crate::hex_key")]
        host: PublicKey,
        id: DeviceId,
        name: String,
    },
    RemoveComputer {
        #[serde(with = "crate::hex_key")]
        host: PublicKey,
    },
    /// Computer `host` opens to the devices of account `with` (another
    /// chain), allowing `allowed`, until `until` (Unix time; `None`: until
    /// taken back). `name`: how that account is shown (its e-mail).
    Share {
        #[serde(with = "crate::hex_key")]
        host: PublicKey,
        #[serde(with = "crate::hex_array")]
        with: ChainId,
        name: String,
        allowed: Allowed,
        until: Option<u64>,
    },
    Unshare {
        #[serde(with = "crate::hex_key")]
        host: PublicKey,
        #[serde(with = "crate::hex_array")]
        with: ChainId,
    },
}

/// A technician asks a computer, with its unattended password, to bind
/// itself to the technician's account (the chain `chain`): from then on the
/// account's devices come in without the password, once the computer is on
/// the account's list. Signed by the technician's key for that computer's
/// ID, so a server passing the request on cannot point it at a chain of its
/// own (it has no such signature) nor at another computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindAsk {
    #[serde(with = "crate::hex_array")]
    pub chain: ChainId,
    #[serde(with = "crate::hex_sig")]
    pub signature: Signature,
}

impl BindAsk {
    /// `key` (the technician's) asks computer `host` to bind to `chain`.
    pub fn new(key: &DeviceKey, chain: ChainId, host: DeviceId) -> Self {
        Self {
            chain,
            signature: key.sign(&bind_bytes(&chain, host)),
        }
    }

    /// Whether `viewer` (the key the connection proves) asked it, of
    /// computer `host`.
    pub fn holds(&self, viewer: &PublicKey, host: DeviceId) -> bool {
        viewer.verify(&bind_bytes(&self.chain, host), &self.signature)
    }
}

fn bind_bytes(chain: &ChainId, host: DeviceId) -> Vec<u8> {
    let mut bytes = BIND.to_vec();
    bytes.extend(chain);
    bytes.extend(host.get().to_be_bytes());
    bytes
}

/// One signed entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Its place: 0 for the first.
    pub seq: u64,
    /// The hash of the entry before (zeros for the first).
    #[serde(with = "crate::hex_array")]
    pub prev: [u8; 32],
    #[serde(flatten)]
    pub op: Op,
    #[serde(with = "crate::hex_key")]
    pub signer: PublicKey,
    #[serde(with = "crate::hex_sig")]
    pub signature: Signature,
}

/// A computer in the account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Computer {
    #[serde(with = "crate::hex_key")]
    pub host: PublicKey,
    pub id: DeviceId,
    pub name: String,
}

/// A device in the account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Device {
    #[serde(with = "crate::hex_key")]
    pub key: PublicKey,
    pub name: String,
}

/// A computer opened to another account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Share {
    #[serde(with = "crate::hex_key")]
    pub host: PublicKey,
    #[serde(with = "crate::hex_array")]
    pub with: ChainId,
    pub name: String,
    pub allowed: Allowed,
    pub until: Option<u64>,
}

/// Why a chain was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChainError {
    #[error("lista de dispositivos vazia")]
    Empty,
    #[error("lista de dispositivos longa demais")]
    TooLong,
    #[error("entrada {0}: fora de ordem")]
    OutOfOrder(u64),
    #[error("entrada {0}: assinatura inválida")]
    BadSignature(u64),
    #[error("entrada {0}: assinada por quem não está na conta")]
    NotAMember(u64),
    #[error("entrada {0}: não vale aqui")]
    Invalid(u64),
    #[error("a lista não continua a que já se conhecia")]
    Rolledback,
}

/// A chain whose every entry was checked: who is in the account now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    pub id: ChainId,
    /// The hash of the last entry.
    pub head: [u8; 32],
    /// How many entries.
    pub len: u64,
    pub recovery: PublicKey,
    devices: Vec<Device>,
    computers: Vec<Computer>,
    shares: Vec<Share>,
}

impl Chain {
    /// Checks `entries`, from the first: each in its place, after the one
    /// before, signed by someone allowed to.
    pub fn verify(entries: &[Entry]) -> Result<Self, ChainError> {
        if entries.len() > MAX_ENTRIES {
            return Err(ChainError::TooLong);
        }
        let (first, rest) = entries.split_first().ok_or(ChainError::Empty)?;
        let mut chain = Self::create(first)?;
        for entry in rest {
            chain.apply(entry)?;
        }
        Ok(chain)
    }

    fn create(entry: &Entry) -> Result<Self, ChainError> {
        let Op::Create {
            device,
            name,
            recovery,
        } = &entry.op
        else {
            return Err(ChainError::Invalid(entry.seq));
        };
        if entry.seq != 0 || entry.prev != [0; 32] {
            return Err(ChainError::OutOfOrder(entry.seq));
        }
        if entry.signer != *device || *recovery == *device || !good_name(name) {
            return Err(ChainError::Invalid(0));
        }
        if !entry
            .signer
            .verify(&signed_bytes(&[0; 32], entry), &entry.signature)
        {
            return Err(ChainError::BadSignature(0));
        }
        let id = entry.hash();
        Ok(Self {
            id,
            head: id,
            len: 1,
            recovery: *recovery,
            devices: vec![Device {
                key: *device,
                name: name.clone(),
            }],
            computers: Vec::new(),
            shares: Vec::new(),
        })
    }

    /// Adds `entry` at the end, if it belongs there.
    pub fn apply(&mut self, entry: &Entry) -> Result<(), ChainError> {
        let seq = entry.seq;
        if seq != self.len || entry.prev != self.head {
            return Err(ChainError::OutOfOrder(seq));
        }
        if self.len as usize >= MAX_ENTRIES {
            return Err(ChainError::TooLong);
        }
        let by_device = self.is_device(&entry.signer);
        let by_recovery = entry.signer == self.recovery;
        let allowed = match entry.op {
            Op::AddDevice { .. } | Op::RemoveDevice { .. } => by_device || by_recovery,
            Op::Create { .. } => false,
            _ => by_device,
        };
        if !allowed {
            return Err(ChainError::NotAMember(seq));
        }
        if !entry
            .signer
            .verify(&signed_bytes(&self.id, entry), &entry.signature)
        {
            return Err(ChainError::BadSignature(seq));
        }
        let invalid = Err(ChainError::Invalid(seq));
        let members = self.devices.len() + self.computers.len() + self.shares.len();
        match &entry.op {
            Op::Create { .. } => return invalid,
            Op::AddDevice { device, name } => {
                if self.is_device(device)
                    || *device == self.recovery
                    || !good_name(name)
                    || members >= MAX_MEMBERS
                {
                    return invalid;
                }
                self.devices.push(Device {
                    key: *device,
                    name: name.clone(),
                });
            }
            Op::RemoveDevice { device } => {
                if !self.is_device(device) || self.devices.len() == 1 {
                    return invalid;
                }
                self.devices.retain(|d| d.key != *device);
            }
            Op::AddComputer { host, id, name } => {
                if self.computer(host).is_some()
                    || self.computers.iter().any(|c| c.id == *id)
                    || !good_name(name)
                    || members >= MAX_MEMBERS
                {
                    return invalid;
                }
                self.computers.push(Computer {
                    host: *host,
                    id: *id,
                    name: name.clone(),
                });
            }
            Op::RemoveComputer { host } => {
                if self.computer(host).is_none() {
                    return invalid;
                }
                self.computers.retain(|c| c.host != *host);
                self.shares.retain(|s| s.host != *host);
            }
            Op::Share {
                host,
                with,
                name,
                allowed,
                until,
            } => {
                if self.computer(host).is_none()
                    || *with == self.id
                    || !good_name(name)
                    || members >= MAX_MEMBERS
                {
                    return invalid;
                }
                // Sharing again changes the terms.
                self.shares
                    .retain(|s| !(s.host == *host && s.with == *with));
                self.shares.push(Share {
                    host: *host,
                    with: *with,
                    name: name.clone(),
                    allowed: *allowed,
                    until: *until,
                });
            }
            Op::Unshare { host, with } => {
                let before = self.shares.len();
                self.shares
                    .retain(|s| !(s.host == *host && s.with == *with));
                if self.shares.len() == before {
                    return invalid;
                }
            }
        }
        self.head = entry.hash();
        self.len += 1;
        Ok(())
    }

    /// Whether this chain is `known` or comes after it (the same first
    /// entries): `entries` are this chain's.
    pub fn extends(&self, entries: &[Entry], known_len: u64, known_head: &[u8; 32]) -> bool {
        known_len <= self.len
            && known_len > 0
            && entries
                .get(known_len as usize - 1)
                .is_some_and(|last| last.hash() == *known_head)
    }

    pub fn devices(&self) -> &[Device] {
        &self.devices
    }

    pub fn computers(&self) -> &[Computer] {
        &self.computers
    }

    pub fn shares(&self) -> &[Share] {
        &self.shares
    }

    pub fn is_device(&self, key: &PublicKey) -> bool {
        self.devices.iter().any(|d| d.key == *key)
    }

    pub fn computer(&self, host: &PublicKey) -> Option<&Computer> {
        self.computers.iter().find(|c| c.host == *host)
    }

    /// The shares of computer `host` that hold at `now` (Unix time), by the
    /// account they are with.
    pub fn shares_of(&self, host: &PublicKey, now: u64) -> HashMap<ChainId, &Share> {
        self.shares
            .iter()
            .filter(|s| s.host == *host && s.until.is_none_or(|until| until > now))
            .map(|s| (s.with, s))
            .collect()
    }

    /// The next entry, doing `op`, signed by `key`.
    pub fn sign(&self, op: Op, key: &DeviceKey) -> Entry {
        Entry::signed(&self.id, self.len, self.head, op, key)
    }
}

impl Entry {
    /// The first entry of a new chain: device `key`, called `name`, with the
    /// recovery key `recovery`.
    pub fn create(key: &DeviceKey, name: &str, recovery: PublicKey) -> Self {
        let op = Op::Create {
            device: key.public_key(),
            name: name.to_owned(),
            recovery,
        };
        Self::signed(&[0; 32], 0, [0; 32], op, key)
    }

    fn signed(chain: &ChainId, seq: u64, prev: [u8; 32], op: Op, key: &DeviceKey) -> Self {
        let mut entry = Self {
            seq,
            prev,
            op,
            signer: key.public_key(),
            signature: Signature::from_bytes([0; 64]),
        };
        entry.signature = key.sign(&signed_bytes(chain, &entry));
        entry
    }

    /// What the next entry names as [`Entry::prev`] (and, for the first, the
    /// chain's ID).
    pub fn hash(&self) -> [u8; 32] {
        let mut bytes = HASHED.to_vec();
        bytes.extend(self.seq.to_be_bytes());
        bytes.extend(self.prev);
        bytes.extend(self.signer.as_bytes());
        op_bytes(&self.op, &mut bytes);
        bytes.extend(self.signature.to_bytes());
        sha256(&bytes)
    }
}

fn good_name(name: &str) -> bool {
    let length = name.chars().count();
    (1..=MAX_NAME).contains(&length) && !name.chars().any(char::is_control)
}

/// What an entry's signature covers: the chain (zeros for the first entry,
/// whose hash is the chain's ID), its place, the entry before it, the signer
/// and the operation.
fn signed_bytes(chain: &ChainId, entry: &Entry) -> Vec<u8> {
    let mut bytes = SIGNED.to_vec();
    bytes.extend(chain);
    bytes.extend(entry.seq.to_be_bytes());
    bytes.extend(entry.prev);
    bytes.extend(entry.signer.as_bytes());
    op_bytes(&entry.op, &mut bytes);
    bytes
}

/// The operation as bytes: a tag, then each field (text with its length).
fn op_bytes(op: &Op, out: &mut Vec<u8>) {
    let text = |out: &mut Vec<u8>, s: &str| {
        out.extend((s.len() as u32).to_be_bytes());
        out.extend(s.as_bytes());
    };
    match op {
        Op::Create {
            device,
            name,
            recovery,
        } => {
            out.push(1);
            out.extend(device.as_bytes());
            text(out, name);
            out.extend(recovery.as_bytes());
        }
        Op::AddDevice { device, name } => {
            out.push(2);
            out.extend(device.as_bytes());
            text(out, name);
        }
        Op::RemoveDevice { device } => {
            out.push(3);
            out.extend(device.as_bytes());
        }
        Op::AddComputer { host, id, name } => {
            out.push(4);
            out.extend(host.as_bytes());
            out.extend(id.get().to_be_bytes());
            text(out, name);
        }
        Op::RemoveComputer { host } => {
            out.push(5);
            out.extend(host.as_bytes());
        }
        Op::Share {
            host,
            with,
            name,
            allowed,
            until,
        } => {
            out.push(6);
            out.extend(host.as_bytes());
            out.extend(with);
            text(out, name);
            out.extend([
                u8::from(allowed.control),
                u8::from(allowed.clipboard),
                u8::from(allowed.files),
                u8::from(allowed.restart),
            ]);
            match until {
                Some(until) => {
                    out.push(1);
                    out.extend(until.to_be_bytes());
                }
                None => out.push(0),
            }
        }
        Op::Unshare { host, with } => {
            out.push(7);
            out.extend(host.as_bytes());
            out.extend(with);
        }
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut out = [0; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// Letters and digits that do not look alike.
const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

/// A random code of `len` characters, in groups of 4 (`ABCD-EFGH-…`).
pub fn new_code(len: usize) -> String {
    use ring::rand::{SecureRandom, SystemRandom};
    let rng = SystemRandom::new();
    let limit = 256 - 256 % ALPHABET.len();
    let mut chars = Vec::with_capacity(len);
    while chars.len() < len {
        let mut byte = [0];
        rng.fill(&mut byte).expect("system random numbers");
        if usize::from(byte[0]) < limit {
            chars.push(ALPHABET[usize::from(byte[0]) % ALPHABET.len()] as char);
        }
    }
    chars
        .chunks(4)
        .map(|group| group.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
}

/// A code as typed: upper case, letters and digits only.
pub fn clean_code(code: &str) -> String {
    code.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Characters of a recovery code (about 100 bits).
pub const RECOVERY_CODE_LEN: usize = 20;
/// Characters of a code for adding a device (about 60 bits: it lives 10
/// minutes, and the server would have to guess it in that time).
pub const JOIN_CODE_LEN: usize = 12;

/// The recovery key of `code` (as shown, or as typed).
pub fn recovery_key(code: &str) -> DeviceKey {
    let mut bytes = RECOVERY.to_vec();
    bytes.extend(clean_code(code).as_bytes());
    DeviceKey::from_seed(&sha256(&bytes)).expect("any 32 bytes are a seed")
}

/// What a device asking to join chain `chain` sends with its key, made with
/// the code shown on the device already in it: proof, to that device, that
/// the key is the one at the other computer, not one the server put there.
pub fn join_proof(code: &str, chain: &ChainId, device: &PublicKey, name: &str) -> [u8; 32] {
    let key = hmac::Key::new(hmac::HMAC_SHA256, clean_code(code).as_bytes());
    let mut message = JOIN.to_vec();
    message.extend(chain);
    message.extend(device.as_bytes());
    message.extend(name.as_bytes());
    let mut out = [0; 32];
    out.copy_from_slice(hmac::sign(&key, &message).as_ref());
    out
}

/// Whether `proof` came from someone with `code`.
pub fn join_proof_ok(
    code: &str,
    chain: &ChainId,
    device: &PublicKey,
    name: &str,
    proof: &[u8; 32],
) -> bool {
    hext_crypto::constant_time_eq(&join_proof(code, chain, device, name), proof)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> DeviceKey {
        DeviceKey::generate().unwrap()
    }

    fn start() -> (Vec<Entry>, DeviceKey, DeviceKey) {
        let first = key();
        let recovery = recovery_key(&new_code(RECOVERY_CODE_LEN));
        let entries = vec![Entry::create(&first, "PC", recovery.public_key())];
        (entries, first, recovery)
    }

    fn push(entries: &mut Vec<Entry>, op: Op, by: &DeviceKey) -> Result<Chain, ChainError> {
        let chain = Chain::verify(entries).unwrap();
        entries.push(chain.sign(op, by));
        Chain::verify(entries)
    }

    #[test]
    fn devices_join_and_leave() {
        let (mut entries, first, recovery) = start();
        let notebook = key();
        let add = Op::AddDevice {
            device: notebook.public_key(),
            name: "Notebook".into(),
        };
        let chain = push(&mut entries, add, &first).unwrap();
        assert!(chain.is_device(&notebook.public_key()));
        assert_eq!(chain.devices().len(), 2);
        // The notebook signs now too.
        let host = key().public_key();
        let id = DeviceId::new(482_913_557).unwrap();
        let computer = Op::AddComputer {
            host,
            id,
            name: "Escritório".into(),
        };
        let chain = push(&mut entries, computer, &notebook).unwrap();
        assert_eq!(chain.computer(&host).unwrap().id, id);
        // Lost: the recovery key takes the first device out.
        let remove = Op::RemoveDevice {
            device: first.public_key(),
        };
        let chain = push(&mut entries, remove, &recovery).unwrap();
        assert!(!chain.is_device(&first.public_key()));
        assert_eq!(chain.len, 4);
        // The last device stays.
        let last = Op::RemoveDevice {
            device: notebook.public_key(),
        };
        assert_eq!(
            push(&mut entries, last, &notebook),
            Err(ChainError::Invalid(4))
        );
    }

    #[test]
    fn entries_from_outside_are_refused() {
        let (mut entries, first, recovery) = start();
        let stranger = key();
        // A key not in the account (the server's, say) cannot add itself.
        let add = Op::AddDevice {
            device: stranger.public_key(),
            name: "Intruso".into(),
        };
        assert_eq!(
            push(&mut entries, add.clone(), &stranger),
            Err(ChainError::NotAMember(1))
        );
        entries.pop();
        // The recovery key adds devices, but not computers.
        let computer = Op::AddComputer {
            host: key().public_key(),
            id: DeviceId::new(111_222_333).unwrap(),
            name: "PC".into(),
        };
        assert_eq!(
            push(&mut entries, computer, &recovery),
            Err(ChainError::NotAMember(1))
        );
        entries.pop();
        // A signature over something else does not hold.
        let chain = Chain::verify(&entries).unwrap();
        let mut forged = chain.sign(add, &first);
        forged.op = Op::AddDevice {
            device: stranger.public_key(),
            name: "Outro nome".into(),
        };
        entries.push(forged);
        assert_eq!(Chain::verify(&entries), Err(ChainError::BadSignature(1)));
    }

    #[test]
    fn order_is_kept() {
        let (mut entries, first, _) = start();
        for name in ["A", "B"] {
            let op = Op::AddDevice {
                device: key().public_key(),
                name: name.into(),
            };
            push(&mut entries, op, &first).unwrap();
        }
        let chain = Chain::verify(&entries).unwrap();
        // Dropping one from the middle, or swapping two, breaks it.
        let mut dropped = entries.clone();
        dropped.remove(1);
        assert_eq!(Chain::verify(&dropped), Err(ChainError::OutOfOrder(2)));
        let mut swapped = entries.clone();
        swapped.swap(1, 2);
        assert!(Chain::verify(&swapped).is_err());
        // Repeated.
        let mut repeated = entries.clone();
        repeated.push(entries[2].clone());
        assert_eq!(Chain::verify(&repeated), Err(ChainError::OutOfOrder(2)));
        // Held back: an older one does not extend what was seen.
        let older = Chain::verify(&entries[..2]).unwrap();
        assert!(chain.extends(&entries, older.len, &older.head));
        assert!(!older.extends(&entries[..2], chain.len, &chain.head));
        // A fork of the same first entry does not extend it either.
        let mut fork = entries[..2].to_vec();
        let other = Op::AddDevice {
            device: key().public_key(),
            name: "C".into(),
        };
        let forked = push(&mut fork, other, &first).unwrap();
        assert!(!forked.extends(&fork, chain.len, &chain.head));
        // Another account's first entry is another chain.
        let (others, _, _) = start();
        assert_ne!(Chain::verify(&others).unwrap().id, chain.id);
    }

    #[test]
    fn shares_hold_until_they_end() {
        let (mut entries, first, _) = start();
        let host = key().public_key();
        let computer = Op::AddComputer {
            host,
            id: DeviceId::new(482_913_557).unwrap(),
            name: "PC".into(),
        };
        push(&mut entries, computer, &first).unwrap();
        let with = [7; 32];
        let share = Op::Share {
            host,
            with,
            name: "tecnico@example.com".into(),
            allowed: Allowed {
                control: true,
                ..Allowed::default()
            },
            until: Some(1_000),
        };
        let chain = push(&mut entries, share, &first).unwrap();
        assert!(chain.shares_of(&host, 999).contains_key(&with));
        assert!(chain.shares_of(&host, 1_000).is_empty());
        let chain = push(&mut entries, Op::Unshare { host, with }, &first).unwrap();
        assert!(chain.shares().is_empty());
        // Nothing to take back.
        assert!(push(&mut entries, Op::Unshare { host, with }, &first).is_err());
    }

    #[test]
    fn entries_go_as_json_and_back() {
        let (mut entries, first, _) = start();
        let op = Op::AddDevice {
            device: key().public_key(),
            name: "Notebook".into(),
        };
        push(&mut entries, op, &first).unwrap();
        let json = serde_json::to_string(&entries).unwrap();
        let back: Vec<Entry> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, entries);
        assert!(Chain::verify(&back).is_ok());
    }

    #[test]
    fn codes() {
        let code = new_code(JOIN_CODE_LEN);
        assert_eq!(code.len(), JOIN_CODE_LEN + 2);
        assert_eq!(clean_code(&code.to_lowercase()).len(), JOIN_CODE_LEN);
        // The same code, typed any way, gives the same recovery key.
        let recovery = new_code(RECOVERY_CODE_LEN);
        assert_eq!(
            recovery_key(&recovery).public_key(),
            recovery_key(&recovery.to_lowercase().replace('-', " ")).public_key()
        );
        let device = key().public_key();
        let proof = join_proof(&code, &[1; 32], &device, "PC");
        assert!(join_proof_ok(&code, &[1; 32], &device, "PC", &proof));
        assert!(!join_proof_ok(
            &code,
            &[1; 32],
            &key().public_key(),
            "PC",
            &proof
        ));
        assert!(!join_proof_ok(
            &new_code(12),
            &[1; 32],
            &device,
            "PC",
            &proof
        ));
    }

    #[test]
    fn an_ask_to_bind_holds_only_as_signed() {
        let technician = key();
        let host = DeviceId::new(482_913_557).unwrap();
        let ask = BindAsk::new(&technician, [5; 32], host);
        assert!(ask.holds(&technician.public_key(), host));
        // Not from another key, for another computer, nor to another chain.
        assert!(!ask.holds(&key().public_key(), host));
        assert!(!ask.holds(
            &technician.public_key(),
            DeviceId::new(111_222_333).unwrap()
        ));
        let moved = BindAsk {
            chain: [6; 32],
            ..ask.clone()
        };
        assert!(!moved.holds(&technician.public_key(), host));
        let json = serde_json::to_string(&ask).unwrap();
        assert_eq!(serde_json::from_str::<BindAsk>(&json).unwrap(), ask);
    }
}
