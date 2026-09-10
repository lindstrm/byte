//! Executing a journal against the filesystem.
//!
//! `Partitions` is not moved by any of this, deliberately: its
//! account-scoped entry is already named `cowork-artifact-<accountUuid>-...`
//! and so self-segregates -- distinct accounts get distinct directories, and
//! switching back finds the previous one intact.

use std::path::Path;

use crate::desktop::journal::{Journal, Stage};
use crate::error::{Error, Result};
use crate::paths::HostPaths;

/// What to do with an interrupted swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Finish the install: the incoming profile is already partly in place.
    RollForward,
    /// Undo the parks: nothing of the incoming profile has landed yet.
    Reverse,
}

/// Decide by stage, never by a count.
///
/// One completed `Install` is enough to commit to rolling forward: the
/// incoming profile is already partly live, and reversing would have to
/// unpick it while the outgoing profile is only half parked.
pub fn recovery_for(journal: &Journal) -> Recovery {
    let any_install_done = journal
        .moves
        .iter()
        .any(|m| m.stage == Stage::Install && m.done);

    if any_install_done {
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

fn clear_journal(paths: &impl HostPaths) -> Result<()> {
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
///   `done` as it is. Two situations produce this state, and the reverse
///   walk order is what makes a no-op right for both:
///     1. The move never ran. This is far from exotic -- it is the resting
///        state of every install move throughout the whole park stage: the
///        incoming profile is still in its store and the live directory it
///        would replace has not been parked yet.
///     2. A *later* move re-occupied this move's destination. A park of
///        name N has destination `live/N`, which the install of the same
///        name re-fills; so a park can be both-occupied even with
///        `done: true`.
///   Case 2 is safe ONLY because `Journal::plan` emits every park before
///   every install and `recover_if_interrupted` walks in reverse, so any
///   later move that re-occupied this destination has already been undone
///   by the time this one is visited. That ordering is the guarantee --
///   not, as an earlier version of this comment claimed, that a swap can
///   never re-occupy a destination. **Do not reorder or parallelise the
///   reverse loop**: doing so reintroduces the stranded-directory bug this
///   arm exists to prevent.
///
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

/// Run a planned swap, recording progress as it goes.
///
/// Precondition: no journal file may already exist at
/// `paths.desktop_journal_file()`. `recover_if_interrupted` runs at the
/// start of every byte command specifically so that precondition holds by
/// the time any command plans a new swap and calls this function --
/// starting a new plan on top of an unrepaired one would overwrite the only
/// record of it while its `from` paths may already have moved. `execute`
/// enforces the precondition itself, refusing to start, rather than
/// trusting every future caller to have run recovery first.
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

    if journal.moves.is_empty() {
        return Ok(());
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

    clear_journal(paths)
}

/// Repair a swap left behind by a crash, kill, or power loss.
///
/// Called at the start of EVERY byte command, not just `switch`: a half-swap
/// has to be repaired by whatever runs next, not only by a retry of the
/// command that failed.
pub fn recover_if_interrupted(paths: &impl HostPaths) -> Result<Option<Recovery>> {
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

    clear_journal(paths)?;
    Ok(Some(recovery))
}
