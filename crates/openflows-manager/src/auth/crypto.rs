//! Authentication cryptographic primitives.
//!
//! These helpers implement the shared persistence rules for secrets:
//!   * High-entropy random tokens (device secrets, invitation tokens, access
//!     and refresh credentials, OAuth state) are hashed with SHA-256 when only
//!     verification is needed; the plaintext is returned at most once and
//!     never persisted.
//!   * Recoverable OAuth material (PKCE verifier, retained user tokens) is
//!     encrypted with an authenticated envelope key and a stored key version.
//!   * Direct secret comparisons use constant-time equality; database lookups use hashes.
//!
//! This module intentionally contains no database or network access; callers
//! combine these primitives with repositories and adapters.

use crate::error::ManagerError;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// The size of an OAuth `state` value / device secret / invitation token in
/// bytes (32 bytes = 256 bits).
pub const TOKEN_BYTES: usize = 32;
/// The size of a random nonce in bytes (96-bit AES-GCM nonce).
pub const NONCE_BYTES: usize = 12;
/// The length of a base64url (no padding) encoded 32-byte token.
pub const TOKEN_B64_LEN: usize = 43;

/// A version tag attached to ciphertext so keys can be rotated independently
/// of the stored blob.
pub type KeyVersion = u32;

/// A raw 256-bit secret, kept out of the public DTO surface.
pub struct Secret(pub [u8; TOKEN_BYTES]);

impl Secret {
    /// Generate a fresh cryptographically-random 256-bit secret.
    pub fn generate() -> Self {
        let mut bytes = [0u8; TOKEN_BYTES];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Secret(bytes)
    }

    /// Encode as a URL-safe base64 string (no padding). This is the form
    /// delivered to a client exactly once.
    pub fn encode(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.0)
    }

    /// The SHA-256 hex digest used for equality checks and storage.
    pub fn hash(&self) -> String {
        hash_token(&self.encode())
    }
}

/// SHA-256 hex digest of an arbitrary string value. Used to hash a raw token
/// string (e.g. an invitation token or device secret) that arrived from the
/// wire rather than being generated here.
pub fn hash_token(raw: &str) -> String {
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}

/// Constant-time comparison of two byte slices.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}

/// A challenge verifier pair for PKCE (RFC 7636, S256 method).
pub struct PkcePair {
    /// The random 32-byte verifier, base64url-encoded. Kept secret.
    pub verifier: String,
    /// The S256 code challenge, base64url-encoded SHA-256 of the verifier.
    pub challenge: String,
}

impl PkcePair {
    /// Generate a fresh PKCE pair.
    pub fn new() -> Self {
        let secret = Secret::generate();
        let verifier = secret.encode();
        let digest = Sha256::digest(verifier.as_bytes());
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
        PkcePair {
            verifier,
            challenge,
        }
    }

    /// Verify a presented code_verifier against the stored challenge.
    pub fn verify(&self, presented_verifier: &str) -> bool {
        let digest = Sha256::digest(presented_verifier.as_bytes());
        let presented_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
        ct_eq(presented_challenge.as_bytes(), self.challenge.as_bytes())
    }
}

impl Default for PkcePair {
    fn default() -> Self {
        Self::new()
    }
}

/// A versioned authenticated-encryption envelope for recoverable secrets.
///
/// The same 32-byte key is derived from the configured base secret for the
/// given purpose and version. The caller stores the version beside ciphertext.
/// The current config supports version 1; a retained key ring and online key
/// rotation remain an operator-adapter requirement.
#[derive(Clone)]
pub struct EnvelopeCipher {
    /// A 32-byte key material derived deterministically from the configured
    /// deployment key and the purpose name.
    key: [u8; 32],
    /// The active key version this cipher was constructed with.
    version: KeyVersion,
}

impl EnvelopeCipher {
    /// Derive a purpose-scoped 32-byte key from the configured 32-byte
    /// deployment master key. A compromised purpose key never exposes another
    /// purpose's ciphertext.
    pub fn derive(master: &[u8; 32], purpose: &str, version: KeyVersion) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"openflows-envelope-v1");
        hasher.update(master);
        hasher.update(purpose.as_bytes());
        hasher.update(version.to_le_bytes());
        let key: [u8; 32] = hasher.finalize().into();
        EnvelopeCipher { key, version }
    }

    pub fn version(&self) -> KeyVersion {
        self.version
    }

    /// Encrypt `plaintext` with a fresh random nonce. Returns
    /// `nonce || ciphertext` (the version is stored separately by the caller).
    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, ManagerError> {
        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|e| ManagerError::Service(anyhow::anyhow!("invalid key: {e}")))?;
        let mut nonce_bytes = [0u8; NONCE_BYTES];
        rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ct = cipher
            .encrypt(
                nonce,
                Payload {
                    msg: plaintext,
                    aad: &[],
                },
            )
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("encryption failed")))?;
        let mut out = Vec::with_capacity(nonce_bytes.len() + ct.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Decrypt `sealed` produced by [`EnvelopeCipher::seal`]. `sealed` is
    /// `nonce || ciphertext`. Authentication failures (wrong key/version or
    /// tampering) map to a generic error.
    pub fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, ManagerError> {
        if sealed.len() < NONCE_BYTES {
            return Err(ManagerError::Service(anyhow::anyhow!(
                "sealed value too short"
            )));
        }
        let (nonce_bytes, ct) = sealed.split_at(NONCE_BYTES);
        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|e| ManagerError::Service(anyhow::anyhow!("invalid key: {e}")))?;
        let nonce = Nonce::from_slice(nonce_bytes);
        cipher
            .decrypt(nonce, Payload { msg: ct, aad: &[] })
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("decryption/authentication failed")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_roundtrip_and_hash_stability() {
        let s = Secret::generate();
        let encoded = s.encode();
        assert_eq!(encoded.len(), TOKEN_B64_LEN);
        // The hash is stable across calls and distinct per secret.
        assert_eq!(s.hash(), hash_token(&encoded));
        let s2 = Secret::generate();
        assert_ne!(s.hash(), s2.hash());
        // Re-encoding a fresh secret from the same bytes yields the same token.
        let s3 = Secret(s.0);
        assert_eq!(s3.encode(), encoded);
        assert_eq!(s3.hash(), s.hash());
    }

    #[test]
    fn ct_eq_rejects_different_lengths() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"xyz"));
        assert!(!ct_eq(b"abc", b"ab"));
    }

    #[test]
    fn pkce_verifies_only_original_verifier() {
        let pair = PkcePair::new();
        assert!(pair.verify(&pair.verifier));
        assert!(!pair.verify("wrong-verifier"));
    }

    #[test]
    fn envelope_seal_open_roundtrip_and_tamper_detection() {
        let master = [7u8; 32];
        let cipher = EnvelopeCipher::derive(&master, "test", 1);
        let sealed = cipher.seal(b"secret-value").unwrap();
        assert_eq!(cipher.open(&sealed).unwrap(), b"secret-value");

        // A different version/master must fail to decrypt.
        let other = EnvelopeCipher::derive(&[8u8; 32], "test", 1);
        assert!(other.open(&sealed).is_err());

        // Tampering with the ciphertext must be rejected.
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        assert!(cipher.open(&tampered).is_err());
    }

    #[test]
    fn derive_is_purpose_scoped() {
        let master = [1u8; 32];
        let a = EnvelopeCipher::derive(&master, "purpose-a", 1);
        let b = EnvelopeCipher::derive(&master, "purpose-b", 1);
        assert_ne!(a.key, b.key);
        assert_eq!(a.key, EnvelopeCipher::derive(&master, "purpose-a", 1).key);
    }
}
