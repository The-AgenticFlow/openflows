//! Operator approval seals for LORE's post-merge documentation gate.
//!
//! Approvals carry a **seal**: an HMAC over the ticket, revision and head,
//! keyed with a key derived from the controller's secret ([`DocsGateKeys`]),
//! which workspaces never receive. The reducer only checks that a seal is
//! present; NEXUS (before opening the docs PR) and VESSEL (before merging it)
//! verify it, so a workspace that can write the store still cannot forge an
//! approval.
use anyhow::{bail, ensure, Result};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::fmt;

use super::DocsRecord;

/// Minimum length of a docs gate secret: the floor the hook server enforces
/// for HS256 secrets.
pub const MIN_GATE_SECRET_LEN: usize = 32;
/// Label for deriving the seal key from the configured secret, so the bytes
/// that verify Coder hook JWTs are never used directly as a second key.
const KEY_LABEL: &[u8] = b"openflows-docs-gate-key-v1";
/// Seal format version: `v2:<key id>:<hex HMAC>`.
const SEAL_VERSION: &str = "v2";
/// Domain separator for approval seals.
pub(super) const SEAL_DOMAIN: &[u8] = b"openflows-docs-approval-v1\0";

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One seal key, derived from a configured secret, with a short public id.
#[derive(Clone)]
struct GateKey {
    id: String,
    key: [u8; 32],
}

impl GateKey {
    fn derive(secret: &str) -> Self {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
        mac.update(KEY_LABEL);
        let key: [u8; 32] = mac.finalize().into_bytes().into();
        let id = hex(&Sha256::digest(key)[..4]);
        Self { id, key }
    }

    fn mac(&self, ticket: &str, revision: u64, head: &str) -> Hmac<Sha256> {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC accepts any key length");
        mac.update(SEAL_DOMAIN);
        mac.update(ticket.as_bytes());
        mac.update(b"\0");
        mac.update(revision.to_string().as_bytes());
        mac.update(b"\0");
        mac.update(head.as_bytes());
        mac
    }
}

/// The keys that seal and verify operator docs approvals.
///
/// The secret is `OPENFLOWS_DOCS_GATE_SECRET`, falling back to
/// `CODER_CHAT_HOOK_SECRET` for existing deployments. Either way the seal key
/// is derived from it with a separate label, and both must be at least
/// [`MIN_GATE_SECRET_LEN`] bytes. During a rotation,
/// `OPENFLOWS_DOCS_GATE_SECRET_PREVIOUS` still verifies approvals sealed
/// before it; each seal names its key id, so an approval sealed with a key the
/// controller no longer holds is reported as such and can be re-approved.
#[derive(Clone)]
pub struct DocsGateKeys {
    current: GateKey,
    previous: Option<GateKey>,
}

impl fmt::Debug for DocsGateKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DocsGateKeys")
            .field("current", &self.current.id)
            .field("previous", &self.previous.as_ref().map(|k| &k.id))
            .finish()
    }
}

impl DocsGateKeys {
    /// Keys sealing with (and verifying) `secret` only.
    pub fn new(secret: &str) -> Self {
        Self {
            current: GateKey::derive(secret),
            previous: None,
        }
    }

    /// Also verify approvals sealed with `secret` (a rotated-out secret).
    pub fn with_previous(mut self, secret: &str) -> Self {
        self.previous = Some(GateKey::derive(secret));
        self
    }

    /// Public id of the key new approvals are sealed with.
    pub fn key_id(&self) -> &str {
        &self.current.id
    }

    /// The controller's docs gate keys, or `None` when no secret is configured.
    /// A configured secret that is too short is an error, never ignored.
    pub fn from_env() -> Result<Option<Self>> {
        let env = crate::EnvConfig::from_env()?;
        let configured = |value: Option<String>| value.filter(|s| !s.trim().is_empty());
        let (secret, source) = match configured(env.docs_gate.secret) {
            Some(secret) => (secret, "OPENFLOWS_DOCS_GATE_SECRET"),
            None => match configured(env.hooks.chat_hook_secret) {
                Some(secret) => (secret, "CODER_CHAT_HOOK_SECRET"),
                None => return Ok(None),
            },
        };
        ensure!(
            secret.len() >= MIN_GATE_SECRET_LEN,
            "{source} must be at least {MIN_GATE_SECRET_LEN} bytes to seal docs approvals"
        );
        let mut keys = Self::new(&secret);
        if let Some(previous) = configured(env.docs_gate.previous_secret) {
            ensure!(
                previous.len() >= MIN_GATE_SECRET_LEN,
                "OPENFLOWS_DOCS_GATE_SECRET_PREVIOUS must be at least {MIN_GATE_SECRET_LEN} bytes"
            );
            keys = keys.with_previous(&previous);
        }
        Ok(Some(keys))
    }

    /// Seal an operator approval of `ticket` docs `revision` at `head`.
    pub fn seal(&self, ticket: &str, revision: u64, head: &str) -> String {
        let mac = self
            .current
            .mac(ticket, revision, head)
            .finalize()
            .into_bytes();
        format!("{SEAL_VERSION}:{}:{}", self.current.id, hex(&mac))
    }

    /// The approved head of `record`, if its decision is an approval whose seal
    /// verifies for `ticket` with one of these keys. Callers must use this (not
    /// the raw record fields) before opening or merging a docs PR.
    pub fn verify<'a>(&self, record: &'a DocsRecord, ticket: &str) -> Result<&'a str> {
        let Some(decision) = record.decision.as_ref().filter(|d| d.approved) else {
            bail!("Docs record for {ticket} carries no operator approval");
        };
        let Some((key_id, mac)) = decision
            .seal
            .strip_prefix(SEAL_VERSION)
            .and_then(|rest| rest.strip_prefix(':'))
            .and_then(|rest| rest.split_once(':'))
        else {
            bail!("Docs approval for {ticket} has an invalid seal; it was not issued by the operator CLI");
        };
        let Some(key) = std::iter::once(&self.current)
            .chain(self.previous.as_ref())
            .find(|key| key.id == key_id)
        else {
            bail!(
                "Docs approval for {ticket} was sealed with key {key_id}, which this controller does not hold (current key {}); if the secret was rotated, approve revision {} again with `openflows gate docs`",
                self.current.id,
                decision.revision
            );
        };
        ensure!(
            key.mac(ticket, decision.revision, &decision.head)
                .verify_slice(&decode_hex(mac).unwrap_or_default())
                .is_ok(),
            "Docs approval for {ticket} has an invalid seal; it was not issued by the operator CLI"
        );
        ensure!(
            decision.revision == record.revision
                && record.head.as_deref() == Some(decision.head.as_str()),
            "Docs approval for {ticket} does not match the submitted revision and head"
        );
        Ok(&decision.head)
    }
}

/// [`DocsGateKeys::from_env`] for long-running components: a misconfigured
/// secret is logged and treated as absent, so NEXUS and VESSEL
/// refuse to open or merge docs PRs instead of failing to start.
pub fn gate_keys_from_env_logged() -> Option<DocsGateKeys> {
    match DocsGateKeys::from_env() {
        Ok(keys) => keys,
        Err(error) => {
            tracing::error!(%error, "Docs gate disabled: the docs gate secret is misconfigured");
            None
        }
    }
}

/// Seal with `secret` alone; see [`DocsGateKeys::seal`].
pub fn approval_seal(secret: &str, ticket: &str, revision: u64, head: &str) -> String {
    DocsGateKeys::new(secret).seal(ticket, revision, head)
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(value.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Verify with `secret` alone; see [`DocsGateKeys::verify`].
pub fn verify_approval<'a>(record: &'a DocsRecord, ticket: &str, secret: &str) -> Result<&'a str> {
    DocsGateKeys::new(secret).verify(record, ticket)
}
