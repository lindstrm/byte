use byte::ops::manage;
use byte::ops::switch::Switcher;
use byte::paths::{HostPaths, TestPaths};
use byte::store::secrets::{MemoryStore, SecretStore};
use serde_json::json;

fn login_as(tp: &TestPaths, uuid: &str, email: &str) {
    std::fs::write(
        tp.claude_credentials(),
        serde_json::to_string(&json!({
            "claudeAiOauth": {"refreshToken": "r", "expiresAt": 1i64}
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        tp.claude_config(),
        serde_json::to_string_pretty(&json!({
            "oauthAccount": {"accountUuid": uuid, "emailAddress": email},
            "userID": "uid"
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn list_is_empty_before_anything_is_captured() {
    let tp = TestPaths::new().unwrap();
    let sw = Switcher::new(&tp, MemoryStore::new());
    assert!(manage::list(&sw).unwrap().is_empty());
}

#[test]
fn list_marks_exactly_one_account_active() {
    let tp = TestPaths::new().unwrap();
    let sw = Switcher::new(&tp, MemoryStore::new());
    login_as(&tp, "u1", "a@example.com");
    sw.capture_current().unwrap();
    login_as(&tp, "u2", "b@example.com");
    sw.capture_current().unwrap();

    // Switch back to u1 so the active account is neither the most recently
    // captured account nor the last entry in the accounts vector. That is
    // what actually distinguishes "active is derived from the accounts
    // file's `active` pointer" from a broken implementation that just
    // guesses (e.g. always reporting the last-inserted account as active).
    sw.switch_to("a@example.com").unwrap();

    let listing = manage::list(&sw).unwrap();

    assert_eq!(listing.len(), 2);
    assert_eq!(listing.iter().filter(|l| l.active).count(), 1);
    assert!(listing.iter().find(|l| l.active).unwrap().meta.uuid == "u1");
}

#[test]
fn current_returns_the_active_account() {
    let tp = TestPaths::new().unwrap();
    let sw = Switcher::new(&tp, MemoryStore::new());
    login_as(&tp, "u1", "a@example.com");
    sw.capture_current().unwrap();

    assert_eq!(manage::current(&sw).unwrap().unwrap().uuid, "u1");
}

#[test]
fn rename_changes_the_label_and_it_is_then_resolvable() {
    let tp = TestPaths::new().unwrap();
    let sw = Switcher::new(&tp, MemoryStore::new());
    login_as(&tp, "u1", "a@example.com");
    sw.capture_current().unwrap();

    manage::rename(&sw, "a@example.com", "work").unwrap();

    assert_eq!(manage::current(&sw).unwrap().unwrap().label, "work");
    assert!(sw.switch_to("work").is_ok());
}

#[test]
fn remove_deletes_both_metadata_and_secret() {
    let tp = TestPaths::new().unwrap();
    let sw = Switcher::new(&tp, MemoryStore::new());
    login_as(&tp, "u1", "a@example.com");
    sw.capture_current().unwrap();

    manage::remove(&sw, "a@example.com").unwrap();

    assert!(manage::list(&sw).unwrap().is_empty());
    assert!(sw.secrets().get("u1").unwrap().is_none());
}

/// A parked Claude Desktop session for `uuid`: a cookie jar plus the
/// `oauth:` keys byte captured out of the app's own `config.json`. Both are
/// live credentials as plaintext files -- see SECURITY.md's location 5.
fn park_desktop_session(tp: &TestPaths, uuid: &str) -> std::path::PathBuf {
    let dir = tp.desktop_profile_dir(uuid);
    std::fs::create_dir_all(dir.join("Network")).unwrap();
    std::fs::write(dir.join("Network").join("Cookies"), b"sessionKey=live").unwrap();
    std::fs::write(
        dir.join("oauth.json"),
        json!({
            "oauth": {"oauth:tokenCache": {"accessToken": "live-desktop-token"}},
            "account_uuid": uuid
        })
        .to_string(),
    )
    .unwrap();
    dir
}

#[test]
fn remove_deletes_the_parked_desktop_session_it_says_cannot_be_recovered() {
    // `byte remove` tells the user their stored credentials cannot be
    // recovered afterward, and the account then vanishes from `byte list` --
    // so a parked desktop profile left behind is a live claude.ai session,
    // plus a plaintext `oauth:tokenCache`, that no byte command can reach
    // and the user has been told is gone.
    let tp = TestPaths::new().unwrap();
    let sw = Switcher::new(&tp, MemoryStore::new());
    login_as(&tp, "u1", "a@example.com");
    sw.capture_current().unwrap();
    let parked = park_desktop_session(&tp, "u1");

    manage::remove(&sw, "a@example.com").unwrap();

    assert!(
        !parked.exists(),
        "the removed account's desktop session must be deleted, not retained at {}",
        parked.display()
    );
}

#[test]
fn remove_leaves_every_other_accounts_desktop_session_alone() {
    // The deletion is scoped to the one account's own directory: the store
    // itself, and every other account's parked session inside it, survive.
    let tp = TestPaths::new().unwrap();
    let sw = Switcher::new(&tp, MemoryStore::new());
    login_as(&tp, "u1", "a@example.com");
    sw.capture_current().unwrap();
    login_as(&tp, "u2", "b@example.com");
    sw.capture_current().unwrap();
    park_desktop_session(&tp, "u1");
    let keep = park_desktop_session(&tp, "u2");

    manage::remove(&sw, "a@example.com").unwrap();

    assert!(keep.join("oauth.json").exists(), "u2's session must remain");
    assert!(
        tp.desktop_store_dir().is_dir(),
        "the store itself must remain"
    );
}

#[test]
fn removing_an_account_that_never_had_a_desktop_session_still_succeeds() {
    // The common case on every non-Windows machine: there is no directory to
    // delete, and that is not a failure.
    let tp = TestPaths::new().unwrap();
    let sw = Switcher::new(&tp, MemoryStore::new());
    login_as(&tp, "u1", "a@example.com");
    sw.capture_current().unwrap();

    let meta = manage::remove(&sw, "a@example.com").unwrap();

    assert_eq!(meta.uuid, "u1");
    assert!(manage::list(&sw).unwrap().is_empty());
}

#[test]
fn removing_an_unknown_account_errors() {
    let tp = TestPaths::new().unwrap();
    let sw = Switcher::new(&tp, MemoryStore::new());
    assert!(matches!(
        manage::remove(&sw, "nobody"),
        Err(byte::Error::NoSuchAccount(_))
    ));
}
