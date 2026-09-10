use byte::claude::detect::FakeProbe;
use byte::claude::snapshot::AccountSnapshot;
use byte::desktop::journal::Journal;
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

/// A live desktop session signed in as `uuid`, with `account-<uuid>` written
/// into the profile tree so a later assertion can tell whose session moved.
///
/// The uuid written into `config.json` is the SAME string the caller passes
/// to `switch_desktop` as `outgoing`. That is not cosmetic: byte now refuses
/// to park a live session whose `lastKnownAccountUuid` names a different
/// account than the one it is filing it under, so a fixture that signs the
/// app in as `account-a` while calling the account `a` is modelling the
/// desynchronised state, not the ordinary one.
fn seed_live(d: &TestDesktopPaths, uuid: &str) {
    let dir = d.desktop_dir().join("Network");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("marker.txt"), format!("account-{uuid}")).unwrap();
    std::fs::write(
        d.config_file(),
        serde_json::json!({"lastKnownAccountUuid": uuid, "locale": "en-GB"}).to_string(),
    )
    .unwrap();
}

#[test]
fn a_running_app_blocks_the_desktop_half_and_changes_nothing() {
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live(&dp, "a");

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
    seed_live(&dp, "a");

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
    seed_live(&dp, "a");
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
    seed_live(&dp, "a");
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
    seed_live(&dp, "a");

    switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("a"), "b").unwrap();

    let parked = tp.desktop_profile_dir("a").join("oauth.json");
    let got: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&parked).unwrap()).unwrap();
    assert_eq!(
        got["account_uuid"],
        serde_json::json!("a"),
        "the parked oauth must be the OUTGOING account's, not the incoming one's"
    );
}

#[test]
fn a_second_switch_into_a_previously_parked_account_restores_its_real_oauth_not_defaults() {
    // Regression: `oauth.json` (byte's own capture, written directly inside
    // the profile directory by this module) did not exist when
    // `desktop::profile`'s denylist was written, so `movable_entries`
    // classified it as ordinary account state. Left unfixed, installing
    // FROM a profile that has already been parked once (so it holds both a
    // moved directory and its `oauth.json` companion) would sweep
    // `oauth.json` into the live desktop directory along with everything
    // else -- littering an app directory Claude Desktop has never heard of
    // AND deleting the very file this function reads back afterwards,
    // silently replacing the incoming account's real oauth with
    // `DesktopOauth::default()` instead of restoring it.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();

    // "a" is live, fully signed in with a real token cache.
    let dir = dp.desktop_dir().join("Network");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("marker.txt"), "account-a").unwrap();
    std::fs::write(
        dp.config_file(),
        serde_json::json!({
            "lastKnownAccountUuid": "a",
            "oauth:tokenCache": {"accessToken": "a-token"},
            "locale": "en-GB"
        })
        .to_string(),
    )
    .unwrap();
    let probe = FakeProbe::with_desktop(0, false);

    // Switch away to "b" (uncaptured): this parks "a", writing its
    // oauth.json alongside the freshly moved "Network" directory -- the
    // exact shape a previously-parked profile has.
    switch_desktop(&tp, &dp, &probe, Some("a"), "b").unwrap();
    assert!(tp.desktop_profile_dir("a").join("oauth.json").exists());

    // Now switch back to "a". Its stored profile has both "Network" AND
    // "oauth.json" sitting side by side -- the scenario the denylist never
    // saw when it was written.
    switch_desktop(&tp, &dp, &probe, Some("b"), "a").unwrap();

    assert!(
        !dp.desktop_dir().join("oauth.json").exists(),
        "byte's own oauth capture file must never be installed into the app's live directory"
    );
    assert!(
        tp.desktop_profile_dir("a").join("oauth.json").exists(),
        "it must stay behind in the store, available for a future switch"
    );
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dp.config_file()).unwrap()).unwrap();
    assert_eq!(
        cfg["oauth:tokenCache"],
        serde_json::json!({"accessToken": "a-token"}),
        "the real captured token cache must be restored, not silently defaulted"
    );
}

#[test]
fn parking_a_profile_records_it_against_the_account() {
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live(&dp, "a");
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

#[test]
fn switching_an_account_to_itself_changes_nothing() {
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live(&dp, "a");

    let out = switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("a"), "a").unwrap();

    assert_eq!(out, DesktopOutcome::NothingToDo);
    // Live directory marker file must be unchanged.
    assert_eq!(
        std::fs::read_to_string(dp.desktop_dir().join("Network/marker.txt")).unwrap(),
        "account-a",
        "self-switch must not move the live profile"
    );
    // Config's lastKnownAccountUuid must still be present and unchanged.
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dp.config_file()).unwrap()).unwrap();
    assert_eq!(
        cfg["lastKnownAccountUuid"],
        serde_json::json!("a"),
        "self-switch must not clear the OAuth keys"
    );
    // No journal file should be created. The journal lives under HostPaths
    // (`<byte_config_dir>/desktop/journal.json`), a different temp root from
    // the desktop directory entirely -- an assertion aimed at the desktop
    // root, or at a filename byte never writes, could never have failed.
    assert!(!tp.desktop_journal_file().exists());
}

#[test]
fn a_self_switch_is_a_no_op_even_when_a_profile_is_already_stored() {
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live(&dp, "a");
    // Pre-create a stored profile for account "a".
    let stored = tp.desktop_profile_dir("a").join("Network");
    std::fs::create_dir_all(&stored).unwrap();
    std::fs::write(stored.join("marker.txt"), "account-a-stored").unwrap();

    let out = switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("a"), "a").unwrap();

    assert_eq!(out, DesktopOutcome::NothingToDo);
    // Live directory must remain unchanged.
    assert_eq!(
        std::fs::read_to_string(dp.desktop_dir().join("Network/marker.txt")).unwrap(),
        "account-a"
    );
    // Stored profile must remain untouched.
    assert_eq!(
        std::fs::read_to_string(tp.desktop_profile_dir("a").join("Network/marker.txt")).unwrap(),
        "account-a-stored",
        "stored profile must not be touched by a self-switch"
    );
    // Config must be unchanged.
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dp.config_file()).unwrap()).unwrap();
    assert_eq!(cfg["lastKnownAccountUuid"], serde_json::json!("a"));
}

#[test]
fn a_live_session_belonging_to_another_account_is_not_parked() {
    // Review finding: the tray switches Claude Code without switching the
    // desktop half at all, so the two can drift apart -- and `outgoing` is
    // derived from sync-back, i.e. from whichever account Claude CODE was
    // on. Concretely: the tray switches Code a -> b while Claude Desktop
    // still holds a's session and `config.json` still names a; a later `byte
    // switch c` from the CLI reports b as outgoing. Parking then writes a's
    // token cache to `<store>/b/oauth.json` and moves a's profile
    // directories under b's uuid, so a later `byte switch b` installs a's
    // session and applies a's identity as b.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live(&dp, "a");
    let live_config = std::fs::read_to_string(dp.config_file()).unwrap();

    let out = switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("b"), "c").unwrap();

    assert_eq!(out, DesktopOutcome::IdentityMismatch);
    assert!(
        !tp.desktop_profile_dir("b").exists(),
        "a's live session must not be filed under b's uuid -- not even its oauth.json"
    );
    assert_eq!(
        std::fs::read_to_string(dp.desktop_dir().join("Network/marker.txt")).unwrap(),
        "account-a",
        "nothing may move when the live session belongs to another account"
    );
    assert_eq!(
        std::fs::read_to_string(dp.config_file()).unwrap(),
        live_config,
        "the refusal must be byte-for-byte side-effect-free, config.json included"
    );
    assert!(
        !tp.desktop_journal_file().exists(),
        "no swap may have been planned, let alone started"
    );
}

#[test]
fn a_matching_live_session_is_still_parked_normally() {
    // The guard must refuse only the desynchronised case: when the app's
    // `lastKnownAccountUuid` IS the outgoing account, the ordinary park
    // still happens.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live(&dp, "a");

    let out = switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("a"), "b").unwrap();

    assert_eq!(out, DesktopOutcome::NoProfileForIncoming);
    assert_eq!(
        std::fs::read_to_string(tp.desktop_profile_dir("a").join("Network/marker.txt")).unwrap(),
        "account-a"
    );
}

#[test]
fn an_absent_account_uuid_is_not_a_mismatch_and_still_parks() {
    // A desktop app that has never been signed in (or was signed out) has no
    // `lastKnownAccountUuid` at all. That is not a misfiling risk -- there is
    // no session to file under the wrong account -- and refusing here would
    // break the first switch on every fresh machine, so the park proceeds.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    let dir = dp.desktop_dir().join("Network");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("marker.txt"), "signed-out").unwrap();
    std::fs::write(
        dp.config_file(),
        serde_json::json!({"locale": "en-GB"}).to_string(),
    )
    .unwrap();

    let out = switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("a"), "b").unwrap();

    assert_eq!(out, DesktopOutcome::NoProfileForIncoming);
    assert_eq!(
        std::fs::read_to_string(tp.desktop_profile_dir("a").join("Network/marker.txt")).unwrap(),
        "signed-out",
        "an unidentified live profile is still parked, not stranded in the live directory"
    );

    // This is the ONE fixture where the outgoing account's name and the live
    // `lastKnownAccountUuid` differ, so it is the only place that can tell a
    // parked identity READ from `config.json` apart from one synthesised
    // from the `outgoing` parameter. There was no session, so byte must
    // record that there was none -- inventing `"a"` here would make a later
    // switch back into "a" stamp an identity the app never had over a
    // profile that holds no token to match it.
    let parked: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tp.desktop_profile_dir("a").join("oauth.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        parked["account_uuid"],
        serde_json::Value::Null,
        "the parked identity must be what config.json said, not the outgoing account's name"
    );
}

/// A realistic unrepaired journal: one park that already completed, with the
/// swap killed before anything was installed. Only its presence matters to
/// the guard under test, but a hand-authored file that could never have been
/// written by `swap::execute` would prove less.
fn seed_interrupted_journal(tp: &TestPaths, live: &std::path::Path, outgoing: &str) {
    std::fs::create_dir_all(tp.desktop_store_dir()).unwrap();
    std::fs::write(
        tp.desktop_journal_file(),
        serde_json::json!({
            "version": 1,
            "outgoing": outgoing,
            "incoming": null,
            "moves": [{
                "stage": "Park",
                "from": live.join("Network"),
                "to": tp.desktop_profile_dir(outgoing).join("Network"),
                "done": true
            }]
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
fn an_unrepaired_journal_refuses_the_switch_without_touching_the_parked_oauth() {
    // `swap::execute` already refuses to start a swap over an unrepaired
    // journal -- but by the time it does, the outgoing account's parked
    // `oauth.json` has already been overwritten, and that file is not
    // journalled, so no recovery ever repairs it.
    //
    // The scenario: `switch a -> b` was interrupted, so the journal survives
    // and `config.json` still holds a's keys. The next switch names the
    // now-active account as outgoing -- `switch b -> c`. A capture that runs
    // before the refusal reads A's keys out of the live `config.json` and
    // files them as B's parked oauth, destroying b's genuine copy for good.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live(&dp, "a");
    seed_interrupted_journal(&tp, &dp.desktop_dir(), "a");

    // b's genuine parked identity, from the last switch that did complete.
    let store_b = tp.desktop_profile_dir("b");
    std::fs::create_dir_all(&store_b).unwrap();
    let genuine = serde_json::json!({
        "token_cache": {"accessToken": "b-token"},
        "token_cache_v2": null,
        "account_uuid": "account-b"
    })
    .to_string();
    std::fs::write(store_b.join("oauth.json"), &genuine).unwrap();

    let err = switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("b"), "c")
        .expect_err("a swap must not be started over an unrepaired journal");

    assert!(
        matches!(err, byte::Error::DesktopSwapInterrupted { .. }),
        "expected the interrupted-swap refusal, got: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(store_b.join("oauth.json")).unwrap(),
        genuine,
        "a refused switch must be side-effect-free: b's parked oauth must be byte-for-byte \
         what it was, not the outgoing config.json's stale keys"
    );
}

#[test]
fn the_journal_records_which_two_accounts_the_swap_is_between() {
    // A completed swap deletes its own journal, so the only seam that can
    // read these fields back is an `execute` that fails: a colliding park
    // destination leaves the journal exactly as it was written, before the
    // first rename.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live(&dp, "a");

    // A stored profile for the incoming account, so `incoming` is Some.
    let stored_b = tp.desktop_profile_dir("b").join("Network");
    std::fs::create_dir_all(&stored_b).unwrap();
    std::fs::write(stored_b.join("marker.txt"), "account-b").unwrap();

    // A non-empty directory already sitting where the park must land.
    let collision = tp.desktop_profile_dir("a").join("Network");
    std::fs::create_dir_all(&collision).unwrap();
    std::fs::write(collision.join("marker.txt"), "stale").unwrap();

    let err = switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("a"), "b")
        .expect_err("the park rename must collide with the occupied destination");
    assert!(
        matches!(err, byte::Error::Io { .. }),
        "expected the rename collision, got: {err}"
    );

    let journal = Journal::from_bytes(&std::fs::read(tp.desktop_journal_file()).unwrap()).unwrap();
    assert_eq!(
        journal.outgoing.as_deref(),
        Some("a"),
        "a recovery hitting this journal must be able to name the account being parked"
    );
    assert_eq!(
        journal.incoming.as_deref(),
        Some("b"),
        "...and the account being installed"
    );
}

#[test]
fn a_committed_switch_survives_an_accounts_file_it_cannot_read() {
    // The `desktop_profile` record only feeds `byte list`. By the time it is
    // written the renames are committed and the journal is cleared, so a `?`
    // here would report a failed switch over a swap that fully happened --
    // and, worse, would skip the config patch, leaving `config.json` naming
    // the OUTGOING account over the incoming account's live directory. No
    // journal exists by then, so nothing repairs that.
    //
    // `AccountsFile::load` fails on ordinary conditions, not just crashes:
    // malformed JSON reports `Parse`, an outdated file reports
    // `AccountsSchemaMismatch`.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live(&dp, "a");
    std::fs::write(tp.accounts_file(), b"{ not json at all").unwrap();

    let out = switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("a"), "b")
        .expect("a display-only bookkeeping failure must not fail an already-committed switch");

    assert_eq!(out, DesktopOutcome::NoProfileForIncoming);
    // The park committed...
    assert_eq!(
        std::fs::read_to_string(tp.desktop_profile_dir("a").join("Network/marker.txt")).unwrap(),
        "account-a"
    );
    // ...and, the half that matters, so did the config patch.
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dp.config_file()).unwrap()).unwrap();
    assert!(
        cfg.get("lastKnownAccountUuid").is_none(),
        "config.json must not still name the outgoing account over the incoming session"
    );
    assert_eq!(cfg["locale"], serde_json::json!("en-GB"));
}

/// Windows only, and deliberately so: `MoveFileExW` refuses to replace a
/// read-only destination, which is what makes `atomic::write` -- and with it
/// `AccountsFile::save` -- fail on a file that still loads perfectly. A POSIX
/// `rename` over a read-only file succeeds (only the directory's write bit
/// matters), so there is no equivalent seam there and a portable version of
/// this test would assert nothing on Unix. The feature under test is itself
/// Windows-only.
#[cfg(windows)]
#[test]
fn a_committed_switch_survives_an_accounts_file_it_cannot_write() {
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live(&dp, "a");

    let mut accounts = byte::store::metadata::AccountsFile::default();
    accounts.upsert_from("a", &sample_snapshot("a"));
    accounts
        .save(&tp.accounts_file(), &tp.backup_dir())
        .unwrap();
    set_readonly(&tp.accounts_file(), true);

    let out = switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("a"), "b")
        .expect("an unwritable bookkeeping file must not fail an already-committed switch");

    assert_eq!(out, DesktopOutcome::NoProfileForIncoming);
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dp.config_file()).unwrap()).unwrap();
    assert!(
        cfg.get("lastKnownAccountUuid").is_none(),
        "config.json must not still name the outgoing account over the incoming session"
    );

    // Leave nothing read-only behind: TempDir's own cleanup cannot remove a
    // read-only file on Windows, and `atomic::backup`'s copy carries the
    // attribute across to the backup it just made.
    set_readonly(&tp.accounts_file(), false);
    for entry in std::fs::read_dir(tp.backup_dir()).unwrap().flatten() {
        set_readonly(&entry.path(), false);
    }
}

#[cfg(windows)]
fn set_readonly(path: &std::path::Path, readonly: bool) {
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_readonly(readonly);
    std::fs::set_permissions(path, perms).unwrap();
}

#[test]
fn switching_into_an_account_whose_park_stored_nothing_reports_no_stored_profile() {
    // A park with no movable entries -- a fresh install, or a directory
    // holding only denylisted content -- still creates the store directory
    // and writes byte's own `oauth.json` into it. Deciding "is there a stored
    // profile" on the directory merely existing therefore believes a claim
    // this module's own write manufactured: it would apply an all-`None`
    // `DesktopOauth` (which `config::apply` treats as removal, i.e. exactly
    // what `clear` does) and report `Switched` -- telling the user their
    // desktop session was restored while signing them out.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    let probe = FakeProbe::with_desktop(0, false);
    // A fresh desktop install: config.json, and nothing that moves.
    std::fs::write(
        dp.config_file(),
        serde_json::json!({"locale": "en-GB"}).to_string(),
    )
    .unwrap();

    let out = switch_desktop(&tp, &dp, &probe, Some("a"), "b").unwrap();
    assert_eq!(out, DesktopOutcome::NoProfileForIncoming);
    assert!(
        tp.desktop_profile_dir("a").join("oauth.json").exists(),
        "the empty park still creates the store directory -- the state under test"
    );

    let out = switch_desktop(&tp, &dp, &probe, Some("b"), "a").unwrap();

    assert_eq!(
        out,
        DesktopOutcome::NoProfileForIncoming,
        "an empty park stores no session, so switching back into it restores nothing"
    );
}

// ---------------------------------------------------------------------------
// Review findings on the journal's boundary. The tests below pin the seam
// between the journalled directory renames and the `config.json` patch that
// used to sit outside them, and the two ways byte could destroy a stored
// credential while reporting success.
// ---------------------------------------------------------------------------

/// A live desktop session signed in as `uuid`, carrying a real token cache as
/// well as an identity -- the shape `seed_live` deliberately lacks, so a test
/// can tell a genuine capture from an all-`None` one.
fn seed_live_signed_in(d: &TestDesktopPaths, uuid: &str) {
    let dir = d.desktop_dir().join("Network");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("marker.txt"), format!("account-{uuid}")).unwrap();
    std::fs::write(
        d.config_file(),
        serde_json::json!({
            "lastKnownAccountUuid": uuid,
            "oauth:tokenCacheV2": {"accessToken": format!("{uuid}-token")},
            "locale": "en-GB"
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
fn an_all_none_capture_never_overwrites_a_parked_oauth_over_a_real_profile() {
    // The drift this reproduces is ordinary, not exotic:
    //
    // 1. Desktop and Code are both on "a". `byte switch b` (b unstored) parks
    //    a's directories into `<store>/a`, writes a's real keys to
    //    `<store>/a/oauth.json`, and `config::clear`s the live config --
    //    which REMOVES `lastKnownAccountUuid`. byte manufactures the exact
    //    state its own absent-uuid carve-out treats as harmless.
    // 2. A tray click switches Claude CODE back to "a". The tray never
    //    touches the desktop app, so it stays signed out and `<store>/a`
    //    stays full.
    // 3. `byte switch c` names "a" as outgoing. The capture reads all-`None`
    //    out of the cleared config; the identity guard sees no uuid and waves
    //    it through; the park overwrites `<store>/a/oauth.json` with
    //    `{null,null,null}` and a's real desktop token is gone for good.
    //
    // An EMPTY `<store>/a` is what makes an absent uuid mean "no session to
    // misfile". A full one makes it mean "the live directory is not the
    // session byte thinks it is".
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    let probe = FakeProbe::with_desktop(0, false);
    seed_live_signed_in(&dp, "a");

    // Step 1.
    switch_desktop(&tp, &dp, &probe, Some("a"), "b").unwrap();
    let parked = tp.desktop_profile_dir("a").join("oauth.json");
    let genuine = std::fs::read_to_string(&parked).unwrap();
    assert!(
        genuine.contains("a-token"),
        "the park must have stored a's real token to begin with, got: {genuine}"
    );
    // Step 2 needs no byte call at all -- that is the whole point.

    // Step 3.
    let out = switch_desktop(&tp, &dp, &probe, Some("a"), "c").unwrap();

    assert_eq!(
        out,
        DesktopOutcome::IdentityMismatch,
        "an unidentified live session over a full store is drift, not a green light"
    );
    assert_eq!(
        std::fs::read_to_string(&parked).unwrap(),
        genuine,
        "a's real desktop token must survive byte-for-byte, not be replaced by nulls"
    );
    assert_eq!(
        std::fs::read_to_string(tp.desktop_profile_dir("a").join("Network/marker.txt")).unwrap(),
        "account-a",
        "the refusal must touch nothing at all"
    );
    assert!(
        !tp.desktop_journal_file().exists(),
        "no swap may have been planned, let alone started"
    );
}

#[test]
fn an_all_none_capture_never_overwrites_parked_keys_that_have_no_directories() {
    // The other half of the same protection, and the reason it lives at the
    // write rather than only in the park-target decision. A park files two
    // things side by side, and can legitimately produce keys with no
    // directories: a live profile whose every entry is denylisted stores a
    // real `oauth.json` and moves nothing. The parked-directory check waves
    // that store through as "empty", so only a check on the keys themselves
    // stops an empty capture replacing them with `{null,null,null}` -- and
    // `oauth.json` is in no journal, so nothing would ever repair it.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    let store_a = tp.desktop_profile_dir("a");
    std::fs::create_dir_all(&store_a).unwrap();
    let genuine = serde_json::json!({
        "oauth": {"oauth:tokenCacheV2": {"accessToken": "a-token"}},
        "account_uuid": "a"
    })
    .to_string();
    std::fs::write(store_a.join("oauth.json"), &genuine).unwrap();
    assert!(
        !store_a.join("Network").exists(),
        "the store under test holds keys and no directories"
    );

    // A signed-out live app: nothing to capture.
    std::fs::write(
        dp.config_file(),
        serde_json::json!({"locale": "en-GB"}).to_string(),
    )
    .unwrap();

    let out = switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("a"), "b").unwrap();

    assert_eq!(out, DesktopOutcome::IdentityMismatch);
    assert_eq!(
        std::fs::read_to_string(store_a.join("oauth.json")).unwrap(),
        genuine,
        "a's parked keys must survive byte-for-byte"
    );
}

#[test]
fn a_logged_out_claude_code_does_not_blend_the_live_desktop_session_into_the_incoming_one() {
    // `desktop_half` maps `SyncOutcome::LoggedOut` to `outgoing: None`, so
    // the identity guard -- which only evaluates when BOTH an outgoing uuid
    // and a capture are present -- never runs, and no park is planned. The
    // live directory's contents stay in place while the incoming profile's
    // install moves land beside them.
    //
    // The entry sets here are deliberately DISJOINT (live holds "Network",
    // the stored profile holds "IndexedDB"), because that is the silent
    // version: overlapping names collide and at least produce an error.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live_signed_in(&dp, "a");
    let stored = tp.desktop_profile_dir("b").join("IndexedDB");
    std::fs::create_dir_all(&stored).unwrap();
    std::fs::write(stored.join("marker.txt"), "account-b").unwrap();

    let out = switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), None, "b").unwrap();

    assert_eq!(out, DesktopOutcome::Switched);
    assert!(
        !dp.desktop_dir().join("Network").exists(),
        "a's cookies must not be left underneath b's freshly stamped identity"
    );
    assert_eq!(
        std::fs::read_to_string(tp.desktop_profile_dir("a").join("Network/marker.txt")).unwrap(),
        "account-a",
        "byte knows whose session this is from config.json; it must park it under that uuid"
    );
    assert_eq!(
        std::fs::read_to_string(dp.desktop_dir().join("IndexedDB/marker.txt")).unwrap(),
        "account-b"
    );
    let parked: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tp.desktop_profile_dir("a").join("oauth.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        parked["account_uuid"],
        serde_json::json!("a"),
        "a's OAuth keys must be parked with a's profile, not discarded"
    );
}

#[test]
fn a_corrupt_stored_oauth_does_not_report_a_plain_switch() {
    // `serde_json::from_slice(..).unwrap_or_default()` turns an unreadable
    // stored capture into `DesktopOauth::default()`, which `config::apply`
    // treats as removal -- so byte installs the account's cookies and then
    // signs the app out, while telling the user "Claude desktop app switched
    // too."
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live_signed_in(&dp, "a");
    let store_b = tp.desktop_profile_dir("b");
    std::fs::create_dir_all(store_b.join("Network")).unwrap();
    std::fs::write(store_b.join("Network/marker.txt"), "account-b").unwrap();
    std::fs::write(store_b.join("oauth.json"), b"{ truncated mid-writ").unwrap();

    let out = switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("a"), "b").unwrap();

    assert_eq!(
        out,
        DesktopOutcome::SwitchedWithoutIdentity,
        "the swap committed but the app is signed out; reporting a plain switch is a lie"
    );
    // The directories still moved -- a reporting detail must never undo a
    // committed swap.
    assert_eq!(
        std::fs::read_to_string(dp.desktop_dir().join("Network/marker.txt")).unwrap(),
        "account-b"
    );
    // ...and the outgoing account's keys are gone rather than left behind
    // for b's cookies to authenticate with.
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dp.config_file()).unwrap()).unwrap();
    assert!(cfg.get("lastKnownAccountUuid").is_none());
    assert!(cfg.get("oauth:tokenCacheV2").is_none());
    assert!(
        !tp.desktop_journal_file().exists(),
        "the swap is as finished as it can be; rereading the same corrupt file will not help"
    );
}

#[test]
fn a_config_patch_that_fails_leaves_the_journal_for_the_next_command() {
    // `config::apply` fails on entirely ordinary conditions --
    // `JsonDocument::save` backs the file up first, which is where this
    // codebase already meets "Access is denied (os error 5)". With the patch
    // outside the journal's commit boundary the journal is cleared BEFORE it
    // runs, so the error propagates over roughly sixteen already-moved
    // directories with no record left of them: the app authenticates from the
    // outgoing account's token cache over the incoming account's cookie jar,
    // and nothing repairs it.
    //
    // The failure is injected portably by replacing the backup DIRECTORY with
    // a file, which is what `atomic::backup`'s `create_dir_all` trips over --
    // and nothing before the patch writes through it.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live_signed_in(&dp, "a");
    let stored = tp.desktop_profile_dir("b").join("Network");
    std::fs::create_dir_all(&stored).unwrap();
    std::fs::write(stored.join("marker.txt"), "account-b").unwrap();

    std::fs::remove_dir(tp.backup_dir()).unwrap();
    std::fs::write(tp.backup_dir(), b"not a directory").unwrap();

    let err = switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("a"), "b")
        .expect_err("an unwritable backup directory must fail the config patch");
    assert!(
        matches!(err, byte::Error::Io { .. }),
        "expected the backup directory's io error, got: {err}"
    );

    let journal = Journal::from_bytes(
        &std::fs::read(tp.desktop_journal_file())
            .expect("the journal must survive an unfinished patch so the next command retries it"),
    )
    .unwrap();
    assert!(
        journal.is_complete(),
        "every rename landed; what is outstanding is the identity patch"
    );
    assert_eq!(journal.incoming.as_deref(), Some("b"));
}

#[test]
fn a_completed_switch_clears_the_journal_only_after_the_identity_lands() {
    // The commit boundary, stated from the outside: when `switch_desktop`
    // returns `Switched`, both halves are done and no journal is left for the
    // next command to "repair" a finished swap with.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    seed_live_signed_in(&dp, "a");
    let stored = tp.desktop_profile_dir("b").join("Network");
    std::fs::create_dir_all(&stored).unwrap();
    std::fs::write(stored.join("marker.txt"), "account-b").unwrap();
    // b's stored identity, in the legacy on-disk shape an existing install
    // would already have on disk.
    std::fs::write(
        tp.desktop_profile_dir("b").join("oauth.json"),
        serde_json::json!({
            "token_cache": null,
            "token_cache_v2": {"accessToken": "b-token"},
            "account_uuid": "b"
        })
        .to_string(),
    )
    .unwrap();

    let out = switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("a"), "b").unwrap();

    assert_eq!(out, DesktopOutcome::Switched);
    assert!(!tp.desktop_journal_file().exists());
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dp.config_file()).unwrap()).unwrap();
    assert_eq!(cfg["lastKnownAccountUuid"], serde_json::json!("b"));
    assert_eq!(
        cfg["oauth:tokenCacheV2"],
        serde_json::json!({"accessToken": "b-token"}),
        "an oauth.json written by the previous format must still load"
    );
}

#[test]
fn an_unrecognised_oauth_key_travels_with_the_account_through_a_real_switch() {
    // `oauth:tokenCache` -> `oauth:tokenCacheV2` already happened once; the
    // next rename is not byte's to predict. A hardcoded allowlist leaves the
    // OUTGOING account's newer cache sitting in `config.json` underneath the
    // incoming account's identity -- nothing strands, it leaks.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    let probe = FakeProbe::with_desktop(0, false);
    std::fs::create_dir_all(dp.desktop_dir().join("Network")).unwrap();
    std::fs::write(dp.desktop_dir().join("Network/marker.txt"), "account-a").unwrap();
    std::fs::write(
        dp.config_file(),
        serde_json::json!({
            "lastKnownAccountUuid": "a",
            "oauth:tokenCacheV3": {"accessToken": "a-v3"},
            "locale": "en-GB"
        })
        .to_string(),
    )
    .unwrap();

    // Park a, leaving the app signed out for b.
    switch_desktop(&tp, &dp, &probe, Some("a"), "b").unwrap();
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dp.config_file()).unwrap()).unwrap();
    assert!(
        cfg.get("oauth:tokenCacheV3").is_none(),
        "a's unrecognised cache must not be left behind for b to authenticate from"
    );

    // Sign in as b, then switch back to a.
    std::fs::create_dir_all(dp.desktop_dir().join("Network")).unwrap();
    std::fs::write(dp.desktop_dir().join("Network/marker.txt"), "account-b").unwrap();
    switch_desktop(&tp, &dp, &probe, Some("b"), "a").unwrap();

    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dp.config_file()).unwrap()).unwrap();
    assert_eq!(
        cfg["oauth:tokenCacheV3"],
        serde_json::json!({"accessToken": "a-v3"}),
        "a's unrecognised cache must come back with a's profile"
    );
    assert_eq!(cfg["locale"], serde_json::json!("en-GB"));
}
