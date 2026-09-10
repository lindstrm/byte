//! Error type for byte.

use std::path::PathBuf;

/// Every fallible operation in this crate returns this error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Claude Code file not found: {0}\nIs Claude Code installed and logged in?")]
    ClaudeFileMissing(PathBuf),

    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("no account matching '{0}'")]
    NoSuchAccount(String),

    #[error("'{query}' is ambiguous; it matches {count} accounts")]
    AmbiguousAccount { query: String, count: usize },

    #[error("no Claude account is currently logged in")]
    NotLoggedIn,

    #[error(
        "the live Claude Code login has credentials but {config_path} has no identifiable \
         account (missing accountUuid and emailAddress); byte refused to switch rather than \
         risk losing it. Nothing was changed, and your current login is untouched. Relaunch \
         Claude Code and try again."
    )]
    UnidentifiableLiveAccount { config_path: PathBuf },

    #[error("unsupported snapshot schema version {found}; this build expects {expected}")]
    SchemaMismatch { found: u32, expected: u32 },

    #[error("unsupported accounts.json schema version {found}; this build expects {expected}")]
    AccountsSchemaMismatch { found: u32, expected: u32 },

    #[error("stored credentials for '{account}' are unusable: {reason}")]
    InvalidSnapshot { account: String, reason: String },

    #[error("secret store unavailable: {0}")]
    Secret(String),

    #[error("tray error: {0}")]
    Tray(String),

    #[error("failed to render JSON output: {0}")]
    Render(String),

    #[error("write verification failed for {path}; the original was restored from backup")]
    VerifyFailed { path: PathBuf },

    #[error(
        "write verification failed for {path}, and restoring the pre-write backup afterwards \
         also failed: {restore_source}\n\
         the file may now hold neither the old nor the new content and must be checked by hand"
    )]
    VerifyRestoreFailed {
        path: PathBuf,
        #[source]
        restore_source: Box<Error>,
    },

    #[error(
        "applying the account snapshot failed ({apply_error}), and rolling back {creds_path} \
         afterwards also failed: {rollback_source}\n\
         the credentials and config files may now disagree about which account is active \
         and must be checked by hand"
    )]
    ApplyRollbackFailed {
        creds_path: PathBuf,
        apply_error: String,
        #[source]
        rollback_source: Box<Error>,
    },

    #[error("timed out after {0} seconds waiting for a new login")]
    LoginTimeout(u64),

    #[error("{action} needs confirmation; re-run with --yes to proceed without prompting")]
    ConfirmationRequired { action: String },

    #[error(
        "another byte process is currently changing accounts.\n\
         Wait for it to finish and try again."
    )]
    Busy,

    #[error(
        "a desktop profile swap was interrupted and could not be repaired automatically.\n\
         Its journal is at {journal}\n\
         {detail}"
    )]
    DesktopSwapInterrupted { journal: PathBuf, detail: String },
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;
