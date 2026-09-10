//! What in the desktop app's directory belongs to an account.
//!
//! Pure: operates on entry names, so it compiles and is tested on every
//! platform even though the feature is Windows-only.
//!
//! This is a DENYLIST, and that direction is deliberate (design §3). An
//! allowlist would silently strand any session store byte has not heard of
//! -- including one a future app version adds -- producing a half-signed-in
//! profile that looks like a byte bug. Carrying an unknown directory costs
//! disk; leaving one behind costs a broken switch.

use std::path::Path;

use crate::error::{Error, Result};

/// What byte does with one entry in the desktop directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Account state: parked and restored with the account.
    Move,
    /// Mixed: byte owns some keys inside it and patches them in place.
    Patch,
    /// Shared, disposable, or already account-keyed.
    Leave,
}

/// Entries that are junk, shared, or self-segregating. Lowercased.
const LEAVE: &[&str] = &[
    // Throwaway caches -- 690 MB of the measured 1.28 GB.
    "cache",
    "code cache",
    "gpucache",
    "dawngraphitecache",
    "dawnwebgpucache",
    "blob_storage",
    "shared_proto_db",
    "videodecodestats",
    // Diagnostics.
    "logs",
    "crashpad",
    "sentry",
    "lockfile",
    // 416 MB of Claude Code program binaries -- NOT account state.
    "claude-code",
    // Already keyed by account uuid; see the module doc of `swap`.
    "partitions",
    // Holds the DPAPI key that decrypts cookies. Shared, or every parked
    // profile becomes undecryptable at once.
    "local state",
    // The user's own configuration and app preferences.
    "claude_desktop_config.json",
    "preferences",
    "window-state.json",
    "bridge-state.json",
    "ant-device-registry.json",
    "ant-did",
    // 68 bytes, one key `tokens-today`: an LLM usage counter.
    "buddy-tokens.json",
];

/// Decide what happens to one entry.
pub fn classify(entry_name: &str) -> Disposition {
    let name = entry_name.to_ascii_lowercase();

    if name == "config.json" {
        return Disposition::Patch;
    }
    // byte's own captured-OAuth file (`desktop::config`/`ops::desktop`),
    // written directly inside a profile-store directory alongside the
    // entries that DO move with the account. It must stay exactly where
    // classify's caller finds it: on the install side, `ops::desktop` reads
    // it back from that same directory right after the swap completes, and
    // moving it into the live app directory would both litter that
    // directory with a file Claude Desktop has never heard of and delete
    // the only copy byte has left to restore from -- silently replacing a
    // real restore with defaults. Not part of `LEAVE` below, whose own doc
    // comment ("junk, shared, or self-segregating") does not describe this
    // file any better than it describes `config.json` above: both are
    // single, deliberate exceptions, not entries in a general list.
    if name == "oauth.json" {
        return Disposition::Leave;
    }
    if LEAVE.contains(&name.as_str()) {
        return Disposition::Leave;
    }
    Disposition::Move
}

/// Names of the entries in `dir` that move, in directory order.
///
/// A missing directory yields an empty list rather than an error: capturing
/// from a fresh install, or installing into a location that does not exist
/// yet, are both ordinary.
pub fn movable_entries(dir: &Path) -> Result<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(Error::Io {
                path: dir.to_path_buf(),
                source,
            });
        }
    };

    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| Error::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if classify(&name) == Disposition::Move {
            out.push(name);
        }
    }
    Ok(out)
}
