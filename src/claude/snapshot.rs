//! The unit of account identity that byte moves around.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};

/// Bumped only when the *stored* representation changes incompatibly.
pub const SCHEMA_VERSION: u32 = 1;

/// Everything that makes Claude Code "logged in as" a particular account.
///
/// `oauth` and `account` are held as opaque JSON on purpose: byte does not
/// model their fields, so any field Anthropic adds travels through a capture
/// and apply cycle untouched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountSnapshot {
    pub schema: u32,
    /// The `claudeAiOauth` object from `.credentials.json`.
    pub oauth: Value,
    /// The `oauthAccount` object from `.claude.json`.
    pub account: Value,
    /// The `userID` value from `.claude.json`.
    pub user_id: Option<String>,
}

impl AccountSnapshot {
    pub fn new(oauth: Value, account: Value, user_id: Option<String>) -> Self {
        Self {
            schema: SCHEMA_VERSION,
            oauth,
            account,
            user_id,
        }
    }

    /// Reassemble a snapshot from its two stored halves (spec §6.3): the
    /// secret `oauth` block, read from the OS keychain, and the non-secret
    /// `account` / `user_id` recorded alongside it in `accounts.json`.
    ///
    /// Deliberately distinct from `new()`, which always stamps the
    /// *current* `SCHEMA_VERSION`. `new()` is only right for a snapshot
    /// freshly captured from the live Claude Code files -- reassembling a
    /// *stored* one must instead carry forward whatever schema version it
    /// was captured under, so `validate()` can still refuse a stored
    /// account that predates a schema bump. Using `new()` here would
    /// silently re-stamp every reassembly as current and defeat that check
    /// (spec risk 2).
    pub fn reassemble(oauth: Value, account: Value, user_id: Option<String>, schema: u32) -> Self {
        Self {
            schema,
            oauth,
            account,
            user_id,
        }
    }

    fn account_str(&self, key: &str) -> Option<&str> {
        self.account.get(key).and_then(Value::as_str)
    }

    pub fn account_uuid(&self) -> Option<&str> {
        self.account_str("accountUuid")
    }

    pub fn email(&self) -> Option<&str> {
        self.account_str("emailAddress")
    }

    pub fn organization_name(&self) -> Option<&str> {
        self.account_str("organizationName")
    }

    pub fn subscription_type(&self) -> Option<&str> {
        self.oauth.get("subscriptionType").and_then(Value::as_str)
    }

    /// A stable identity for this account. Falls back to the email address
    /// when no UUID is present, which keeps pre-UUID snapshots usable.
    pub fn identity(&self) -> Option<&str> {
        self.account_uuid().or_else(|| self.email())
    }

    /// A human label, used when byte auto-names a captured account.
    pub fn default_label(&self) -> String {
        self.email()
            .or_else(|| self.account_uuid())
            .unwrap_or("unknown account")
            .to_string()
    }

    /// Refuse snapshots byte cannot safely write back (spec §7 step 4).
    pub fn validate(&self) -> Result<()> {
        let who = self.default_label();

        if self.schema != SCHEMA_VERSION {
            return Err(Error::SchemaMismatch {
                found: self.schema,
                expected: SCHEMA_VERSION,
            });
        }

        let refresh = self
            .oauth
            .get("refreshToken")
            .and_then(Value::as_str)
            .unwrap_or_default();

        if refresh.is_empty() {
            return Err(Error::InvalidSnapshot {
                account: who,
                reason: "no refresh token; re-authenticate this account with `byte add`".into(),
            });
        }

        // Spec §7 step 4: "refreshToken non-empty, expiresAt parses, schema
        // version known". expiresAt is a Unix-epoch-milliseconds integer in
        // every snapshot Claude Code itself produces; as_i64() is the parse
        // check, rejecting both an absent key and a non-numeric value.
        if self
            .oauth
            .get("expiresAt")
            .and_then(Value::as_i64)
            .is_none()
        {
            return Err(Error::InvalidSnapshot {
                account: who,
                reason: "expiresAt is missing or not a number; re-authenticate this account \
                         with `byte add`"
                    .into(),
            });
        }

        if self.identity().is_none() {
            return Err(Error::InvalidSnapshot {
                account: who,
                reason: "no account UUID or email address".into(),
            });
        }

        Ok(())
    }
}
