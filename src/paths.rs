//! Filesystem locations byte reads and writes.
//!
//! `HostPaths` is the primary test seam: production code uses [`RealPaths`],
//! tests use [`TestPaths`], which is rooted in a temporary directory that is
//! deleted when it drops.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Where the Claude Code config files and byte's own state live.
pub trait HostPaths: Send + Sync {
    /// `~/.claude.json` — the large document holding `oauthAccount`.
    fn claude_config(&self) -> PathBuf;

    /// `~/.claude/.credentials.json` — holds `claudeAiOauth`.
    fn claude_credentials(&self) -> PathBuf;

    /// byte's own configuration directory.
    fn byte_config_dir(&self) -> PathBuf;

    /// Where pre-write backups are kept.
    fn backup_dir(&self) -> PathBuf {
        self.byte_config_dir().join("backups")
    }

    /// The account metadata file.
    fn accounts_file(&self) -> PathBuf {
        self.byte_config_dir().join("accounts.json")
    }

    /// Where parked desktop profiles are stored, one directory per account.
    ///
    /// byte's own state, not the app's, so it sits beside `accounts.json`
    /// rather than inside `%APPDATA%\Claude`. Note that unlike everything
    /// else byte stores, its contents are live session credentials as plain
    /// files -- see §6 of the desktop-switching design for why the keychain
    /// cannot hold them.
    fn desktop_store_dir(&self) -> PathBuf {
        self.byte_config_dir().join("desktop")
    }

    /// The in-progress swap journal. Present only mid-swap.
    fn desktop_journal_file(&self) -> PathBuf {
        self.desktop_store_dir().join(JOURNAL_FILE_NAME)
    }

    /// One account's parked profile.
    ///
    /// Callers must satisfy [`is_profile_store_component`] for
    /// `account_uuid` first; this join cannot check it (see that function
    /// for what an unchecked value does to the path).
    fn desktop_profile_dir(&self, account_uuid: &str) -> PathBuf {
        self.desktop_store_dir().join(account_uuid)
    }
}

/// The swap journal's name inside the profile store.
///
/// Named rather than inlined because [`is_profile_store_component`] has to
/// refuse it: the journal and the per-account profile directories share one
/// directory, so an account identifier of `journal.json` would name the
/// journal itself.
const JOURNAL_FILE_NAME: &str = "journal.json";

/// Is `name` usable as ONE entry name inside the desktop profile store?
///
/// [`HostPaths::desktop_profile_dir`] joins this string straight onto the
/// store path, and `Path::join` is not a string append: an absolute
/// component REPLACES the base outright, a separator nests, and `..` walks
/// up. An unvalidated value therefore redirects not only the park itself but
/// the `to` paths the swap journal records -- which is what a later recovery
/// renames against, and what `ops::manage::remove` later deletes.
///
/// That matters because the string is not always byte's own. With Claude
/// Code logged out, `ops::desktop::park_target` derives the park target from
/// the desktop app's `config.json` (`lastKnownAccountUuid`) rather than from
/// byte's `accounts.json`. Anyone able to write that file can already write
/// the profile store, so this is not a privilege boundary -- it is the
/// ordinary rule that input byte did not produce does not get to steer a
/// path.
///
/// Deliberately a check on SHAPE, not on content: byte treats account
/// identifiers as opaque strings everywhere else (see `store::metadata`), so
/// "must look like a uuid" would be a new assumption about the format of
/// somebody else's identifier.
pub fn is_profile_store_component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.eq_ignore_ascii_case(JOURNAL_FILE_NAME)
        && !name.contains(['/', '\\', ':', '\0'])
}

/// Lets `&TestPaths` and `&RealPaths` satisfy `HostPaths`, so callers can hold
/// a cheap `Copy` reference instead of taking ownership. Later types are
/// generic over `P: HostPaths + Copy`, which this makes possible.
impl<T: HostPaths + ?Sized> HostPaths for &T {
    fn claude_config(&self) -> PathBuf {
        (**self).claude_config()
    }
    fn claude_credentials(&self) -> PathBuf {
        (**self).claude_credentials()
    }
    fn byte_config_dir(&self) -> PathBuf {
        (**self).byte_config_dir()
    }
}

/// Production paths, resolved from the user's home directory.
///
/// Two environment variables override discovery:
/// - `CLAUDE_CONFIG_DIR` — directory containing `.claude.json` and `.credentials.json`
/// - `BYTE_CONFIG_DIR` — byte's own config directory
#[derive(Debug, Clone)]
pub struct RealPaths {
    claude_config: PathBuf,
    claude_credentials: PathBuf,
    byte_config_dir: PathBuf,
}

impl RealPaths {
    pub fn discover() -> Result<Self> {
        let claude_dir = std::env::var_os("CLAUDE_CONFIG_DIR");
        let byte_dir = std::env::var_os("BYTE_CONFIG_DIR");

        // home_dir() reads $HOME/%USERPROFILE%, which can legitimately be
        // unset on a service account or a minimal CI container. Only
        // resolve it when `resolve()` will actually use it below -- if both
        // overrides are set, home is never consulted, so its absence must
        // not block discovery (a machine with both env vars set and no
        // $HOME could otherwise fail with a nonsensical
        // ClaudeFileMissing("$HOME") despite neither override needing it).
        let home = if claude_dir.is_some() && byte_dir.is_some() {
            PathBuf::new()
        } else {
            home_dir().ok_or_else(|| Error::ClaudeFileMissing(PathBuf::from("$HOME")))?
        };

        Ok(Self::resolve(
            &home,
            claude_dir.as_deref(),
            byte_dir.as_deref(),
        ))
    }

    /// Pure path arithmetic, split out from [`discover`] so the mapping from
    /// environment overrides to concrete paths is directly testable against
    /// fixed inputs, rather than only through process-global environment
    /// variables shared by every test in the binary. `pub` (like
    /// `cli::run::resolve_add_failure`) specifically so `tests/paths_test.rs`
    /// can reach it.
    ///
    /// `home` is read only when the corresponding override (`claude_dir` or
    /// `byte_dir`) is absent. A caller that already knows both overrides are
    /// set -- like [`discover`] -- may pass any placeholder path, since it
    /// is then never dereferenced.
    pub fn resolve(home: &Path, claude_dir: Option<&OsStr>, byte_dir: Option<&OsStr>) -> Self {
        let (claude_config, claude_credentials) = match claude_dir {
            Some(dir) => {
                let dir = PathBuf::from(dir);
                (dir.join(".claude.json"), dir.join(".credentials.json"))
            }
            None => (
                home.join(".claude.json"),
                home.join(".claude").join(".credentials.json"),
            ),
        };

        let byte_config_dir = match byte_dir {
            Some(dir) => PathBuf::from(dir),
            None => default_config_dir(home),
        };

        Self {
            claude_config,
            claude_credentials,
            byte_config_dir,
        }
    }
}

impl HostPaths for RealPaths {
    fn claude_config(&self) -> PathBuf {
        self.claude_config.clone()
    }
    fn claude_credentials(&self) -> PathBuf {
        self.claude_credentials.clone()
    }
    fn byte_config_dir(&self) -> PathBuf {
        self.byte_config_dir.clone()
    }
}

#[cfg(windows)]
fn default_config_dir(home: &Path) -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("AppData").join("Roaming"))
        .join("byte")
}

#[cfg(target_os = "macos")]
fn default_config_dir(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Application Support")
        .join("byte")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn default_config_dir(home: &Path) -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"))
        .join("byte")
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Test paths rooted in a temporary directory.
///
/// The directory is removed when this value drops, so tests must keep it
/// alive for as long as they use the paths.
#[derive(Debug)]
pub struct TestPaths {
    dir: tempfile::TempDir,
}

impl TestPaths {
    pub fn new() -> Result<Self> {
        let dir = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("<tempdir>"),
            source,
        })?;
        let this = Self { dir };
        for d in [
            this.root().join(".claude"),
            this.byte_config_dir(),
            this.backup_dir(),
        ] {
            std::fs::create_dir_all(&d).map_err(|source| Error::Io { path: d, source })?;
        }
        Ok(this)
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }
}

impl HostPaths for TestPaths {
    fn claude_config(&self) -> PathBuf {
        self.root().join(".claude.json")
    }
    fn claude_credentials(&self) -> PathBuf {
        self.root().join(".claude").join(".credentials.json")
    }
    fn byte_config_dir(&self) -> PathBuf {
        self.root().join("byte-config")
    }
}
