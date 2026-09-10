use serde_json::json;

use byte::desktop::config::{DesktopOauth, apply, capture, clear};

fn write_config(dir: &std::path::Path) -> std::path::PathBuf {
    let p = dir.join("config.json");
    std::fs::write(
        &p,
        serde_json::to_string_pretty(&json!({
            "locale": "en-GB",
            "oauth:tokenCache": {"access": "aaa"},
            "userThemeMode": "dark",
            "oauth:tokenCacheV2": {"access": "bbb"},
            "lastKnownAccountUuid": "uuid-a",
            "windowSizeWasSignedIn": true
        }))
        .unwrap(),
    )
    .unwrap();
    p
}

#[test]
fn capture_takes_exactly_the_three_owned_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_config(tmp.path());

    let got = capture(&p).unwrap();

    assert_eq!(got.token_cache, Some(json!({"access": "aaa"})));
    assert_eq!(got.token_cache_v2, Some(json!({"access": "bbb"})));
    assert_eq!(got.account_uuid, Some(json!("uuid-a")));
}

#[test]
fn apply_changes_the_owned_keys_and_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_config(tmp.path());
    let backups = tmp.path().join("backups");

    apply(
        &p,
        &DesktopOauth {
            token_cache: Some(json!({"access": "zzz"})),
            token_cache_v2: None,
            account_uuid: Some(json!("uuid-b")),
        },
        &backups,
    )
    .unwrap();

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();

    assert_eq!(after["oauth:tokenCache"], json!({"access": "zzz"}));
    assert_eq!(after["lastKnownAccountUuid"], json!("uuid-b"));
    // Absent in the incoming account: removed, not left holding the old
    // account's token.
    assert!(after.get("oauth:tokenCacheV2").is_none());
    // The user's own settings are untouched.
    assert_eq!(after["locale"], json!("en-GB"));
    assert_eq!(after["userThemeMode"], json!("dark"));
    assert_eq!(after["windowSizeWasSignedIn"], json!(true));
}

#[test]
fn unmodelled_keys_survive_byte_for_byte() {
    // The JsonDocument contract: byte must not reformat a file it does not
    // own. Comparing raw bytes, not parsed values -- a parsed comparison is
    // blind to key order, which is exactly what a naive rewrite destroys.
    let tmp = tempfile::tempdir().unwrap();
    let p = write_config(tmp.path());
    let backups = tmp.path().join("backups");
    let before = std::fs::read_to_string(&p).unwrap();

    let captured = capture(&p).unwrap();
    apply(&p, &captured, &backups).unwrap();

    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        before,
        "a capture/apply round trip must be a no-op on the file"
    );
}

#[test]
fn clear_removes_all_three_and_leaves_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_config(tmp.path());
    let backups = tmp.path().join("backups");

    clear(&p, &backups).unwrap();

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert!(after.get("oauth:tokenCache").is_none());
    assert!(after.get("oauth:tokenCacheV2").is_none());
    assert!(after.get("lastKnownAccountUuid").is_none());
    assert_eq!(after["locale"], json!("en-GB"));
}

#[test]
fn capturing_a_config_without_the_keys_yields_nothing_rather_than_failing() {
    // A fresh install, or one that has never signed in.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("config.json");
    std::fs::write(&p, br#"{"locale":"en-GB"}"#).unwrap();

    let got = capture(&p).unwrap();

    assert_eq!(got.token_cache, None);
    assert_eq!(got.account_uuid, None);
}
