//! Switching the desktop app's session, alongside Claude Code's.
//!
//! Called AFTER Claude Code has already switched. Nothing here may undo
//! that: the CLI switch is the fast, safe, always-available half, and it
//! stays committed whatever happens to the desktop half (design decision 2).

use std::path::Path;

use crate::claude::detect::ProcessProbe;
use crate::desktop::journal::Journal;
use crate::desktop::paths::DesktopPaths;
use crate::desktop::{config, swap};
use crate::error::{Error, Result};
use crate::paths::HostPaths;
use crate::store::metadata::{AccountsFile, DesktopProfileRecord, now_rfc3339};

/// What the desktop half of a switch managed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopOutcome {
    /// Profile parked and the incoming one installed.
    Switched,
    /// The app was running; nothing was touched.
    AppRunning,
    /// Outgoing profile parked, but the incoming account has none stored, so
    /// the app is now signed out and waiting for a login to capture.
    NoProfileForIncoming,
    /// No outgoing account and nothing stored: there was nothing to move.
    NothingToDo,
}

/// Total size of a directory tree, for the stored-profile record.
///
/// Best-effort: an unreadable entry contributes zero rather than failing the
/// switch. This number is for display, and a switch that already committed
/// must never be turned into an `Err` by a reporting detail.
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

pub fn switch_desktop<P: HostPaths, D: DesktopPaths, R: ProcessProbe>(
    paths: &P,
    desktop: &D,
    probe: &R,
    outgoing: Option<&str>,
    incoming: &str,
) -> Result<DesktopOutcome> {
    // Guard: without this, a self-switch parks the live profile into the
    // very directory it is about to install from, and clears the OAuth keys
    // of the account the user is staying on. A first-time self-switch
    // silently loses their OAuth keys (signing out); a repeat self-switch
    // hits a DirectoryNotEmpty rename collision, self-healing through
    // recovery machinery but still surfacing an Err to the caller.
    if outgoing == Some(incoming) {
        return Ok(DesktopOutcome::NothingToDo);
    }

    // Chromium corrupts profile state if its directories move underneath a
    // running process, so this is a hard gate, not a warning.
    if probe.desktop_app_running() {
        return Ok(DesktopOutcome::AppRunning);
    }

    let live = desktop.desktop_dir();
    let park_to = outgoing.map(|uuid| paths.desktop_profile_dir(uuid));
    let install_from = paths.desktop_profile_dir(incoming);
    let has_incoming = install_from.is_dir();

    // The outgoing account's OAuth keys are captured and written into its
    // parked directory BEFORE any rename runs. Order is load-bearing: once
    // the swap starts, `config.json` is about to be overwritten with the
    // incoming account's values, so capturing afterwards would read back
    // the wrong account -- and a failure mid-swap would lose the outgoing
    // account's identity entirely, leaving its parked cookies unusable.
    if let Some(park_to) = park_to.as_deref() {
        let outgoing_oauth = config::capture(&desktop.config_file())?;
        std::fs::create_dir_all(park_to).map_err(|source| Error::Io {
            path: park_to.to_path_buf(),
            source,
        })?;
        let bytes = serde_json::to_vec_pretty(&outgoing_oauth).map_err(|source| Error::Parse {
            path: park_to.join("oauth.json"),
            source,
        })?;
        crate::atomic::write(&park_to.join("oauth.json"), &bytes)?;
    }

    let mut journal = Journal::plan(
        &live,
        park_to.as_deref(),
        has_incoming.then_some(install_from.as_path()),
    )?;
    // `Journal::plan` only knows paths, not account identities, so it always
    // hands back `outgoing`/`incoming` as `None` (task-3 carry-forward).
    // Filled in here, before the journal is ever persisted, so that a
    // recovery hitting this journal mid-swap can report which two accounts
    // it was between. Mirrors the Option-ness already decided above rather
    // than guessing at new semantics: `None` means this swap genuinely has
    // no such side (no outgoing account at all; no stored profile to
    // install), matching `park_to`/`install_from`.
    journal.outgoing = outgoing.map(str::to_string);
    journal.incoming = has_incoming.then(|| incoming.to_string());

    if journal.moves.is_empty() && !has_incoming && outgoing.is_none() {
        return Ok(DesktopOutcome::NothingToDo);
    }

    swap::execute(paths, journal)?;

    // The desktop_profile record (task 7) is what lets `byte list` answer
    // "which accounts have a stored desktop session, and what is it costing
    // me in disk" -- stamped only once a park actually ran, and only when
    // accounts.json already knows this account. An unrecognised uuid (no
    // entry to land the record on) silently skips the stamp rather than
    // failing a switch whose files have already moved on disk.
    if let (Some(uuid), Some(park_to)) = (outgoing, park_to.as_deref()) {
        let mut accounts = AccountsFile::load(&paths.accounts_file())?;
        if let Ok(meta) = accounts.resolve_mut(uuid) {
            meta.desktop_profile = Some(DesktopProfileRecord {
                captured_at: now_rfc3339(),
                bytes: dir_size(park_to),
            });
            accounts.save(&paths.accounts_file(), &paths.backup_dir())?;
        }
    }

    // The config patch follows the moves, not the other way round: if the
    // moves fail, the app's identity should still name whatever session is
    // actually in place.
    let backups = paths.backup_dir();
    if has_incoming {
        let stored = install_from.join("oauth.json");
        let oauth = match std::fs::read(&stored) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => config::DesktopOauth::default(),
        };
        config::apply(&desktop.config_file(), &oauth, &backups)?;
        Ok(DesktopOutcome::Switched)
    } else {
        // Signed out: the user logs in as the new account and byte captures.
        config::clear(&desktop.config_file(), &backups)?;
        Ok(DesktopOutcome::NoProfileForIncoming)
    }
}
