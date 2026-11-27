//! A GitHub token that can be replaced while every client sharing it keeps running.

use std::fmt;
use std::sync::{Arc, RwLock};

use crate::GithubRestClient;

/// A bearer token shared by every clone of a client, so a rotated hosted
/// installation token reaches all of them at once.
#[derive(Clone, Default)]
pub struct SharedToken(Arc<RwLock<String>>);

impl SharedToken {
    pub fn new(token: impl Into<String>) -> Self {
        Self(Arc::new(RwLock::new(token.into())))
    }

    /// Replace the token. A poisoned lock is recovered: the guarded value is a
    /// plain string, so a panicking writer cannot leave it half-written.
    pub fn set(&self, token: impl Into<String>) {
        *self.0.write().unwrap_or_else(|error| error.into_inner()) = token.into();
    }

    pub(crate) fn get(&self) -> String {
        self.0
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl From<String> for SharedToken {
    fn from(token: String) -> Self {
        Self::new(token)
    }
}

impl From<&str> for SharedToken {
    fn from(token: &str) -> Self {
        Self::new(token)
    }
}

impl fmt::Debug for SharedToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SharedToken(<redacted>)")
    }
}

impl GithubRestClient {
    /// The token this client and its clones authenticate with, for rotating it in place.
    pub fn token_handle(&self) -> SharedToken {
        self.token.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_one_token() {
        let token = SharedToken::new("old");
        let other = token.clone();
        token.set("new");
        assert_eq!(other.get(), "new");
    }

    #[test]
    fn debug_never_prints_the_token() {
        assert!(!format!("{:?}", SharedToken::new("ghs_secret")).contains("ghs_secret"));
    }
}
