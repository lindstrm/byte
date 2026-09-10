//! The three keys byte owns inside the desktop app's `config.json`.
//!
//! That file is patched, never moved, because it mixes account state with
//! the user's own settings -- `locale`, `userThemeMode`, window sizing,
//! updater state, MCP allowlist caches. Moving it between accounts would
//! drag the user's theme and window layout along with their session.
//!
//! Everything here goes through `JsonDocument`, so every byte byte does not
//! explicitly model survives a capture/apply cycle untouched: key order,
//! number formatting, and any key a future app version adds.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::claude::document::JsonDocument;
use crate::error::Result;

const TOKEN_CACHE: &str = "oauth:tokenCache";
const TOKEN_CACHE_V2: &str = "oauth:tokenCacheV2";
const ACCOUNT_UUID: &str = "lastKnownAccountUuid";

/// The desktop app's account identity, as byte stores it.
///
/// Each field is `Option` because an account captured before a given key
/// existed simply has nothing to restore for it -- and because restoring
/// `None` must REMOVE the key rather than leave the previous account's
/// value in place.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopOauth {
    pub token_cache: Option<Value>,
    pub token_cache_v2: Option<Value>,
    pub account_uuid: Option<Value>,
}

/// Read the owned keys out of `config.json`.
pub fn capture(config_path: &Path) -> Result<DesktopOauth> {
    let doc = JsonDocument::load_or_empty(config_path)?;
    Ok(DesktopOauth {
        token_cache: doc.get(TOKEN_CACHE).cloned(),
        token_cache_v2: doc.get(TOKEN_CACHE_V2).cloned(),
        account_uuid: doc.get(ACCOUNT_UUID).cloned(),
    })
}

/// Write the owned keys into `config.json`, removing any the account lacks.
pub fn apply(config_path: &Path, oauth: &DesktopOauth, backup_dir: &Path) -> Result<()> {
    let mut doc = JsonDocument::load_or_empty(config_path)?;

    for (key, value) in [
        (TOKEN_CACHE, &oauth.token_cache),
        (TOKEN_CACHE_V2, &oauth.token_cache_v2),
        (ACCOUNT_UUID, &oauth.account_uuid),
    ] {
        match value {
            Some(v) => doc.set(key, v.clone()),
            // Removal, not "leave it": leaving it would keep the previous
            // account's token cache under the new account's session.
            None => doc.remove(key),
        }
    }

    doc.save(config_path, backup_dir)
}

/// Remove all three, leaving the app signed out.
pub fn clear(config_path: &Path, backup_dir: &Path) -> Result<()> {
    apply(config_path, &DesktopOauth::default(), backup_dir)
}
