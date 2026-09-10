//! Non-secret account metadata, stored as plain JSON.
//!
//! Kept separate from the secret store so that listing accounts — including
//! rendering the tray menu — never needs to unlock the OS keychain.
//!
//! Since the keychain-size fix, this is also where the non-secret half of a
//! captured snapshot lives: the raw `oauthAccount` object (`account` below)
//! and `userID` (`user_id`), which used to travel inside the same keychain
//! entry as the secret `oauth` block. Splitting them out is what keeps a
//! single account's keychain payload under the OS credential store's size
//! limit (Windows Credential Manager's is the tightest, at 1280 characters
//! per entry once UTF-16 encoding is accounted for) -- see
//! `store::secrets` for the other half of the split.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::atomic;
use crate::claude::snapshot::AccountSnapshot;
use crate::error::{Error, Result};

/// Bumped when `accounts.json`'s own document shape changes -- e.g. when a
/// field is added to or removed from [`AccountMeta`], as happened when the
/// keychain-size fix added `account`, `user_id`, and `credential_schema`.
/// Distinct from [`crate::claude::snapshot::SCHEMA_VERSION`], which versions
/// a captured *credential* (the `oauth`/`account` pair), not the metadata
/// file's own layout -- the two happen to both be at version 1/2 today but
/// version independently.
const METADATA_SCHEMA: u32 = 2;

/// Everything shown about an account without touching its secrets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountMeta {
    pub uuid: String,
    pub label: String,
    pub email: Option<String>,
    pub organization_name: Option<String>,
    pub subscription_type: Option<String>,
    /// The full `oauthAccount` object, held opaque exactly like
    /// [`AccountSnapshot::account`] so that switching back to this account
    /// restores every field Anthropic put there -- including ones byte does
    /// not model -- not just the subset mirrored above for display without
    /// a keychain unlock. Never holds a secret: `oauthAccount` is identity
    /// and profile data (email, org, billing, roles), not credentials --
    /// those live only in the `oauth` block, which never reaches this file.
    pub account: Value,
    /// The `userID` value from `.claude.json`.
    pub user_id: Option<String>,
    /// The [`AccountSnapshot::schema`] this account was captured under. See
    /// [`crate::claude::snapshot::AccountSnapshot::reassemble`] for why this
    /// must be threaded through rather than assumed current.
    pub credential_schema: u32,
    pub added_at: String,
    pub last_used_at: Option<String>,
    /// The parked desktop profile, if one has been captured.
    ///
    /// `#[serde(default)]` and skipped when absent, deliberately: this is
    /// purely additive, so it must not bump the accounts schema and must not
    /// appear in the files of users who never touch the desktop app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_profile: Option<DesktopProfileRecord>,
}

/// What byte has parked for one account's desktop session.
///
/// Absent means never captured, which is what drives the "park the old
/// profile, leave a fresh one" behaviour on a first switch (design
/// decision 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopProfileRecord {
    /// RFC 3339, matching `added_at`.
    pub captured_at: String,
    /// Size on disk when captured, for `byte list` and for warning about
    /// accumulation.
    pub bytes: u64,
}

/// The contents of `accounts.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountsFile {
    pub schema: u32,
    pub active: Option<String>,
    pub accounts: Vec<AccountMeta>,
}

impl Default for AccountsFile {
    fn default() -> Self {
        Self {
            schema: METADATA_SCHEMA,
            active: None,
            accounts: Vec::new(),
        }
    }
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| String::from("unknown"))
}

impl AccountsFile {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(Error::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };

        // Parsed as a bare `Value` first, specifically so the schema can be
        // checked *before* attempting to deserialize into `AccountsFile` --
        // an incompatible schema (e.g. a pre-keychain-size-fix file with no
        // `account`/`user_id`/`credential_schema` fields) would otherwise
        // surface as a generic, confusing `Error::Parse` about a missing
        // field rather than the clear "update byte" message this produces.
        // A missing `schema` key reads as version 0, which can never match
        // `METADATA_SCHEMA` and so is refused the same way.
        let value: Value = serde_json::from_str(&raw).map_err(|source| Error::Parse {
            path: path.to_path_buf(),
            source,
        })?;

        let found = value.get("schema").and_then(Value::as_u64).unwrap_or(0) as u32;
        if found != METADATA_SCHEMA {
            return Err(Error::AccountsSchemaMismatch {
                found,
                expected: METADATA_SCHEMA,
            });
        }

        serde_json::from_value(value).map_err(|source| Error::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn save(&self, path: &Path, backup_dir: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(|source| Error::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        atomic::backup(path, backup_dir)?;
        atomic::write(path, format!("{text}\n").as_bytes())?;

        // Without this, accounts.json accumulates one backup per capture,
        // switch, rename, and remove, unbounded -- unlike .claude.json and
        // .credentials.json, which JsonDocument::save already prunes to the
        // same limit.
        if let Some(name) = path.file_name() {
            atomic::prune(
                backup_dir,
                &name.to_string_lossy(),
                atomic::BACKUP_RETENTION,
            );
        }
        Ok(())
    }

    /// Insert or refresh the entry for `uuid`, returning the stored
    /// metadata. A label the user set with `rename` is preserved.
    ///
    /// Takes `uuid` explicitly rather than deriving it from `snapshot` and
    /// falling back to the literal string `"unknown"` when identity is
    /// absent. Every current call site already has a validated identity in
    /// hand (from `AccountSnapshot::validate()`, or a `resolve()`d existing
    /// account), so the fallback was always dead code -- but it is the same
    /// hazard class as the credential-loss family: a future caller that
    /// upserts before validating would file that snapshot under the shared
    /// key `"unknown"`, silently colliding with any other unidentifiable
    /// account filed the same way.
    pub fn upsert_from(&mut self, uuid: &str, snapshot: &AccountSnapshot) -> AccountMeta {
        let existing = self.accounts.iter().position(|a| a.uuid == uuid);

        let meta = AccountMeta {
            uuid: uuid.to_string(),
            label: match existing {
                Some(i) => self.accounts[i].label.clone(),
                None => snapshot.default_label(),
            },
            email: snapshot.email().map(str::to_string),
            organization_name: snapshot.organization_name().map(str::to_string),
            subscription_type: snapshot.subscription_type().map(str::to_string),
            // The non-secret half of the keychain-size split: stored here,
            // verbatim and opaque, so a later reassemble() can hand
            // `apply()` back every field this account's `oauthAccount`
            // object had -- not just the display subset mirrored above.
            account: snapshot.account.clone(),
            user_id: snapshot.user_id.clone(),
            credential_schema: snapshot.schema,
            added_at: match existing {
                Some(i) => self.accounts[i].added_at.clone(),
                None => now_rfc3339(),
            },
            last_used_at: existing.and_then(|i| self.accounts[i].last_used_at.clone()),
            // Carried forward exactly like `last_used_at` above, not reset:
            // this is stamped by a wholly separate operation (the desktop
            // swap's `resolve_mut` call, task 8), not by this credential
            // sync. Resetting it here would mean an ordinary CLI-only
            // switch -- one that never touches the desktop app at all --
            // silently erases the record of a profile that is still sitting
            // on disk, breaking the "absent means never captured" invariant
            // the whole feature depends on.
            desktop_profile: existing.and_then(|i| self.accounts[i].desktop_profile.clone()),
        };

        match existing {
            Some(i) => self.accounts[i] = meta.clone(),
            None => self.accounts.push(meta.clone()),
        }
        meta
    }

    /// Find an account by label, email, or UUID prefix.
    pub fn resolve(&self, query: &str) -> Result<&AccountMeta> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Err(Error::NoSuchAccount(query.to_string()));
        }

        let exact: Vec<&AccountMeta> = self
            .accounts
            .iter()
            .filter(|a| {
                a.label.to_lowercase() == q
                    || a.email.as_deref().map(str::to_lowercase) == Some(q.clone())
                    || a.uuid.to_lowercase() == q
            })
            .collect();

        if exact.len() == 1 {
            return Ok(exact[0]);
        }
        if exact.len() > 1 {
            return Err(Error::AmbiguousAccount {
                query: query.to_string(),
                count: exact.len(),
            });
        }

        let prefixed: Vec<&AccountMeta> = self
            .accounts
            .iter()
            .filter(|a| {
                a.uuid.to_lowercase().starts_with(&q) || a.label.to_lowercase().starts_with(&q)
            })
            .collect();

        match prefixed.len() {
            1 => Ok(prefixed[0]),
            0 => Err(Error::NoSuchAccount(query.to_string())),
            n => Err(Error::AmbiguousAccount {
                query: query.to_string(),
                count: n,
            }),
        }
    }

    /// `resolve`'s `&mut` mirror, for a caller that needs to update the
    /// matched account in place (e.g. stamping a `desktop_profile` record
    /// after a capture). Matches `resolve`'s lookup semantics exactly --
    /// same exact-match fields, same ambiguity handling, same uuid/label
    /// prefix fallback -- expressed over indices rather than collected
    /// `&AccountMeta` references, since multiple live mutable borrows out of
    /// `self.accounts` cannot coexist the way `resolve`'s immutable ones do.
    /// Keep this in sync with `resolve` if its matching rules ever change.
    pub fn resolve_mut(&mut self, query: &str) -> Result<&mut AccountMeta> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Err(Error::NoSuchAccount(query.to_string()));
        }

        let exact: Vec<usize> = self
            .accounts
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                a.label.to_lowercase() == q
                    || a.email.as_deref().map(str::to_lowercase) == Some(q.clone())
                    || a.uuid.to_lowercase() == q
            })
            .map(|(i, _)| i)
            .collect();

        if exact.len() == 1 {
            return Ok(&mut self.accounts[exact[0]]);
        }
        if exact.len() > 1 {
            return Err(Error::AmbiguousAccount {
                query: query.to_string(),
                count: exact.len(),
            });
        }

        let prefixed: Vec<usize> = self
            .accounts
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                a.uuid.to_lowercase().starts_with(&q) || a.label.to_lowercase().starts_with(&q)
            })
            .map(|(i, _)| i)
            .collect();

        match prefixed.len() {
            1 => Ok(&mut self.accounts[prefixed[0]]),
            0 => Err(Error::NoSuchAccount(query.to_string())),
            n => Err(Error::AmbiguousAccount {
                query: query.to_string(),
                count: n,
            }),
        }
    }

    pub fn rename(&mut self, uuid: &str, label: &str) -> Result<AccountMeta> {
        let idx = self
            .accounts
            .iter()
            .position(|a| a.uuid == uuid)
            .ok_or_else(|| Error::NoSuchAccount(uuid.to_string()))?;
        self.accounts[idx].label = label.to_string();
        Ok(self.accounts[idx].clone())
    }

    pub fn remove(&mut self, uuid: &str) -> Result<AccountMeta> {
        let idx = self
            .accounts
            .iter()
            .position(|a| a.uuid == uuid)
            .ok_or_else(|| Error::NoSuchAccount(uuid.to_string()))?;
        let removed = self.accounts.remove(idx);
        if self.active.as_deref() == Some(uuid) {
            self.active = None;
        }
        Ok(removed)
    }

    pub fn set_active(&mut self, uuid: &str) {
        self.active = Some(uuid.to_string());
        if let Some(a) = self.accounts.iter_mut().find(|a| a.uuid == uuid) {
            a.last_used_at = Some(now_rfc3339());
        }
    }

    pub fn active_meta(&self) -> Option<&AccountMeta> {
        let active = self.active.as_deref()?;
        self.accounts.iter().find(|a| a.uuid == active)
    }
}
