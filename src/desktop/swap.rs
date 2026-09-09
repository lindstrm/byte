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

/// Run a planned swap, recording progress as it goes.
pub fn execute(paths: &impl HostPaths, mut journal: Journal) -> Result<()> {
    if journal.moves.is_empty() {
        return Ok(());
    }

    // The whole plan hits disk BEFORE the first rename. This ordering is the
    // entire point: a journal written afterwards would describe a swap that
    // had already partly happened.
    write_journal(paths, &journal)?;

    for index in 0..journal.moves.len() {
        let (from, to) = {
            let m = &journal.moves[index];
            (m.from.clone(), m.to.clone())
        };
        rename(&from, &to)?;
        journal.mark_done(index);
        write_journal(paths, &journal)?;
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

    let recovery = recovery_for(&journal);
    match recovery {
        Recovery::RollForward => {
            let mut journal = journal;
            for index in 0..journal.moves.len() {
                if journal.moves[index].done {
                    continue;
                }
                let (from, to) = {
                    let m = &journal.moves[index];
                    (m.from.clone(), m.to.clone())
                };
                rename(&from, &to)?;
                journal.mark_done(index);
                write_journal(paths, &journal)?;
            }
        }
        Recovery::Reverse => {
            // Reverse order, so a directory is never restored on top of one
            // still waiting to be moved out of the way.
            let mut journal = journal;
            for index in (0..journal.moves.len()).rev() {
                if !journal.moves[index].done {
                    continue;
                }
                let (from, to) = {
                    let m = &journal.moves[index];
                    (m.from.clone(), m.to.clone())
                };
                rename(&to, &from)?;
                journal.moves[index].done = false;
                write_journal(paths, &journal)?;
            }
        }
    }

    clear_journal(paths)?;
    Ok(Some(recovery))
}
