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
fn capture_takes_every_oauth_key_and_the_account_uuid() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_config(tmp.path());

    let got = capture(&p).unwrap();

    assert_eq!(got.oauth["oauth:tokenCache"], json!({"access": "aaa"}));
    assert_eq!(got.oauth["oauth:tokenCacheV2"], json!({"access": "bbb"}));
    assert_eq!(got.account_uuid, Some(json!("uuid-a")));
    // The user's own settings are not byte's to carry between accounts.
    assert!(!got.oauth.contains_key("locale"));
    assert!(!got.oauth.contains_key("userThemeMode"));
}

#[test]
fn apply_changes_the_owned_keys_and_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_config(tmp.path());
    let backups = tmp.path().join("backups");

    let mut oauth = serde_json::Map::new();
    oauth.insert("oauth:tokenCache".to_string(), json!({"access": "zzz"}));
    apply(
        &p,
        &DesktopOauth {
            oauth,
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

    assert!(got.is_empty(), "a config with no session yields no keys");
    assert_eq!(got.account_uuid, None);
}

#[test]
fn an_unrecognised_oauth_key_is_captured_and_restored_with_the_account() {
    // `desktop::profile`'s module doc argues at length that byte must not
    // assume it knows the full set of account state -- and then `config.json`
    // used a hardcoded three-key allowlist. The version-suffixed names are
    // the tell: `oauth:tokenCache` -> `oauth:tokenCacheV2` already happened
    // once. When `oauth:tokenCacheV3` ships, an allowlist writes the incoming
    // account's V1/V2/uuid and leaves the OUTGOING account's V3 in place, so
    // the app may authenticate as the previous account under the new
    // account's identity. Unlike the directory side, nothing strands: it
    // leaks.
    let tmp = tempfile::tempdir().unwrap();
    let backups = tmp.path().join("backups");
    let from = tmp.path().join("from.json");
    std::fs::write(
        &from,
        json!({
            "locale": "en-GB",
            "oauth:tokenCacheV3": {"access": "future"},
            "lastKnownAccountUuid": "uuid-a"
        })
        .to_string(),
    )
    .unwrap();

    let captured = capture(&from).unwrap();

    let onto = tmp.path().join("onto.json");
    std::fs::write(&onto, json!({"userThemeMode": "dark"}).to_string()).unwrap();
    apply(&onto, &captured, &backups).unwrap();

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&onto).unwrap()).unwrap();
    assert_eq!(
        after["oauth:tokenCacheV3"],
        json!({"access": "future"}),
        "a key byte has never heard of must travel with the account that owns it"
    );
    assert_eq!(after["lastKnownAccountUuid"], json!("uuid-a"));
    assert_eq!(after["userThemeMode"], json!("dark"));
}

#[test]
fn an_unrecognised_oauth_key_is_removed_when_the_incoming_account_lacks_it() {
    // The other direction, and the one that leaks: switching INTO an account
    // that has no `oauth:tokenCacheV3` must remove the outgoing account's,
    // exactly as `apply` already does for the keys byte does know.
    let tmp = tempfile::tempdir().unwrap();
    let backups = tmp.path().join("backups");
    let p = tmp.path().join("config.json");
    std::fs::write(
        &p,
        json!({
            "locale": "en-GB",
            "oauth:tokenCacheV3": {"access": "outgoing"},
            "lastKnownAccountUuid": "uuid-a"
        })
        .to_string(),
    )
    .unwrap();

    clear(&p, &backups).unwrap();

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert!(
        after.get("oauth:tokenCacheV3").is_none(),
        "the outgoing account's unrecognised cache must not survive under another identity"
    );
    assert_eq!(after["locale"], json!("en-GB"));
}

#[test]
fn an_oauth_json_written_by_the_previous_format_still_loads() {
    // `DesktopOauth`'s serialised shape changed when the allowlist became a
    // prefix set, but profiles parked by the earlier build are on real disks
    // and must keep restoring their sessions rather than silently signing the
    // user out.
    let legacy = json!({
        "token_cache": {"access": "aaa"},
        "token_cache_v2": {"access": "bbb"},
        "account_uuid": "uuid-a"
    })
    .to_string();

    let got: DesktopOauth = serde_json::from_str(&legacy).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("config.json");
    std::fs::write(&p, json!({"locale": "en-GB"}).to_string()).unwrap();
    apply(&p, &got, &tmp.path().join("backups")).unwrap();

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(after["oauth:tokenCache"], json!({"access": "aaa"}));
    assert_eq!(after["oauth:tokenCacheV2"], json!({"access": "bbb"}));
    assert_eq!(after["lastKnownAccountUuid"], json!("uuid-a"));
}

#[test]
fn a_parked_capture_serialises_its_account_uuid_even_when_there_is_none() {
    // `ops::desktop` writes this file, and `tests/ops_desktop_test.rs` reads
    // `account_uuid` back out of it to prove the parked identity was read
    // from `config.json` rather than synthesised from the outgoing account's
    // name. That assertion only has teeth while the key is always present.
    let text = serde_json::to_string(&DesktopOauth::default()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();

    assert_eq!(value["account_uuid"], serde_json::Value::Null);
}
