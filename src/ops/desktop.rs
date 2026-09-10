//! Switching the desktop app's session, alongside Claude Code's.
//!
//! Called AFTER Claude Code has already switched. Nothing here may undo
//! that: the CLI switch is the fast, safe, always-available half, and it
//! stays committed whatever happens to the desktop half (design decision 2).

use std::path::Path;

use crate::claude::detect::ProcessProbe;
use crate::desktop::journal::Journal;
use crate::desktop::paths::DesktopPaths;
use crate::desktop::{config, profile, swap};
use crate::error::{Error, Result};
use crate::output;
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
    /// The live desktop session belongs to a different account than the one
    /// byte was about to file it under; nothing was touched. See the guard
    /// in [`switch_desktop`] for how the two halves drift apart.
    IdentityMismatch,
    /// No outgoing account and nothing stored: there was nothing to move.
    NothingToDo,
}

/// Does the live desktop session belong to `expected`?
///
/// An ABSENT `lastKnownAccountUuid` is not a mismatch: there is no session
/// to misfile, and parking a signed-out profile under the outgoing account
/// is harmless -- that is the ordinary state of a desktop app the user has
/// never signed into, and refusing there would break the first switch on
/// every fresh machine. An explicit JSON `null` is read the same way, since
/// it carries no identity either.
///
/// A value that is present but not a string cannot equal `expected` and so
/// is a mismatch: byte would be about to file a session whose identity it
/// cannot even read.
fn identity_matches(live: &config::DesktopOauth, expected: &str) -> bool {
    match live.account_uuid.as_ref() {
        None | Some(serde_json::Value::Null) => true,
        Some(v) => v.as_str() == Some(expected),
    }
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
    // Refuse an unrepaired swap HERE, before anything is written, rather
    // than leaving it to `swap::execute`'s own identical precondition.
    // `execute` refuses too late: by the time it runs, the capture block
    // below has already overwritten the outgoing account's parked
    // `oauth.json` -- and that file is not in any journal, so no recovery
    // ever repairs it. Concretely: `switch a -> b` is interrupted, the
    // journal survives with `config.json` still holding a's keys, and the
    // next switch names the now-active account as outgoing
    // (`switch b -> c`). The capture reads a's keys and files them as b's
    // parked oauth, destroying b's genuine copy; `execute` then refuses,
    // and a later `switch x -> b` applies a's identity onto b's cookies.
    // A refusal has to cost nothing, so it comes before every branch.
    //
    // Only the check lives here, never the repair: `recover_if_interrupted`
    // is wired into every command entry point, and running recovery from
    // two places would be worse than running it from one.
    let journal_file = paths.desktop_journal_file();
    let journal_present = journal_file.try_exists().map_err(|source| Error::Io {
        path: journal_file.clone(),
        source,
    })?;
    if journal_present {
        return Err(Error::DesktopSwapInterrupted {
            journal: journal_file,
            detail: "An earlier swap has not been repaired yet, so byte will not start a new \
                     one over it -- that would destroy the only record of the old swap while \
                     its files may already be half-moved. Running any byte command repairs \
                     it: recovery runs automatically at the start of every command. Do that, \
                     then retry."
                .to_string(),
        });
    }

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

    // The outgoing account's OAuth keys are read BEFORE any rename runs.
    // Order is load-bearing: once the swap starts, `config.json` is about to
    // be overwritten with the incoming account's values, so capturing
    // afterwards would read back the wrong account -- and a failure mid-swap
    // would lose the outgoing account's identity entirely, leaving its
    // parked cookies unusable. Read once, used twice: by the identity guard
    // immediately below, and by the park that writes it out further down.
    let outgoing_oauth = match outgoing {
        Some(_) => Some(config::capture(&desktop.config_file())?),
        None => None,
    };

    // Refuse to file one account's live session under another account's
    // uuid. `outgoing` comes from the CLI's sync-back -- it names whichever
    // account Claude CODE was on -- and the two halves can drift apart: the
    // tray switches Claude Code without touching the desktop app at all, and
    // a user can sign into Claude Desktop by hand at any time. When they
    // have drifted, parking would write the live account's cookies into
    // `<store>/<other-uuid>/` and its keys into that directory's
    // `oauth.json`, so a later switch INTO that other account would install
    // this account's session and apply its identity. Nothing on disk changes
    // here: a refusal costs nothing and loses nothing.
    if let (Some(uuid), Some(oauth)) = (outgoing, outgoing_oauth.as_ref())
        && !identity_matches(oauth, uuid)
    {
        return Ok(DesktopOutcome::IdentityMismatch);
    }

    // "Is there a stored profile?" is decided on whether the directory holds
    // anything that MOVES, not on the directory existing -- because this very
    // function creates that directory and writes `oauth.json` into it even
    // when a park had nothing movable to file (a fresh install, or a
    // directory of purely denylisted content). Believing `is_dir()` there
    // means believing a claim byte's own write manufactured: the swap would
    // install nothing, apply an all-`None` `DesktopOauth` (which
    // `config::apply` treats as removal -- exactly what `clear` does), and
    // still report `Switched`, telling the user their desktop session was
    // restored while signing them out. Errors propagate: this runs before
    // anything has been committed, so refusing is still free here.
    let has_incoming = !profile::movable_entries(&install_from)?.is_empty();

    // The keys read above are written into the parked directory before the
    // first rename, for the ordering reason given at that read.
    if let (Some(park_to), Some(outgoing_oauth)) = (park_to.as_deref(), outgoing_oauth.as_ref()) {
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

    // The config patch follows the moves, not the other way round: if the
    // moves fail, the app's identity should still name whatever session is
    // actually in place.
    let backups = paths.backup_dir();
    let outcome = if has_incoming {
        let stored = install_from.join("oauth.json");
        let oauth = match std::fs::read(&stored) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => config::DesktopOauth::default(),
        };
        config::apply(&desktop.config_file(), &oauth, &backups)?;
        DesktopOutcome::Switched
    } else {
        // Signed out: the user logs in as the new account and byte captures.
        config::clear(&desktop.config_file(), &backups)?;
        DesktopOutcome::NoProfileForIncoming
    };

    // Bookkeeping comes LAST, after both halves of the switch have landed,
    // because it is the only step whose failure must not be allowed to
    // matter. See `record_parked_profile`.
    if let (Some(uuid), Some(park_to)) = (outgoing, park_to.as_deref()) {
        record_parked_profile(paths, uuid, park_to);
    }

    Ok(outcome)
}

/// Stamp the `desktop_profile` record for a profile that has just been parked.
///
/// This record (task 7) is what lets `byte list` answer "which accounts have
/// a stored desktop session, and what is it costing me in disk". Nothing else
/// reads it, and nothing about the switch depends on it.
///
/// Infallible by contract, following `atomic::prune`: by the time this runs
/// the renames are committed, the journal is cleared, and `config.json`
/// already names the incoming account, so there is nothing left for a caller
/// to retry or roll back. Both halves fail on entirely ordinary conditions,
/// not just crashes -- `load` refuses an outdated schema or malformed JSON,
/// and `save` copies a backup first, which is where this codebase already
/// sees "Access is denied (os error 5)". Propagating either would turn a
/// fully committed switch into an `Err` over a display detail, and, worse,
/// would have to do so from *before* the config patch to be reached at all:
/// `config.json` would be left holding the outgoing account's
/// `lastKnownAccountUuid` and `oauth:tokenCache` over the incoming account's
/// live directory, with no journal left for recovery to find. That silent
/// mixed-account state is precisely what `desktop::journal` exists to
/// prevent. Problems are reported through `output::warn` instead.
fn record_parked_profile(paths: &impl HostPaths, uuid: &str, park_to: &Path) {
    let file = paths.accounts_file();

    let Ok(mut accounts) = AccountsFile::load(&file) else {
        output::warn(&format!(
            "the desktop session was switched, but {} could not be read, so the stored \
             desktop profile for '{uuid}' will not be listed",
            file.display()
        ));
        return;
    };

    // An unrecognised uuid has no entry to land the record on. Silent, not
    // warned: byte can legitimately park a profile for an account whose
    // metadata it does not track.
    let Ok(meta) = accounts.resolve_mut(uuid) else {
        return;
    };
    meta.desktop_profile = Some(DesktopProfileRecord {
        captured_at: now_rfc3339(),
        bytes: dir_size(park_to),
    });

    if let Err(e) = accounts.save(&file, &paths.backup_dir()) {
        output::warn(&format!(
            "the desktop session was switched, but the stored desktop profile for '{uuid}' \
             could not be recorded in {}: {e}",
            file.display()
        ));
    }
}
