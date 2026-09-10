//! Listing and maintaining stored accounts.

use crate::error::Result;
use crate::ops::switch::Switcher;
use crate::output;
use crate::paths::{HostPaths, is_profile_store_component};
use crate::store::metadata::AccountMeta;
use crate::store::secrets::SecretStore;

/// An account plus whether it is the active one.
#[derive(Debug, Clone)]
pub struct AccountListing {
    pub meta: AccountMeta,
    pub active: bool,
}

pub fn list<P: HostPaths + Copy, S: SecretStore>(
    sw: &Switcher<P, S>,
) -> Result<Vec<AccountListing>> {
    let file = sw.load_accounts()?;
    Ok(file
        .accounts
        .iter()
        .map(|meta| AccountListing {
            active: file.active.as_deref() == Some(meta.uuid.as_str()),
            meta: meta.clone(),
        })
        .collect())
}

pub fn current<P: HostPaths + Copy, S: SecretStore>(
    sw: &Switcher<P, S>,
) -> Result<Option<AccountMeta>> {
    Ok(sw.load_accounts()?.active_meta().cloned())
}

/// Does this account have a parked Claude Desktop session on disk?
///
/// Answered from the directory rather than from `AccountMeta`'s
/// `desktop_profile` record: that record is best-effort bookkeeping
/// (`ops::desktop::record_parked_profile` warns rather than failing, and
/// `AccountsFile::load` refuses a malformed file entirely), so it can be
/// absent for a profile that is very much there. `byte remove`'s
/// confirmation prompt and [`remove`]'s deletion have to agree about what
/// exists, so both ask the disk.
pub fn has_desktop_profile(paths: &impl HostPaths, uuid: &str) -> bool {
    is_profile_store_component(uuid) && paths.desktop_profile_dir(uuid).is_dir()
}

pub fn rename<P: HostPaths + Copy, S: SecretStore>(
    sw: &Switcher<P, S>,
    query: &str,
    label: &str,
) -> Result<AccountMeta> {
    let mut file = sw.load_accounts()?;
    let uuid = file.resolve(query)?.uuid.clone();
    let meta = file.rename(&uuid, label)?;
    sw.save_accounts(&file)?;
    Ok(meta)
}

/// Delete the metadata entry, the stored secret, and the parked desktop
/// session. The secret is deleted first: if that fails, the metadata file is
/// left untouched so the account stays visible rather than silently
/// orphaning a live refresh token in the OS keychain that `list` can no
/// longer show and the user believes is gone.
///
/// The desktop profile goes LAST, after the removal has committed, and its
/// failure is warned rather than returned -- see [`remove_desktop_profile`].
pub fn remove<P: HostPaths + Copy, S: SecretStore>(
    sw: &Switcher<P, S>,
    query: &str,
) -> Result<AccountMeta> {
    let mut file = sw.load_accounts()?;
    let uuid = file.resolve(query)?.uuid.clone();
    let meta = file.remove(&uuid)?;
    sw.secrets().delete(&uuid)?;
    sw.save_accounts(&file)?;
    remove_desktop_profile(sw.paths(), &uuid);
    Ok(meta)
}

/// Delete an account's parked Claude Desktop session, if it has one.
///
/// `byte remove` tells the user their stored credentials cannot be recovered
/// afterward, and the account then disappears from `byte list` -- so a
/// profile left behind in `<byte config>/desktop/<uuid>/` is a live claude.ai
/// session and a plaintext `oauth:tokenCache` that no byte command can reach
/// any more and the user has been told is gone. It is the one place byte
/// keeps a credential outside the OS credential store (SECURITY.md,
/// location 5), so retaining it is the opposite of what the prompt promises.
///
/// Infallible by contract, following `atomic::prune` and
/// `ops::desktop::record_parked_profile`: by the time this runs the keychain
/// entry is gone and `accounts.json` has been rewritten without the account,
/// so there is nothing left for a caller to retry or roll back. Turning a
/// committed removal into an `Err` would report a failure over an operation
/// that fully happened, and it would leave `byte remove` with no way to
/// finish. `remove_dir_all` fails on entirely ordinary conditions -- a file
/// inside the profile held open by another process is enough on Windows --
/// so this is a warning that names the directory the user can delete
/// themselves.
fn remove_desktop_profile(paths: &impl HostPaths, uuid: &str) {
    // Refusing rather than warning: a uuid that is not a single path
    // component does not name a directory inside the store at all, and a
    // recursive delete is the last operation that should act on a path it
    // cannot vouch for. Nothing was ever parked under such a name either --
    // `ops::desktop` refuses to park under one (see `park_under`).
    if !is_profile_store_component(uuid) {
        return;
    }

    let dir = paths.desktop_profile_dir(uuid);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => output::warn(&format!(
            "the account was removed, but its stored Claude Desktop session at {} could not be \
             deleted ({e}). That directory still holds a live claude.ai session and a saved \
             sign-in for it; delete it by hand.",
            dir.display()
        )),
    }
}
