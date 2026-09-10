use byte::claude::detect::FakeProbe;
use byte::claude::snapshot::AccountSnapshot;
use byte::desktop::paths::{DesktopPaths, TestDesktopPaths};
use byte::ops::desktop::{DesktopOutcome, switch_desktop};
use byte::paths::{HostPaths, TestPaths};

/// Mirrors `snap` in `tests/metadata_test.rs`, minus the email parameter:
/// callers here only ever need a resolvable uuid, not a distinct identity.
fn sample_snapshot(uuid: &str) -> AccountSnapshot {
    AccountSnapshot::new(
        serde_json::json!({"refreshToken": "r", "subscriptionType": "max"}),
        serde_json::json!({
            "accountUuid": uuid,
            "emailAddress": format!("{uuid}@example.com"),
            "organizationName": "Org"
        }),
        Some("uid".to_string()),
    )
}

fn seed_live(d: &TestDesktopPaths, marker: &str) {
    let dir = d.desktop_dir().join("Network");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("marker.txt"), marker).unwrap();
    std::fs::write(
        d.config_file(),
        serde_json::json!({"lastKnownAccountUuid": marker, "locale": "en-GB"}).to_string(),
    )
    .unwrap();
}

#[test]
fn a_running_app_blocks_the_desktop_half_and_changes_nothing() {
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live(&dp, "account-a");

    let out = switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, true), Some("a"), "b").unwrap();

    assert_eq!(out, DesktopOutcome::AppRunning);
    assert_eq!(
        std::fs::read_to_string(dp.desktop_dir().join("Network/marker.txt")).unwrap(),
        "account-a",
        "nothing may move while the app holds the profile open"
    );
    assert!(!tp.desktop_profile_dir("a").exists());
}

#[test]
fn switching_to_an_uncaptured_account_parks_the_old_one_and_leaves_the_app_signed_out() {
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live(&dp, "account-a");

    let out = switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("a"), "b").unwrap();

    assert_eq!(out, DesktopOutcome::NoProfileForIncoming);
    // Parked, not discarded -- switching away must never lose a session.
    assert_eq!(
        std::fs::read_to_string(tp.desktop_profile_dir("a").join("Network/marker.txt")).unwrap(),
        "account-a"
    );
    assert!(!dp.desktop_dir().join("Network").exists());
    // And the app's identity is cleared, not left naming account a.
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dp.config_file()).unwrap()).unwrap();
    assert!(cfg.get("lastKnownAccountUuid").is_none());
    assert_eq!(cfg["locale"], serde_json::json!("en-GB"));
}

#[test]
fn switching_to_a_captured_account_restores_its_session() {
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live(&dp, "account-a");
    let stored = tp.desktop_profile_dir("b").join("Network");
    std::fs::create_dir_all(&stored).unwrap();
    std::fs::write(stored.join("marker.txt"), "account-b").unwrap();

    let out = switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("a"), "b").unwrap();

    assert_eq!(out, DesktopOutcome::Switched);
    assert_eq!(
        std::fs::read_to_string(dp.desktop_dir().join("Network/marker.txt")).unwrap(),
        "account-b"
    );
    assert_eq!(
        std::fs::read_to_string(tp.desktop_profile_dir("a").join("Network/marker.txt")).unwrap(),
        "account-a"
    );
}

#[test]
fn a_round_trip_returns_the_original_session_intact() {
    // Park A, switch to B, switch back: A's tree must be exactly what it was.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live(&dp, "account-a");
    let probe = FakeProbe::with_desktop(0, false);

    switch_desktop(&tp, &dp, &probe, Some("a"), "b").unwrap();
    // Simulate signing in as b.
    let dir = dp.desktop_dir().join("Network");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("marker.txt"), "account-b").unwrap();

    switch_desktop(&tp, &dp, &probe, Some("b"), "a").unwrap();

    assert_eq!(
        std::fs::read_to_string(dp.desktop_dir().join("Network/marker.txt")).unwrap(),
        "account-a"
    );
}

#[test]
fn the_outgoing_accounts_oauth_is_parked_with_its_profile() {
    // Captured BEFORE the swap: config.json is about to be rewritten with
    // the incoming account's values, so a capture afterwards reads the wrong
    // account. An implementation that captures after `swap::execute` files
    // the WRONG uuid here, and one that skips the capture writes no file.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live(&dp, "account-a");

    switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("a"), "b").unwrap();

    let parked = tp.desktop_profile_dir("a").join("oauth.json");
    let got: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&parked).unwrap()).unwrap();
    assert_eq!(
        got["account_uuid"],
        serde_json::json!("account-a"),
        "the parked oauth must be the OUTGOING account's, not the incoming one's"
    );
}

#[test]
fn parking_a_profile_records_it_against_the_account() {
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live(&dp, "account-a");
    // An accounts.json holding the outgoing account must exist for the
    // record to land on.
    let mut accounts = byte::store::metadata::AccountsFile::default();
    accounts.upsert_from("a", &sample_snapshot("a"));
    accounts
        .save(&tp.accounts_file(), &tp.backup_dir())
        .unwrap();

    switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("a"), "b").unwrap();

    let back = byte::store::metadata::AccountsFile::load(&tp.accounts_file()).unwrap();
    let rec = back.resolve("a").unwrap().desktop_profile.clone().unwrap();
    assert!(
        rec.bytes > 0,
        "a parked profile with files in it must record a nonzero size"
    );
    assert!(!rec.captured_at.is_empty());
}
