//! Where the Claude desktop app keeps its data.
//!
//! Mirrors `crate::paths::HostPaths`: production uses [`RealDesktopPaths`],
//! tests use [`TestDesktopPaths`], rooted in a temporary directory. Kept
//! separate from `HostPaths` because it describes *another application's*
//! layout, which byte does not own and which can change out from under it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Where the Claude desktop app stores its session.
pub trait DesktopPaths: Send + Sync {
    /// The app's data directory (`%APPDATA%\Claude`).
    fn desktop_dir(&self) -> PathBuf;

    /// `config.json` -- patched in place, never moved, because it mixes the
    /// account's OAuth cache with the user's theme and window layout.
    fn config_file(&self) -> PathBuf {
        self.desktop_dir().join("config.json")
    }
}

impl<T: DesktopPaths + ?Sized> DesktopPaths for &T {
    fn desktop_dir(&self) -> PathBuf {
        (**self).desktop_dir()
    }
}

/// Production paths.
///
/// `CLAUDE_DESKTOP_DIR` overrides discovery, joining `CLAUDE_CONFIG_DIR` and
/// `BYTE_CONFIG_DIR`. It exists so the swap can be exercised end to end
/// against a synthetic profile tree without touching the real app -- which
/// matters more here than elsewhere, because testing against the real app
/// means being signed out mid-session.
#[derive(Debug, Clone)]
pub struct RealDesktopPaths {
    desktop_dir: PathBuf,
}

impl RealDesktopPaths {
    pub fn discover() -> Result<Self> {
        let override_dir = std::env::var_os("CLAUDE_DESKTOP_DIR");

        // Only consult APPDATA when the override is absent, matching
        // RealPaths::discover's treatment of $HOME.
        let appdata = if override_dir.is_some() {
            PathBuf::new()
        } else {
            std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .ok_or_else(|| Error::ClaudeFileMissing(PathBuf::from("%APPDATA%")))?
        };

        Ok(Self::resolve(&appdata, override_dir.as_deref()))
    }

    /// Pure path arithmetic, split out from [`discover`] so the mapping is
    /// testable against fixed inputs rather than process-global environment
    /// variables shared by every test in the binary.
    pub fn resolve(appdata: &Path, override_dir: Option<&OsStr>) -> Self {
        let desktop_dir = match override_dir {
            Some(dir) => PathBuf::from(dir),
            None => appdata.join("Claude"),
        };
        Self { desktop_dir }
    }
}

impl DesktopPaths for RealDesktopPaths {
    fn desktop_dir(&self) -> PathBuf {
        self.desktop_dir.clone()
    }
}

/// A synthetic desktop directory in a temp dir, deleted when it drops.
#[derive(Debug)]
pub struct TestDesktopPaths {
    dir: tempfile::TempDir,
}

impl TestDesktopPaths {
    pub fn new() -> Result<Self> {
        let dir = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("<tempdir>"),
            source,
        })?;
        let this = Self { dir };
        let d = this.desktop_dir();
        std::fs::create_dir_all(&d).map_err(|source| Error::Io { path: d, source })?;
        Ok(this)
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }
}

impl DesktopPaths for TestDesktopPaths {
    fn desktop_dir(&self) -> PathBuf {
        self.root().join("Claude")
    }
}
