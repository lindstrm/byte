//! Capturing, syncing back, and switching accounts (spec §7).

use serde_json::Value;

use crate::claude::files::ClaudeFiles;
use crate::claude::snapshot::AccountSnapshot;
use crate::error::{Error, Result};
use crate::paths::HostPaths;
use crate::store::metadata::{AccountMeta, AccountsFile};
use crate::store::secrets::SecretStore;

/// What sync-back did before a switch proceeded.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncOutcome {
    /// A known account's stored credentials were refreshed from disk.
    Updated(AccountMeta),
    /// A live account byte had never seen was saved (spec §7.2).
    Captured(AccountMeta),
    /// Nothing was logged in.
    LoggedOut,
}

/// The result of a completed switch.
#[derive(Debug, Clone)]
pub struct SwitchOutcome {
    pub switched_to: AccountMeta,
    pub sync: SyncOutcome,
    pub already_active: bool,
}

pub struct Switcher<P: HostPaths, S: SecretStore> {
    paths: P,
    secrets: S,
}

impl<P: HostPaths + Copy, S: SecretStore> Switcher<P, S> {
    pub fn new(paths: P, secrets: S) -> Self {
        Self { paths, secrets }
    }

    pub fn secrets(&self) -> &S {
        &self.secrets
    }

    fn files(&self) -> ClaudeFiles<P> {
        ClaudeFiles::new(self.paths)
    }

    /// Exposed for the add flow, which needs direct file access.
    pub(crate) fn files_for_add(&self) -> ClaudeFiles<P> {
        self.files()
    }

    /// Crate-visible for `ops::manage`, which lists and edits account
    /// metadata directly without going through a capture or switch.
    pub(crate) fn load_accounts(&self) -> Result<AccountsFile> {
        AccountsFile::load(&self.paths.accounts_file())
    }

    pub(crate) fn save_accounts(&self, file: &AccountsFile) -> Result<()> {
        file.save(&self.paths.accounts_file(), &self.paths.backup_dir())
    }

    /// Reassemble a stored account's complete snapshot from its two halves:
    /// the secret `oauth` block in the OS keychain, and the non-secret
    /// `account` / `user_id` / `credential_schema` recorded alongside it in
    /// `meta` (see `store::metadata` and `store::secrets` for why they are
    /// split, and `AccountSnapshot::reassemble` for why the schema is
    /// threaded through rather than assumed current).
    ///
    /// The two halves are written together (every `put`/`upsert_from` pair
    /// in this file) but not atomically -- they are two different storage
    /// backends -- so this is the seam where a disagreement between them
    /// would surface: the keychain entry deleted out from under byte,
    /// `accounts.json` hand-edited, or a crash between the two writes. The
    /// `validate()` call at the end is what actually catches that: a
    /// half-reassembled snapshot must never reach `apply()`.
    ///
    /// `pub`, like `cli::run::resolve_add_failure`, specifically so
    /// `tests/switch_test.rs` can exercise reassembly's failure modes
    /// directly, without needing a full live Claude Code login to reach
    /// them through `switch_to`.
    pub fn load_snapshot(&self, meta: &AccountMeta) -> Result<AccountSnapshot> {
        let oauth = self
            .secrets
            .get(&meta.uuid)?
            .ok_or_else(|| Error::InvalidSnapshot {
                account: meta.label.clone(),
                reason: "no stored credentials in the OS keychain; re-authenticate with \
                         `byte add`"
                    .into(),
            })?;

        let snapshot = AccountSnapshot::reassemble(
            oauth,
            meta.account.clone(),
            meta.user_id.clone(),
            meta.credential_schema,
        );
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Save whatever account is live right now, then mark it active.
    pub fn capture_current(&self) -> Result<AccountMeta> {
        let snapshot = self.files().capture()?.ok_or(Error::NotLoggedIn)?;
        snapshot.validate()?;

        // validate() already returned Err above if identity() were None, so
        // this is guaranteed to be Some.
        let uuid = snapshot
            .identity()
            .expect("validate() guarantees an identity")
            .to_string();

        let mut accounts = self.load_accounts()?;
        let meta = accounts.upsert_from(&uuid, &snapshot);
        accounts.set_active(&uuid);

        self.secrets.put(&uuid, &snapshot.oauth)?;
        self.save_accounts(&accounts)?;
        Ok(meta)
    }

    /// Copy the live credentials into the store, so a token Claude Code
    /// rotated behind byte's back is not lost.
    ///
    /// A live snapshot with no refresh token at all is genuinely logged
    /// out, which is reported as [`SyncOutcome::LoggedOut`] rather than an
    /// error. But a live snapshot that DOES carry a refresh token and still
    /// fails validation — in practice, `.claude.json` has no
    /// `oauthAccount`, so there is no identity to key it by — is not
    /// logged out: there are real credentials on disk this call cannot
    /// safely discard by mislabelling them "logged out". That case is a
    /// hard error instead, specifically so `switch_to`'s `?` aborts before
    /// reaching `apply()`, which would otherwise overwrite those
    /// credentials having never captured them anywhere (spec §7.1).
    pub fn sync_back(&self) -> Result<SyncOutcome> {
        let Some(snapshot) = self.files().capture()? else {
            return Ok(SyncOutcome::LoggedOut);
        };

        if snapshot.validate().is_err() {
            let has_refresh_token = snapshot
                .oauth
                .get("refreshToken")
                .and_then(Value::as_str)
                .is_some_and(|token| !token.is_empty());

            return if has_refresh_token {
                Err(Error::UnidentifiableLiveAccount {
                    config_path: self.paths.claude_config(),
                })
            } else {
                Ok(SyncOutcome::LoggedOut)
            };
        }

        // validate() already returned Err above if identity() were None, so
        // this is guaranteed to be Some.
        let uuid = snapshot
            .identity()
            .expect("validate() guarantees an identity")
            .to_string();

        let mut accounts = self.load_accounts()?;
        let known = accounts.accounts.iter().any(|a| a.uuid == uuid);

        let meta = accounts.upsert_from(&uuid, &snapshot);
        self.secrets.put(&uuid, &snapshot.oauth)?;
        self.save_accounts(&accounts)?;

        Ok(if known {
            SyncOutcome::Updated(meta)
        } else {
            SyncOutcome::Captured(meta)
        })
    }

    /// Switch to a stored account, syncing the current one back first.
    pub fn switch_to(&self, query: &str) -> Result<SwitchOutcome> {
        // Resolve before touching anything, so an unknown name is a clean
        // no-op rather than a half-applied switch.
        let target_uuid = {
            let accounts = self.load_accounts()?;
            accounts.resolve(query)?.uuid.clone()
        };

        let sync = self.sync_back()?;

        // Reloaded after sync_back, not reused from the resolve above --
        // sync_back may just have written a fresh copy of the *live*
        // account's metadata (including, when the target is already
        // active, the target's own entry), and load_snapshot below must see
        // that write, not a stale in-memory copy from before it happened.
        let mut accounts = self.load_accounts()?;
        let target_meta = accounts
            .accounts
            .iter()
            .find(|a| a.uuid == target_uuid)
            .cloned()
            .ok_or_else(|| Error::NoSuchAccount(target_uuid.clone()))?;
        let snapshot = self.load_snapshot(&target_meta)?;

        // Derived from the identity `sync_back` just confirmed live, not
        // `accounts.active` -- `sync_back` never updates that pointer, so it
        // can be stale relative to what Claude Code is actually
        // authenticated as right now (e.g. the user ran `/logout` and logged
        // in as a different account directly, bypassing byte entirely). A
        // stale pointer would report `already_active: true` for a switch
        // that in fact just changed the live account, which also suppresses
        // the "sessions must be restarted" warning below -- so `switch_to`
        // would tell the user nothing happened at all when something did.
        let already_active = match &sync {
            SyncOutcome::Updated(meta) | SyncOutcome::Captured(meta) => meta.uuid == target_uuid,
            SyncOutcome::LoggedOut => false,
        };

        self.files().apply(&snapshot)?;

        accounts.upsert_from(&target_uuid, &snapshot);
        accounts.set_active(&target_uuid);
        self.save_accounts(&accounts)?;

        // An exact uuid lookup, not `resolve()`: after the switch has
        // committed, a second fuzzy lookup could in principle match more
        // than one account (e.g. one account's uuid-as-identity fallback
        // colliding with another account's email) and report
        // `AmbiguousAccount` here — masking a switch that already succeeded
        // behind the wrong error.
        let switched_to = accounts
            .accounts
            .iter()
            .find(|a| a.uuid == target_uuid)
            .cloned()
            .ok_or_else(|| Error::NoSuchAccount(target_uuid.clone()))?;

        Ok(SwitchOutcome {
            switched_to,
            sync,
            already_active,
        })
    }
}
