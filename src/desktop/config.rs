//! The account state byte owns inside the desktop app's `config.json`.
//!
//! That file is patched, never moved, because it mixes account state with
//! the user's own settings -- `locale`, `userThemeMode`, window sizing,
//! updater state, MCP allowlist caches. Moving it between accounts would
//! drag the user's theme and window layout along with their session.
//!
//! Everything here goes through `JsonDocument`, so every byte it does not
//! explicitly model survives a capture/apply cycle untouched: key order,
//! number formatting, and any key a future app version adds.
//!
//! What byte owns inside the file is a PREFIX, not a list: every top-level
//! key beginning `oauth:`, plus `lastKnownAccountUuid`. That direction
//! matches `desktop::profile`'s denylist and is chosen for the same reason.
//! An allowlist assumes byte knows the full set of account state, and the
//! version-suffixed names are the standing proof that it does not:
//! `oauth:tokenCache` -> `oauth:tokenCacheV2` already happened once. Under
//! an allowlist, the day `oauth:tokenCacheV3` ships, `switch a -> b` writes
//! b's known keys and leaves A'S V3 sitting in the file -- the app may then
//! authenticate as a under b's identity, and byte cannot notice. Unlike the
//! directory side, an unrecognised key does not strand; it leaks.
//!
//! The cost of the prefix is the mirror image: an `oauth:`-prefixed key that
//! is NOT account state (a port number, a feature flag) would travel with
//! the account and be removed for one that lacks it. No such key exists in
//! any observed `config.json`, and the failure it would cause -- a setting
//! resetting -- is recoverable, where a leaked token is not.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::claude::document::JsonDocument;
use crate::error::Result;

/// Everything under this prefix is the desktop app's OAuth session state.
const OAUTH_PREFIX: &str = "oauth:";
const ACCOUNT_UUID: &str = "lastKnownAccountUuid";

/// The two `config.json` keys the previous, allowlisted `DesktopOauth`
/// modelled as the struct fields `token_cache` and `token_cache_v2`.
/// Profiles parked by that build are on real disks, so the `oauth.json`
/// files it wrote must keep loading.
const LEGACY_TOKEN_CACHE: &str = "oauth:tokenCache";
const LEGACY_TOKEN_CACHE_V2: &str = "oauth:tokenCacheV2";

/// The desktop app's account identity, as byte stores it.
///
/// `account_uuid` is `Option` because restoring `None` must REMOVE the key
/// rather than leave the previous account's value in place; the same is true
/// of every `oauth:` key an account does not carry, which is expressed by
/// its absence from `oauth`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "StoredForm", into = "StoredForm")]
pub struct DesktopOauth {
    /// Every top-level `oauth:` key, under its real `config.json` name.
    pub oauth: Map<String, Value>,
    /// `lastKnownAccountUuid`.
    pub account_uuid: Option<Value>,
}

/// Whose session a `config.json` says it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// No `lastKnownAccountUuid`, or an explicit JSON `null`. Both carry no
    /// identity, so both read the same way.
    Absent,
    Account(String),
    /// Present but not a string -- byte cannot read whose session this is,
    /// which is a different fact from there being none.
    Unreadable,
}

impl DesktopOauth {
    /// Does this capture carry no session at all?
    ///
    /// Used as the last guard before byte overwrites an account's parked
    /// `oauth.json`: an empty capture over a store that still holds a real
    /// profile means the live directory is not the session byte thinks it
    /// is.
    pub fn is_empty(&self) -> bool {
        self.oauth.is_empty() && self.account_uuid.as_ref().is_none_or(Value::is_null)
    }

    pub fn identity(&self) -> Identity {
        match self.account_uuid.as_ref() {
            None | Some(Value::Null) => Identity::Absent,
            Some(Value::String(s)) => Identity::Account(s.clone()),
            Some(_) => Identity::Unreadable,
        }
    }
}

/// Read the owned keys out of `config.json`.
pub fn capture(config_path: &Path) -> Result<DesktopOauth> {
    let doc = JsonDocument::load_or_empty(config_path)?;

    let mut oauth = Map::new();
    for key in doc.top_level_keys() {
        if key.starts_with(OAUTH_PREFIX)
            && let Some(value) = doc.get(&key)
        {
            oauth.insert(key, value.clone());
        }
    }

    Ok(DesktopOauth {
        oauth,
        account_uuid: doc.get(ACCOUNT_UUID).cloned(),
    })
}

/// Write the owned keys into `config.json`, removing any the account lacks.
pub fn apply(config_path: &Path, oauth: &DesktopOauth, backup_dir: &Path) -> Result<()> {
    let mut doc = JsonDocument::load_or_empty(config_path)?;

    // Removal first, and by prefix rather than by name: leaving a key the
    // incoming account does not carry would keep the previous account's
    // token cache under the new account's session -- and the whole point of
    // a prefix is that byte must be able to remove a key it has never heard
    // of. An existing key that IS carried keeps its position in the
    // document, so a capture/apply round trip is byte-for-byte a no-op.
    for key in doc.top_level_keys() {
        if key.starts_with(OAUTH_PREFIX) && !oauth.oauth.contains_key(&key) {
            doc.remove(&key);
        }
    }
    for (key, value) in &oauth.oauth {
        doc.set(key, value.clone());
    }
    match &oauth.account_uuid {
        Some(v) => doc.set(ACCOUNT_UUID, v.clone()),
        None => doc.remove(ACCOUNT_UUID),
    }

    doc.save(config_path, backup_dir)
}

/// Remove every owned key, leaving the app signed out.
pub fn clear(config_path: &Path, backup_dir: &Path) -> Result<()> {
    apply(config_path, &DesktopOauth::default(), backup_dir)
}

/// [`apply`], unless `config.json` already says exactly this.
///
/// For recovery, which is reached differently from a switch: it runs at the
/// start of EVERY byte command, and the same journal can be reached
/// repeatedly, because a patch that fails is deliberately left on disk for
/// the next command to retry. Its commonest case by far is a reversal whose
/// `config.json` was never patched in the first place -- the file already
/// describes the session being restored, and writing it anyway would back
/// up, rewrite and verify a file byte does not own, to no effect, on every
/// pass. Skipping that keeps a repair idempotent, and stops a `config.json`
/// byte cannot write from failing a repair that had nothing to write.
pub fn apply_if_changed(config_path: &Path, oauth: &DesktopOauth, backup_dir: &Path) -> Result<()> {
    if capture(config_path)? == *oauth {
        return Ok(());
    }
    apply(config_path, oauth, backup_dir)
}

/// How a parked account's `oauth.json` read back.
///
/// The three answers are deliberately distinct because they call for
/// different behaviour, and collapsing two of them is what let a truncated
/// file sign the user out while byte reported a clean switch.
#[derive(Debug)]
pub enum StoredOauth {
    /// No such file. Legitimate: a profile parked by a build older than
    /// this one, before byte wrote `oauth.json` at all.
    Absent,
    Loaded(DesktopOauth),
    /// The file is there and byte could not read it. Never silently
    /// defaulted: `DesktopOauth::default()` means "remove every key", so
    /// defaulting here installs an account's cookies and then signs the app
    /// out of it.
    Unreadable(String),
}

/// Read a parked account's `oauth.json`, classifying rather than failing.
///
/// Infallible by contract. Every caller reaches this AFTER the renames have
/// committed, where a switch that already happened must never be turned
/// into an `Err` by a reporting detail -- the classification is how the
/// caller tells the truth about it instead.
pub fn read_stored(path: &Path) -> StoredOauth {
    match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => StoredOauth::Absent,
        Err(e) => StoredOauth::Unreadable(e.to_string()),
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(oauth) => StoredOauth::Loaded(oauth),
            Err(e) => StoredOauth::Unreadable(e.to_string()),
        },
    }
}

/// `oauth.json`'s on-disk shape, current and previous.
///
/// Migration happens on READ rather than by rewriting parked files: byte
/// must never need write access to a store to restore from it, and a
/// rewrite would have to happen while switching INTO an account -- the one
/// moment its parked copy is the only copy of that session.
#[derive(Serialize, Deserialize)]
struct StoredForm {
    #[serde(default)]
    oauth: Map<String, Value>,
    /// Always serialised, `null` when there is no identity, so that "byte
    /// captured no uuid" is a fact on disk rather than an omission.
    #[serde(default)]
    account_uuid: Option<Value>,
    /// Previous format, read but never written back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token_cache: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token_cache_v2: Option<Value>,
}

impl From<StoredForm> for DesktopOauth {
    fn from(stored: StoredForm) -> Self {
        let mut oauth = stored.oauth;
        for (config_key, value) in [
            (LEGACY_TOKEN_CACHE, stored.token_cache),
            (LEGACY_TOKEN_CACHE_V2, stored.token_cache_v2),
        ] {
            // The current shape wins where both are present: a file holding
            // both was written by this build, and `oauth` is what it wrote.
            if let Some(value) = value
                && !oauth.contains_key(config_key)
            {
                oauth.insert(config_key.to_string(), value);
            }
        }
        Self {
            oauth,
            account_uuid: stored.account_uuid,
        }
    }
}

impl From<DesktopOauth> for StoredForm {
    fn from(oauth: DesktopOauth) -> Self {
        Self {
            oauth: oauth.oauth,
            account_uuid: oauth.account_uuid,
            token_cache: None,
            token_cache_v2: None,
        }
    }
}
