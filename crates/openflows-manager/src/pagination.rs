//! Cursor pagination for list endpoints.
//!
//! Lists return opaque cursors plus a `limit` (default 50, maximum 100) and a
//! `next_cursor` that is `null` when there are no further pages. Cursors are
//! base64-encoded opaque strings so clients cannot inject ordering or scope
//! parameters. Stable ordering is guaranteed by sorting on a monotonic key
//! (currently the row's `created_at` + id tiebreaker) within the caller's
//! organization scope.

use crate::error::ManagerError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt::Display;

/// Default and maximum page sizes mandated by the shared API conventions.
pub const DEFAULT_LIMIT: u32 = 50;
pub const MAX_LIMIT: u32 = 100;

/// A sanitized page-size value, clamped into the allowed range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageLimit(u32);

impl PageLimit {
    pub fn new(limit: Option<u32>) -> Self {
        match limit {
            None => PageLimit(DEFAULT_LIMIT),
            Some(n) => PageLimit(n.clamp(1, MAX_LIMIT)),
        }
    }

    pub fn get(&self) -> u32 {
        self.0
    }
}

/// An opaque cursor that encodes the sort position for keyset pagination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    /// The sort timestamp of the last returned row.
    pub created_at: DateTime<Utc>,
    /// A stable tiebreaker id (the row's primary key).
    pub tiebreaker: String,
}

impl Cursor {
    /// Encode the cursor as an opaque base64 string.
    pub fn encode(&self) -> String {
        let payload = serde_json::json!({
            "t": self.created_at.timestamp_micros(),
            "i": self.tiebreaker,
        });
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string())
    }

    /// Decode an opaque cursor string, rejecting malformed input.
    pub fn decode(raw: &str) -> Result<Self, ManagerError> {
        use base64::Engine;
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|_| ManagerError::InvalidInput("malformed cursor".into()))?;
        let value: serde_json::Value = serde_json::from_slice(&decoded)
            .map_err(|_| ManagerError::InvalidInput("malformed cursor".into()))?;
        let ts = value
            .get("t")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ManagerError::InvalidInput("malformed cursor".into()))?;
        let tiebreaker = value
            .get("i")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ManagerError::InvalidInput("malformed cursor".into()))?
            .to_string();
        let created_at = DateTime::from_timestamp_micros(ts)
            .ok_or_else(|| ManagerError::InvalidInput("malformed cursor".into()))?;
        Ok(Cursor {
            created_at,
            tiebreaker,
        })
    }
}

/// A page of results together with the next-cursor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

impl<T> Page<T> {
    /// Build a page from a `limit + 1`-sized fetch, trimming the extra row and
    /// emitting a next cursor when a further page exists.
    pub fn from_fetch(
        mut items: Vec<T>,
        limit: PageLimit,
        cursor_of: impl Fn(&T) -> Cursor,
    ) -> Self {
        let has_more = items.len() > limit.get() as usize;
        items.truncate(limit.get() as usize);
        let next_cursor = if has_more {
            items.last().map(|last| cursor_of(last).encode())
        } else {
            None
        };
        Page { items, next_cursor }
    }
}

/// A generic sort-key helper used to build a stable cursor for a row.
pub fn cursor_for(created_at: DateTime<Utc>, id: impl Display) -> Cursor {
    Cursor {
        created_at,
        tiebreaker: id.to_string(),
    }
}
