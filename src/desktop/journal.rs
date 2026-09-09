//! The record of a swap in progress.
//!
//! A swap is roughly sixteen directory renames, not one atomic write, so it
//! cannot borrow `atomic.rs`'s guarantees. This supplies the equivalent: the
//! whole intended sequence is written down BEFORE the first rename runs,
//! each entry is marked as it completes, and the file is deleted on success.
//!
//! Renames within a volume are effectively instantaneous, so the window is
//! small -- but the failure it guards is severe and silent. Without a
//! journal, an interrupted swap leaves the app apparently signed out while
//! the user's real session sits under another account's directory, with
//! nothing on screen to explain it and no way for byte to tell that from a
//! genuine signed-out state.
//!
//! Pure: plans and codecs only. Executing a journal is `swap.rs`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::desktop::profile::movable_entries;
use crate::error::{Error, Result};

/// Which half of the swap a move belongs to.
///
/// Load-bearing for recovery, not just description: see
/// `swap::recovery_for`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stage {
    /// Live -> the outgoing account's store.
    Park,
    /// The incoming account's store -> live.
    Install,
}

/// One directory rename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Move {
    pub stage: Stage,
    pub from: PathBuf,
    pub to: PathBuf,
    pub done: bool,
}

/// A swap, written down before it happens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    pub version: u32,
    /// Account whose profile is being parked, if any.
    pub outgoing: Option<String>,
    /// Account whose profile is being installed, if any.
    pub incoming: Option<String>,
    pub moves: Vec<Move>,
}

/// Bumped only if the on-disk shape changes incompatibly. A journal from a
/// future version must not be half-understood and acted on.
pub const JOURNAL_VERSION: u32 = 1;

impl Journal {
    /// Work out every rename a swap needs, parks first.
    ///
    /// Order matters: an install that ran before its corresponding park
    /// would overwrite the outgoing account's live session with the
    /// incoming one's, destroying it. `park_to`/`install_from` are `None`
    /// when there is no outgoing account to file or no captured profile to
    /// restore.
    pub fn plan(
        live_dir: &Path,
        park_to: Option<&Path>,
        install_from: Option<&Path>,
    ) -> Result<Self> {
        let mut moves = Vec::new();

        if let Some(park_to) = park_to {
            for name in movable_entries(live_dir)? {
                moves.push(Move {
                    stage: Stage::Park,
                    from: live_dir.join(&name),
                    to: park_to.join(&name),
                    done: false,
                });
            }
        }

        if let Some(install_from) = install_from {
            for name in movable_entries(install_from)? {
                moves.push(Move {
                    stage: Stage::Install,
                    from: install_from.join(&name),
                    to: live_dir.join(&name),
                    done: false,
                });
            }
        }

        Ok(Self {
            version: JOURNAL_VERSION,
            outgoing: None,
            incoming: None,
            moves,
        })
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec_pretty(self).map_err(|source| Error::Parse {
            path: PathBuf::from("<journal>"),
            source,
        })
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).map_err(|source| Error::Parse {
            path: PathBuf::from("<journal>"),
            source,
        })
    }

    pub fn mark_done(&mut self, index: usize) {
        if let Some(m) = self.moves.get_mut(index) {
            m.done = true;
        }
    }

    pub fn is_complete(&self) -> bool {
        self.moves.iter().all(|m| m.done)
    }
}
