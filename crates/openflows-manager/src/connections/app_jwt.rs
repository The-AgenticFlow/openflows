//! GitHub App JWT signing, isolated behind a testable trait.
//!
//! The Manager signs short-lived RS256 App JWTs from the configured GitHub App
//! id and private key (resolved through the secret-provider abstraction). The
//! JWT proves to GitHub that a request comes from the Openflows App and is used
//! only for App-level operations (installation metadata lookups and
//! installation-token exchange). It is never persisted, never logged, and never
//! handed to a tenant runtime.

use crate::error::ManagerError;
use async_trait::async_trait;
use chrono::Utc;

/// The App JWT issuer/audience as required by GitHub's API.
pub const GITHUB_JWT_ISSUER: &str = "https://github.com/apps";
/// The default App JWT lifetime. GitHub requires exp <= iat + 10 minutes.
pub const APP_JWT_LIFETIME_SECS: i64 = 540; // 9 minutes
/// Clock-skew allowance applied to `iat` (issued slightly in the past).
pub const APP_JWT_IAT_SKEW_SECS: i64 = 60;

/// The claims encoded into an App JWT.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AppJwtClaims {
    /// The GitHub App numeric id.
    pub iss: i64,
    /// Issued-at (Unix seconds), pulled back by a small skew allowance.
    pub iat: i64,
    /// Expiry (Unix seconds); must be within 10 minutes of `iat`.
    pub exp: i64,
}

/// A signed App JWT. The `Debug` impl redacts the token so it can never leak
/// into logs or error messages.
pub struct AppJwt(pub String);

impl std::fmt::Debug for AppJwt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AppJwt([REDACTED])")
    }
}

/// A parsed/signed JWT for verification tests.
#[derive(Clone)]
pub struct SignedAppJwt {
    /// The raw token (kept private; use the `Debug`-redacted `AppJwt` in
    /// production paths).
    token: String,
    pub claims: AppJwtClaims,
}

impl std::fmt::Debug for SignedAppJwt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignedAppJwt")
            .field("token", &"[REDACTED]")
            .field("claims", &self.claims)
            .finish()
    }
}

impl SignedAppJwt {
    /// The raw token. Callers must not log or persist it.
    pub fn raw(&self) -> &str {
        &self.token
    }
    /// Parse a JWT's claims (without verifying the signature) — used only in
    /// tests to inspect the produced token.
    pub fn claims(&self) -> &AppJwtClaims {
        &self.claims
    }
}

/// Signs GitHub App JWTs using the configured App id and private key.
#[async_trait]
pub trait AppSigner: Send + Sync {
    /// Sign a fresh short-lived App JWT.
    async fn sign(&self) -> Result<SignedAppJwt, ManagerError>;
}

/// A [`AppSigner`] that reads the RSA private key from a secret provider and
/// signs with RS256.
pub struct RealAppSigner {
    app_id: i64,
    key: Vec<u8>,
}

impl RealAppSigner {
    /// Build a signer with the App id and an already-resolved PEM private key.
    pub fn new(app_id: i64, key: Vec<u8>) -> Self {
        RealAppSigner { app_id, key }
    }

    fn sign_sync(&self) -> Result<SignedAppJwt, ManagerError> {
        let now = Utc::now().timestamp();
        let claims = AppJwtClaims {
            iss: self.app_id,
            iat: now - APP_JWT_IAT_SKEW_SECS,
            exp: now + APP_JWT_LIFETIME_SECS,
        };
        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        let encoding_key = jsonwebtoken::EncodingKey::from_rsa_pem(&self.key)
            .map_err(|e| ManagerError::Config(format!("invalid GitHub App private key: {e}")))?;
        let token = jsonwebtoken::encode(&header, &claims, &encoding_key)
            .map_err(|e| ManagerError::Service(anyhow::anyhow!("failed to sign App JWT: {e}")))?;
        Ok(SignedAppJwt { token, claims })
    }
}

#[async_trait]
impl AppSigner for RealAppSigner {
    async fn sign(&self) -> Result<SignedAppJwt, ManagerError> {
        // Signing is CPU-bound and fast; a blocking call is acceptable and keeps
        // the async boundary simple. No database transaction is held.
        self.sign_sync()
    }
}

/// A deterministic fixture signer for tests, producing a fixed JWT with the
/// given claims and no network/secret dependency.
pub struct FixtureAppSigner {
    claims: AppJwtClaims,
}

impl FixtureAppSigner {
    pub fn new(claims: AppJwtClaims) -> Self {
        FixtureAppSigner { claims }
    }
}

#[async_trait]
impl AppSigner for FixtureAppSigner {
    async fn sign(&self) -> Result<SignedAppJwt, ManagerError> {
        Ok(SignedAppJwt {
            token: "fixture.jwt.token".to_string(),
            claims: self.claims.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway RSA-2048 private key (PEM) for tests.
    pub(crate) fn test_rsa_key_pem() -> Vec<u8> {
        // Generated once for tests; not a production secret.
        include_bytes!("../../testdata/test_rsa_2048.pem").to_vec()
    }

    #[tokio::test]
    async fn real_signer_produces_valid_claims() {
        let signer = RealAppSigner::new(12345, test_rsa_key_pem());
        let before = Utc::now().timestamp();
        let jwt = signer.sign().await.unwrap();
        assert_eq!(jwt.claims.iss, 12345);
        let now = Utc::now().timestamp();
        assert!(
            (before - APP_JWT_IAT_SKEW_SECS..=now - APP_JWT_IAT_SKEW_SECS)
                .contains(&jwt.claims.iat)
        );
        assert!(
            (before + APP_JWT_LIFETIME_SECS..=now + APP_JWT_LIFETIME_SECS)
                .contains(&jwt.claims.exp)
        );
        // Lifetime within GitHub's 10-minute bound.
        assert!(jwt.claims.exp - jwt.claims.iat <= 600);
        assert!(jwt.claims.exp - jwt.claims.iat >= 60);
        assert!(!jwt.raw().is_empty());
    }

    #[tokio::test]
    async fn real_signer_signs_rs256_and_verifies_with_public_key() {
        let signer = RealAppSigner::new(12345, test_rsa_key_pem());
        let jwt = signer.sign().await.unwrap();
        // Verify the signature using the matching public key.
        let public = include_bytes!("../../testdata/test_rsa_2048_pub.pem");
        let key = jsonwebtoken::DecodingKey::from_rsa_pem(public).unwrap();
        let data = jsonwebtoken::decode::<AppJwtClaims>(
            jwt.raw(),
            &key,
            &jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256),
        )
        .unwrap();
        assert_eq!(data.claims.iss, 12345);
    }

    #[test]
    fn app_jwt_debug_is_redacted() {
        let jwt = AppJwt("supersecret".to_string());
        assert_eq!(format!("{:?}", jwt), "AppJwt([REDACTED])");
    }

    #[test]
    fn invalid_key_fails_closed() {
        let signer = RealAppSigner::new(1, b"not a pem".to_vec());
        let result = signer.sign_sync();
        assert!(result.is_err());
    }
}
