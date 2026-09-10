//! Executing a journal against the filesystem.
//!
//! `Partitions` is not moved by any of this, deliberately: its
//! account-scoped entry is already named `cowork-artifact-<accountUuid>-...`
//! and so self-segregates -- distinct accounts get distinct directories, and
//! switching back finds the previous one intact.

use std::path::{Path, PathBuf};

use crate::desktop::config;
use crate::desktop::journal::{Journal, Stage};
use crate::desktop::paths::DesktopPaths;
use crate::error::{Error, Result};
use crate::paths::HostPaths;

/// Create the profile store with owner-only access.
///
/// This is a partial measure on Windows and the plan says so rather than
/// implying otherwise. A real per-directory ACL needs the `windows` crate,
/// which this change deliberately does not add; instead the store is created
/// under byte's config directory, which lives beneath `%APPDATA%` and
/// inherits that location's user-scoped ACL. That is the same protection
/// `accounts.json` already relies on -- but `accounts.json` holds metadata,
/// and this holds cookies, so the weaker guarantee is worth naming.
pub fn create_store_dir(paths: &impl HostPaths) -> Result<()> {
    let dir = paths.desktop_store_dir();
    std::fs::create_dir_all(&dir).map_err(|source| Error::Io {
        path: dir.clone(),
        source,
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(&dir, perms).map_err(|source| Error::Io {
            path: dir.clone(),
            source,
        })?;
    }

    Ok(())
}

/// What to do with an interrupted swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Finish the install: the incoming profile is already partly in place.
    RollForward,
    /// Undo the parks: nothing of the incoming profile has landed yet.
    Reverse,
}

/// Decide by stage and by completeness, never by a count.
///
/// One completed `Install` is enough to commit to rolling forward: the
/// incoming profile is already partly live, and reversing would have to
/// unpick it while the outgoing profile is only half parked.
///
/// A journal whose every move is done rolls forward too, whatever its
/// stages, because there is nothing left to reverse -- the rename half of
/// the swap finished. This is not a count: `is_complete` asks whether any
/// move is outstanding, and one whose rename landed without its `done`
/// persisting is correctly still outstanding here (`apply_move` is what
/// reconciles that, from the disk rather than the flag).
///
/// The case it exists for is a swap with NO installs at all -- switching
/// into an account with nothing stored, which parks and then signs the app
/// out. Such a journal can never satisfy the install test above, so without
/// this it would reverse: undoing a park that fully completed and putting
/// the outgoing account's session back, i.e. quietly undoing a switch the
/// user asked for and that had already happened on disk. That window is
/// real, not theoretical, because the journal now also covers the
/// `config.json` patch, and that patch fails on ordinary conditions.
pub fn recovery_for(journal: &Journal) -> Recovery {
    let any_install_done = journal
        .moves
        .iter()
        .any(|m| m.stage == Stage::Install && m.done);

    if any_install_done || journal.is_complete() {
        Recovery::RollForward
    } else {
        Recovery::Reverse
    }
}

fn rename(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::rename(from, to).map_err(|source| Error::Io {
        path: from.to_path_buf(),
        source,
    })
}

/// `Journal::to_bytes`/`from_bytes` operate on in-memory bytes and have no
/// filesystem path of their own, so a serde failure there names the
/// placeholder `<journal>` (see `journal.rs`). Every caller in this file
/// knows the real path, so rewrite the placeholder into it before the error
/// goes any further -- carried forward from the Task 3 review.
fn attach_journal_path<T>(result: Result<T>, path: &Path) -> Result<T> {
    result.map_err(|err| match err {
        Error::Parse { source, .. } => Error::Parse {
            path: path.to_path_buf(),
            source,
        },
        other => other,
    })
}

fn write_journal(paths: &impl HostPaths, journal: &Journal) -> Result<()> {
    let file = paths.desktop_journal_file();
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    // Written through `atomic::write` so a torn journal can never be the
    // thing that makes a swap unrecoverable.
    let bytes = attach_journal_path(journal.to_bytes(), &file)?;
    crate::atomic::write(&file, &bytes)
}

/// Delete the journal, ending the swap it describes.
///
/// `pub` because the swap does not end at the last rename: `config.json`'s
/// account keys are part of the same commit, and the caller that patches
/// them is the only one that knows when both halves are done. `execute`
/// deliberately does NOT call this -- see its own doc comment.
pub fn clear_journal(paths: &impl HostPaths) -> Result<()> {
    let file = paths.desktop_journal_file();
    match std::fs::remove_file(&file) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Io { path: file, source }),
    }
}

/// Which way a move is currently being applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// `from` -> `to`; mark the move done on success.
    Forward,
    /// `to` -> `from`; mark the move NOT done on success.
    Backward,
}

/// Does this path exist, or why could that not be determined?
///
/// `Path::exists` collapses every IO error into `false`, so a permission
/// error on byte's config directory would read as "absent" -- and each
/// caller below chooses a branch on that answer, one of which is "the
/// journal is not there, nothing to recover". `try_exists` keeps a real IO
/// failure a failure, so the guard says what it means.
fn exists(path: &Path) -> Result<bool> {
    path.try_exists().map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Apply one move in the given direction, then flip `done` and persist.
///
/// `execute`, RollForward, and Reverse recovery all follow the same shape:
/// rename, flip `done`, persist -- and a process killed between the rename
/// succeeding and that persist landing leaves the directory already moved
/// on disk while the journal still disagrees. Left alone, the next attempt
/// to apply this same move re-runs the identical rename and finds its
/// source already gone.
///
/// So the physical source/destination pair for this move and direction is
/// classified before renaming. Which states are reachable depends on the
/// direction, and so does what each one means:
///
/// - source occupied, destination empty -> the ordinary case; rename.
/// - source empty, destination occupied -> an earlier crash landed in this
///   exact window: the rename already happened and only the journal write
///   recording it did not. Treat it as a no-op success rather than failing
///   on a source that predictably no longer exists.
/// - both occupied, undoing (`Backward`) -> leave the disk alone and leave
///   `done` as it is. Two situations produce this state. The first is that
///   the move never ran, which is far from exotic: it is the resting state
///   of every install move throughout the whole park stage, where the
///   incoming profile is still in its store and the live directory it
///   would replace has not been parked yet. The second is that a *later*
///   move re-occupied this move's destination -- a park of name N has
///   destination `live/N`, which the install of the same name re-fills, so
///   a park can be both-occupied even with `done: true`.
///   That second case is safe ONLY because `Journal::plan` emits every
///   park before every install and `recover_if_interrupted` walks in
///   reverse, so any later move that re-occupied this destination has
///   already been undone by the time this one is visited. The walk order
///   is the guarantee -- not, as an earlier version of this comment
///   claimed, that a swap can never re-occupy a destination. Do not
///   reorder or parallelise the reverse loop: doing so reintroduces the
///   stranded-directory bug this arm exists to prevent.
///   Renaming here would instead collide with the occupied destination,
///   and the resulting error escaping recovery would strand the journal on
///   disk for every later byte command to trip over -- which is exactly
///   what a previous version of this function did.
/// - both occupied, applying (`Forward`) -> genuinely wrong: something
///   stale is sitting where this move must land. Falls through to the
///   rename, which fails with the OS's own error. Not papered over.
/// - neither occupied -> genuinely wrong in either direction; falls through
///   to the rename, which fails with the OS's own error naming the missing
///   source. Not papered over either.
fn apply_move(
    paths: &impl HostPaths,
    journal: &mut Journal,
    index: usize,
    direction: Direction,
) -> Result<()> {
    let (source, dest) = {
        let m = &journal.moves[index];
        match direction {
            Direction::Forward => (m.from.clone(), m.to.clone()),
            Direction::Backward => (m.to.clone(), m.from.clone()),
        }
    };

    let source_here = exists(&source)?;
    let dest_here = exists(&dest)?;

    // A move that never ran has nothing to undo, and its `done` already
    // says so. Touch neither the filesystem nor the journal.
    if direction == Direction::Backward && source_here && dest_here {
        return Ok(());
    }

    let already_applied = !source_here && dest_here;
    if !already_applied {
        rename(&source, &dest)?;
    }

    match direction {
        Direction::Forward => journal.mark_done(index),
        Direction::Backward => journal.moves[index].done = false,
    }
    write_journal(paths, journal)
}

/// Run a planned swap's renames, recording progress as it goes.
///
/// Precondition: no journal file may already exist at
/// `paths.desktop_journal_file()`. `recover_if_interrupted` runs at the
/// start of every byte command specifically so that precondition holds by
/// the time any command plans a new swap and calls this function --
/// starting a new plan on top of an unrepaired one would overwrite the only
/// record of it while its `from` paths may already have moved. `execute`
/// enforces the precondition itself, refusing to start, rather than
/// trusting every future caller to have run recovery first.
///
/// RETURNS WITH THE JOURNAL STILL ON DISK, deliberately. The renames are
/// only part of a swap: the desktop app decides which account it is signed
/// in as from a handful of keys in its own `config.json`, and until those are
/// patched the directories on disk belong to one account while the app
/// authenticates as another. Clearing here would put that patch outside the
/// commit boundary, and every way it can end -- a crash, or an ordinary
/// `Err` from the backup copy -- would leave that mixed state with no record
/// of it for any later command to repair. `ops::desktop::switch_desktop`
/// calls `clear_journal` once BOTH halves have landed; a patch that fails
/// leaves the journal for the next command, which finishes it through
/// `recover_if_interrupted`.
///
/// An empty plan still writes a journal, for the same reason: "no directory
/// moves" does not mean "no work", it means the swap is nothing but the
/// config patch, and that patch needs a record too.
pub fn execute(paths: &impl HostPaths, mut journal: Journal) -> Result<()> {
    let file = paths.desktop_journal_file();
    if exists(&file)? {
        return Err(Error::DesktopSwapInterrupted {
            journal: file,
            detail: "An earlier swap left this journal behind and it was never repaired. \
                     byte will not start a new swap over it, since that would destroy the \
                     only record of the old one while its files may already be half-moved. \
                     Run any byte command first -- recovery runs automatically at the start \
                     of every command -- then retry."
                .to_string(),
        });
    }

    // The whole plan hits disk BEFORE the first rename. This ordering is the
    // entire point: a journal written afterwards would describe a swap that
    // had already partly happened.
    write_journal(paths, &journal)?;

    for index in 0..journal.moves.len() {
        if journal.moves[index].done {
            continue;
        }
        apply_move(paths, &mut journal, index, Direction::Forward)?;
    }

    Ok(())
}

/// What a repair managed to finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repair {
    pub recovery: Recovery,
    pub identity: IdentityRepair,
}

/// How far the identity half of a repair got.
///
/// Separate from `Recovery` because the two halves fail independently: the
/// renames need nothing but byte's own store and the app's directory, while
/// the patch needs to find, read and rewrite `config.json`. A caller has to
/// be able to tell the user which of the two is still outstanding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityRepair {
    /// `config.json` now describes the session that is in place.
    Restored,
    /// There was nothing recorded to restore, and `config.json` already
    /// describes the session that is in place.
    NotNeeded,
    /// The account's stored `oauth.json` is there and could not be read. On
    /// a roll-forward the app was left signed out rather than
    /// authenticating as the account whose session just moved away; on a
    /// reversal `config.json` was left exactly as it was, since it still
    /// describes the session being restored.
    Unreadable(PathBuf),
    /// No desktop paths were available, so `config.json` was never reached
    /// -- and there was identity work that needed it. The journal is
    /// deliberately left on disk: only a later command, one that can locate
    /// the app, is able to finish this. A repair with provably no identity
    /// work reports `NotNeeded` instead, whether or not paths were
    /// available; see `restore_identity`.
    Deferred,
    /// The journal names an account (`incoming` on a roll-forward,
    /// `outgoing` on a reversal) that cannot safely name a directory inside
    /// byte's profile store -- see
    /// [`crate::paths::is_profile_store_component`]. `config.json` was
    /// never reached, exactly as for `Deferred` and for the same reason the
    /// journal is kept: `switch_desktop` refuses to write such a value into
    /// a journal in the first place (see `ops::desktop`'s own check on
    /// `incoming`), so reaching this means an existing journal predates that
    /// check, or was edited by hand. There is nothing here a later command
    /// can safely infer either, so this is not repaired automatically.
    AccountIdentifierInvalid(String),
}

/// Finish the identity half of a repair: make `config.json` describe
/// whichever session the directory half just put in place.
///
/// `journal.outgoing` and `journal.incoming` are what name it. Roll-forward
/// ends with the incoming account's profile live, so the incoming account's
/// parked `oauth.json` is what belongs in `config.json` -- or nothing at
/// all, when the swap was installing nothing and the app must end up signed
/// out. Reversal ends with the outgoing account's profile back in place, so
/// its parked keys are what belong there.
///
/// Reversal never CLEARS, only restores: `config.json` on that path is
/// either untouched (still the outgoing account's, since the patch runs
/// after every rename) or already cleared by a patch that did run. It can
/// never hold the incoming account's keys, so there is no blend to break up
/// -- and clearing a file that legitimately describes the session being
/// restored would sign the user out for no reason.
///
/// Which is also why a reversal naming no outgoing account is `NotNeeded`
/// rather than work: there is nothing to restore and nothing to clear. That
/// is decided first, above the `desktop` gate, because it is an answer about
/// the journal alone.
fn restore_identity<P: HostPaths, D: DesktopPaths>(
    paths: &P,
    desktop: Option<&D>,
    journal: &Journal,
    recovery: Recovery,
) -> Result<IdentityRepair> {
    let account = match recovery {
        Recovery::RollForward => journal.incoming.as_deref(),
        Recovery::Reverse => journal.outgoing.as_deref(),
    };

    // Settled BEFORE the desktop gate below, deliberately. A reversal with no
    // outgoing account has no identity work in it at all -- there is no
    // account whose keys belong in `config.json`, and a reversal never clears
    // -- so this answer needs no desktop paths and cannot change once they
    // are available. Deferring it instead would keep a journal recording
    // nothing outstanding, and `recover_if_interrupted` deliberately never
    // clears a `Deferred` journal: every later byte command would re-run the
    // same repair and warn about a swap that was already as finished as it
    // can be.
    if account.is_none() && recovery == Recovery::Reverse {
        return Ok(IdentityRepair::NotNeeded);
    }

    let Some(desktop) = desktop else {
        return Ok(IdentityRepair::Deferred);
    };
    let config_file = desktop.config_file();
    let backups = paths.backup_dir();

    let signed_out = config::DesktopOauth::default();

    let Some(account) = account else {
        // Roll-forward only, by the check above. Nothing was being
        // installed, so the swap's own ending is a signed-out app waiting
        // for a login to capture.
        config::apply_if_changed(&config_file, &signed_out, &backups)?;
        return Ok(IdentityRepair::Restored);
    };

    // Defense in depth, mirroring `ops::desktop::switch_desktop`'s own check
    // on `incoming`: `account` reached this function straight from the
    // journal on disk (`journal.incoming` or `journal.outgoing`), and a
    // journal is data byte wrote in the past, not data this call has any way
    // to have validated itself. `switch_desktop` refuses to persist a value
    // that fails this check, so an on-disk journal should never carry one --
    // but "should never" is exactly the class of assumption every other
    // arm in this store, park_under included, treats as worth checking
    // anyway, because there is no arm where letting an unusable identifier
    // reach `desktop_profile_dir` would be correct.
    if !crate::paths::is_profile_store_component(account) {
        return Ok(IdentityRepair::AccountIdentifierInvalid(
            account.to_string(),
        ));
    }

    let stored = paths.desktop_profile_dir(account).join("oauth.json");
    match config::read_stored(&stored) {
        config::StoredOauth::Loaded(oauth) => {
            config::apply_if_changed(&config_file, &oauth, &backups)?;
            Ok(IdentityRepair::Restored)
        }
        config::StoredOauth::Absent if recovery == Recovery::RollForward => {
            config::apply_if_changed(&config_file, &signed_out, &backups)?;
            Ok(IdentityRepair::Restored)
        }
        config::StoredOauth::Absent => Ok(IdentityRepair::NotNeeded),
        config::StoredOauth::Unreadable(_) if recovery == Recovery::RollForward => {
            config::apply_if_changed(&config_file, &signed_out, &backups)?;
            Ok(IdentityRepair::Unreadable(stored))
        }
        config::StoredOauth::Unreadable(_) => Ok(IdentityRepair::Unreadable(stored)),
    }
}

/// Repair a swap left behind by a crash, kill, or power loss.
///
/// Called at the start of EVERY byte command, not just `switch`: a half-swap
/// has to be repaired by whatever runs next, not only by a retry of the
/// command that failed.
///
/// `desktop` is `None` when no `DesktopPaths` could be discovered for this
/// platform or environment. The directory half is repaired anyway -- it
/// needs only the paths recorded in the journal -- and the journal is then
/// left in place, because the identity half is still outstanding and this
/// process cannot finish it. Skipping the repair entirely would strand the
/// directories as well as the identity.
pub fn recover_if_interrupted<P: HostPaths, D: DesktopPaths>(
    paths: &P,
    desktop: Option<&D>,
) -> Result<Option<Repair>> {
    let file = paths.desktop_journal_file();
    let bytes = match std::fs::read(&file) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(Error::Io { path: file, source }),
    };

    let journal = attach_journal_path(Journal::from_bytes(&bytes), &file).map_err(|e| {
        Error::DesktopSwapInterrupted {
            journal: file.clone(),
            detail: format!(
                "Its journal could not be read ({e}), so byte will not guess. Inspect it by hand."
            ),
        }
    })?;

    if journal.version != crate::desktop::journal::JOURNAL_VERSION {
        return Err(Error::DesktopSwapInterrupted {
            journal: file,
            detail: format!(
                "It was written by byte version {} of the journal format, which this build does not understand. \
                 Upgrade byte rather than letting an older build act on it.",
                journal.version
            ),
        });
    }

    let mut journal = journal;
    let recovery = recovery_for(&journal);
    match recovery {
        Recovery::RollForward => {
            for index in 0..journal.moves.len() {
                if journal.moves[index].done {
                    continue;
                }
                apply_move(paths, &mut journal, index, Direction::Forward)?;
            }
        }
        Recovery::Reverse => {
            // Reverse order, so a directory is never restored on top of one
            // still waiting to be moved out of the way: every install is
            // undone before the park that would put a directory back where
            // that install had placed one.
            //
            // Every index is visited, not just those marked `done`: a move
            // whose rename already happened before a crash but whose `done`
            // flag never persisted must still be reversed, or its directory
            // is stranded -- `apply_move`'s own reading of the two paths,
            // not the stale `done` flag, is what decides whether there is
            // anything left to do. That reading is also what keeps the
            // common case cheap and safe: most installs reached here never
            // ran at all, and their two ends are both still occupied.
            for index in (0..journal.moves.len()).rev() {
                apply_move(paths, &mut journal, index, Direction::Backward)?;
            }
        }
    }

    let identity = restore_identity(paths, desktop, &journal, recovery)?;

    // The journal is the record of BOTH halves, so it is cleared only when
    // both are as finished as they are going to get. `Deferred` is not
    // finished: this process could not locate the app, so the record has to
    // survive for one that can. `AccountIdentifierInvalid` is not finished
    // either, for a different reason -- the app WAS locatable, but the
    // journal names an account this process will not use to look up a
    // stored profile, so nothing here can be assumed safe to discard
    // automatically; it needs a human, not a later byte command.
    if !matches!(
        identity,
        IdentityRepair::Deferred | IdentityRepair::AccountIdentifierInvalid(_)
    ) {
        clear_journal(paths)?;
    }
    Ok(Some(Repair { recovery, identity }))
}
