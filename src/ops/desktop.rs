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
    /// byte was about to file it under; nothing was touched. See
    /// [`park_target`] for how the two halves drift apart.
    IdentityMismatch,
    /// The incoming profile was installed, but the account's stored
    /// `oauth.json` could not be read, so the app was left signed out rather
    /// than authenticating as the account whose session just moved away.
    SwitchedWithoutIdentity,
    /// No outgoing account and nothing stored: there was nothing to move.
    NothingToDo,
    /// `incoming` cannot safely name a directory inside byte's profile store
    /// (see [`crate::paths::is_profile_store_component`]), so nothing was
    /// installed and nothing else was touched either. See `switch_desktop`
    /// for where this is checked and why it must not be read as "no stored
    /// profile".
    IncomingIdentifierInvalid,
}

/// Which account's store the live profile should be filed under.
enum ParkTarget {
    /// File it under this uuid.
    Under(String),
    /// There is no session to file: nothing in the live directory moves.
    Nothing,
    /// byte cannot tell whose session this is, or it is not the one it was
    /// told to expect. Touch nothing.
    Refuse,
}

/// Decide whose store the live desktop profile belongs in, from what
/// `config.json` actually says rather than from `outgoing` alone.
///
/// `outgoing` comes from the CLI's sync-back -- it names whichever account
/// Claude CODE was on -- and the two halves drift apart routinely: the tray
/// switches Claude Code without touching the desktop app, and a user can
/// sign into Claude Desktop by hand at any time. Filing the wrong way round
/// writes one account's cookies into another account's directory, so a later
/// switch INTO that other account installs this account's session and
/// applies its identity. Nothing on disk changes on a refusal: it costs
/// nothing and loses nothing.
///
/// Three cases are worth spelling out, because each was a finding.
///
/// `outgoing` is `None` -- Claude Code is logged out, or freshly installed
/// -- but `config.json` NAMES an account. Reading that as "no session to
/// park" plans no park at all, leaves the live directory's contents in
/// place, and lands the incoming account's install moves on top of them:
/// where the entry sets overlap the rename fails with a bare io error, and
/// where they are disjoint it SUCCEEDS and stamps the incoming account's
/// identity over a directory still holding the previous account's cookies.
/// The app itself knows whose session it is; byte parks it under that uuid.
///
/// `outgoing` is `None` and `config.json` names NOBODY, over a live
/// directory that nonetheless holds a session. Same two failures, and
/// nothing to file the session under, so the only safe answer is to refuse.
/// byte manufactures this precondition itself: the `Absent`/`Unreadable`
/// install arms below call `config::clear`, which removes
/// `lastKnownAccountUuid` while the live directory holds the account's real
/// cookies -- and a tray click then switches Claude Code without touching
/// the desktop app at all. `Nothing` survives only for a live directory with
/// nothing movable in it, which is the ordinary state of a machine that has
/// never signed in and must keep working.
///
/// An ABSENT `lastKnownAccountUuid` alongside an outgoing account used to be
/// waved through unconditionally, on the premise that a signed-out profile
/// is harmless to file under it. That premise holds only while
/// `<store>/<outgoing>` is empty -- and the same `config::clear` violates
/// it. Over a store that already holds a session, an absent uuid is a drift
/// signal, not a green light.
fn park_target(
    paths: &impl HostPaths,
    live_dir: &Path,
    outgoing: Option<&str>,
    live: &config::DesktopOauth,
) -> Result<ParkTarget> {
    let identity = live.identity();

    let Some(expected) = outgoing else {
        return Ok(match identity {
            config::Identity::Account(uuid) => park_under(uuid),
            config::Identity::Unreadable => ParkTarget::Refuse,
            // Neither half names an account. With nothing to file a session
            // under, "there is no session" has to be established from the
            // live directory rather than assumed.
            config::Identity::Absent => {
                if profile::movable_entries(live_dir)?.is_empty() {
                    ParkTarget::Nothing
                } else {
                    ParkTarget::Refuse
                }
            }
        });
    };

    Ok(match identity {
        config::Identity::Account(uuid) if uuid == expected => park_under(uuid),
        config::Identity::Account(_) | config::Identity::Unreadable => ParkTarget::Refuse,
        // No identity: a desktop app that has never been signed in, or one
        // byte itself signed out. Only the first is safe to file, and an
        // empty store is what tells them apart. Refusing whenever the store
        // is empty instead would break the first switch on every fresh
        // machine.
        config::Identity::Absent => {
            let stored = profile::movable_entries(&paths.desktop_profile_dir(expected))?;
            if stored.is_empty() {
                park_under(expected.to_string())
            } else {
                ParkTarget::Refuse
            }
        }
    })
}

/// `Under(uuid)`, unless that string cannot safely name a directory.
///
/// Every `Under` in [`park_target`] goes through here rather than only the
/// one reached with `outgoing: None`, even though that is the arm where the
/// value provably comes from another application's `config.json`: the same
/// string ends up in `HostPaths::desktop_profile_dir`, in the journal's
/// recorded `to` paths, and in the `desktop_profile` record, and there is no
/// arm where letting it steer a path would be correct. See
/// [`crate::paths::is_profile_store_component`] for what an unchecked value
/// does. `Refuse` is already the answer for "byte cannot tell whose session
/// this is", and an identifier it cannot use is a case of exactly that.
fn park_under(uuid: String) -> ParkTarget {
    if crate::paths::is_profile_store_component(&uuid) {
        ParkTarget::Under(uuid)
    } else {
        ParkTarget::Refuse
    }
}

/// Does an account's store already hold a desktop session?
///
/// Either half counts. A park files two things side by side -- the profile
/// directories, and the `oauth.json` byte captured from `config.json` -- and
/// a park can produce one without the other: a live directory whose every
/// entry is denylisted stores real keys and no directories at all. Losing
/// either loses the account's desktop session, so the question is not "are
/// there directories" but "is there anything of that account here".
///
/// An `oauth.json` byte cannot read counts as a session: the point of the
/// caller's guard is to refuse when byte cannot tell, and treating an
/// unreadable file as empty would let the one write that destroys it
/// through.
fn store_holds_a_session(dir: &Path) -> Result<bool> {
    if !profile::movable_entries(dir)?.is_empty() {
        return Ok(true);
    }
    Ok(match config::read_stored(&dir.join("oauth.json")) {
        config::StoredOauth::Absent => false,
        config::StoredOauth::Unreadable(_) => true,
        config::StoredOauth::Loaded(oauth) => !oauth.is_empty(),
    })
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
    // Owner-only (where the platform allows it) and created before anything
    // else in this function writes into it -- see `create_store_dir`'s own
    // doc comment for exactly what protection this does and does not
    // provide on Windows (design §6). Idempotent, so paying this cost on
    // every call, including the early-return branches below, is cheap and
    // simpler than threading a second call site through them.
    swap::create_store_dir(paths)?;

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

    // The live OAuth keys are read BEFORE any rename runs, and
    // UNCONDITIONALLY -- not only when Claude Code names an outgoing
    // account. Order is load-bearing: once the swap starts, `config.json` is
    // about to be overwritten with the incoming account's values, so
    // capturing afterwards would read back the wrong account, and a failure
    // mid-swap would lose the outgoing account's identity entirely, leaving
    // its parked cookies unusable. Reading it unconditionally is what lets
    // `park_target` answer from the app's own record of whose session this
    // is, rather than from a parameter that describes the OTHER half of the
    // switch. Read once, used three times: to decide the park target, to
    // guard the write below, and as the content of that write.
    let live_oauth = config::capture(&desktop.config_file())?;

    let park_under = match park_target(paths, &live, outgoing, &live_oauth)? {
        ParkTarget::Refuse => return Ok(DesktopOutcome::IdentityMismatch),
        ParkTarget::Nothing => None,
        ParkTarget::Under(uuid) => Some(uuid),
    };

    // The self-switch guard again, now that the outgoing side is known from
    // the app rather than from Claude Code. Claude Code being logged out
    // while the desktop app is already signed in as the incoming account is
    // an ordinary state, and there is nothing to do for it: parking into the
    // directory the swap would install from is exactly the collision the
    // guard at the top of this function exists to prevent.
    if park_under.as_deref() == Some(incoming) {
        return Ok(DesktopOutcome::NothingToDo);
    }

    // `incoming` gets the same check `park_under` already gives the outgoing
    // side, and for the identical reason: it is `switch_desktop`'s own
    // parameter, but it did not originate with byte. It is
    // `AccountSnapshot::identity()` -- `.claude.json`'s own `accountUuid`,
    // read by `store::metadata::AccountsFile::upsert_from` into
    // `accounts.json` without any check on its shape, only on its presence
    // -- so a malformed value there reaches here unchanged.
    //
    // Unlike the outgoing side, an unchecked value here does not merely
    // escape the store: `desktop_store_dir().join("..")` resolves to byte's
    // OWN config directory, whose entries (`accounts.json`, `backups`, the
    // store itself) are all real, all absent from `profile`'s denylist, and
    // so all `Move`. `movable_entries` below would find them, `has_incoming`
    // would be `true` for a reason that has nothing to do with any account's
    // stored profile, and the plan a few lines down would journal moving
    // byte's own account database into `%APPDATA%\Claude`.
    //
    // Refusing is the only correct response, not treating this as "no
    // stored profile" (`has_incoming = false`): every name that fails this
    // check is one `Path::join` cannot treat as an ordinary component at
    // all, so silently downgrading the refusal to a quieter outcome would
    // hide a `.claude.json` data problem behind a message that looks
    // identical to the ordinary "nothing captured yet" case, on every
    // subsequent switch, with no signal that anything needs fixing.
    if !crate::paths::is_profile_store_component(incoming) {
        return Ok(DesktopOutcome::IncomingIdentifierInvalid);
    }

    let park_to = park_under
        .as_deref()
        .map(|uuid| paths.desktop_profile_dir(uuid));
    let install_from = paths.desktop_profile_dir(incoming);

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
    if let Some(park_to) = park_to.as_deref() {
        // The last guard before an irreplaceable file is overwritten, and a
        // different question from `park_target`'s. That one asks whose store
        // this profile belongs in, and answers from the parked DIRECTORIES.
        // This one asks whether the write is about to destroy a session, and
        // answers from the parked KEYS -- which a park can legitimately
        // store without any directories at all. `oauth.json` is in no
        // journal, so no recovery ever repairs it: replacing real keys with
        // `{null,null,null}` simply loses that account's desktop token. An
        // empty capture over a store that still holds a session means the
        // live directory is not the session byte thinks it is.
        if live_oauth.is_empty() && store_holds_a_session(park_to)? {
            return Ok(DesktopOutcome::IdentityMismatch);
        }

        std::fs::create_dir_all(park_to).map_err(|source| Error::Io {
            path: park_to.to_path_buf(),
            source,
        })?;
        let bytes = serde_json::to_vec_pretty(&live_oauth).map_err(|source| Error::Parse {
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
    // hands back `outgoing`/`incoming` as `None`. Filled in here, before the
    // journal is ever persisted, because they are what a later recovery uses
    // to finish the identity half of this swap: which account's parked
    // `oauth.json` belongs in `config.json` depends entirely on which way
    // the repair goes. `None` means this swap genuinely has no such side (no
    // session to park; no stored profile to install), matching
    // `park_to`/`install_from` -- and `outgoing` is the DERIVED park target,
    // not the parameter, because that is whose session actually moved.
    journal.outgoing = park_under.clone();
    journal.incoming = has_incoming.then(|| incoming.to_string());

    if journal.moves.is_empty() && !has_incoming && park_under.is_none() {
        return Ok(DesktopOutcome::NothingToDo);
    }

    swap::execute(paths, journal)?;

    // The config patch follows the moves, not the other way round: if the
    // moves fail, the app's identity should still name whatever session is
    // actually in place. It runs INSIDE the journal's commit boundary --
    // `execute` no longer clears the journal, and `clear_journal` below runs
    // only once this patch has landed. A crash or an `Err` anywhere in here
    // therefore leaves the journal on disk, and the next byte command
    // finishes the patch through `swap::recover_if_interrupted` instead of
    // leaving the app authenticating as one account over another's cookies.
    let backups = paths.backup_dir();
    let config_file = desktop.config_file();
    let outcome = if has_incoming {
        let stored = install_from.join("oauth.json");
        match config::read_stored(&stored) {
            config::StoredOauth::Loaded(oauth) => {
                config::apply(&config_file, &oauth, &backups)?;
                DesktopOutcome::Switched
            }
            // No file at all: a profile parked by a build older than the one
            // that started writing `oauth.json`. Its cookies still restore;
            // it simply has no identity recorded, so the app opens signed
            // out and byte captures the next login.
            config::StoredOauth::Absent => {
                config::clear(&config_file, &backups)?;
                DesktopOutcome::Switched
            }
            // Present and unreadable. Defaulting silently -- which is what
            // `unwrap_or_default` did -- installs the account's cookies and
            // then signs the app out of them, while reporting a clean
            // switch. The swap has committed, so this must not become an
            // `Err`; it becomes an outcome of its own, and a warning naming
            // the file, so the stderr message and `--json` both say what
            // actually happened.
            config::StoredOauth::Unreadable(reason) => {
                config::clear(&config_file, &backups)?;
                output::warn(&format!(
                    "the desktop session for this account was restored, but its saved sign-in \
                     at {} could not be read ({reason}), so Claude Desktop will open signed \
                     out. Sign in there once and byte will capture it again.",
                    stored.display()
                ));
                DesktopOutcome::SwitchedWithoutIdentity
            }
        }
    } else {
        // Signed out: the user logs in as the new account and byte captures.
        config::clear(&config_file, &backups)?;
        DesktopOutcome::NoProfileForIncoming
    };

    // Both halves have landed, so the swap is over and its record goes.
    swap::clear_journal(paths)?;

    // Bookkeeping comes LAST, after both halves of the switch have landed,
    // because it is the only step whose failure must not be allowed to
    // matter. See `record_parked_profile`.
    if let (Some(uuid), Some(park_to)) = (park_under.as_deref(), park_to.as_deref()) {
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
