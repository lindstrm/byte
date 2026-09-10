use byte::claude::files::ClaudeFiles;
use byte::claude::snapshot::{AccountSnapshot, SCHEMA_VERSION};
use byte::ops::switch::{SwitchOutcome, Switcher, SyncOutcome};
use byte::paths::{HostPaths, TestPaths};
use byte::store::metadata::{AccountMeta, AccountsFile};
use byte::store::secrets::{MemoryStore, SecretStore};
use serde_json::json;

fn login_as(tp: &TestPaths, uuid: &str, email: &str, refresh: &str) {
    std::fs::write(
        tp.claude_credentials(),
        serde_json::to_string(&json!({
            "mcpOAuth": {"srv": {"accessToken": "keep"}},
            "claudeAiOauth": {
                "accessToken": "a", "refreshToken": refresh,
                "expiresAt": 1i64, "subscriptionType": "max"
            }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        tp.claude_config(),
        serde_json::to_string_pretty(&json!({
            "numStartups": 7,
            "oauthAccount": {"accountUuid": uuid, "emailAddress": email},
            "userID": format!("uid-{uuid}")
        }))
        .unwrap(),
    )
    .unwrap();
}

fn live_refresh(tp: &TestPaths) -> String {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tp.claude_credentials()).unwrap()).unwrap();
    v["claudeAiOauth"]["refreshToken"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn capture_current_stores_the_live_account() {
    let tp = TestPaths::new().unwrap();
    login_as(&tp, "u1", "a@example.com", "r1");
    let sw = Switcher::new(&tp, MemoryStore::new());

    let meta = sw.capture_current().unwrap();

    assert_eq!(meta.uuid, "u1");
    let file = AccountsFile::load(&tp.accounts_file()).unwrap();
    assert_eq!(file.accounts.len(), 1);
    assert_eq!(file.active.as_deref(), Some("u1"));
    // The method's whole point is landing the secret in the store, not just
    // the metadata.
    assert_eq!(
        sw.secrets().get("u1").unwrap().unwrap()["refreshToken"],
        json!("r1")
    );
}

#[test]
fn capture_current_stores_only_the_oauth_block_in_the_secret_store() {
    // The keychain-size fix's core wiring, exercised through the real
    // production call path (not a hand-built Value, the way the payload
    // size guard in tests/secrets_test.rs necessarily works, since it can
    // never call the real KeyringStore::put against a real keychain):
    // capture_current must pass only `snapshot.oauth` to the secret store,
    // never the account block or user_id. A regression that merged them
    // back in at this call site -- as opposed to inside
    // KeyringStore::serialize_payload itself -- would slip past every test
    // in tests/secrets_test.rs, which never goes through `Switcher`.
    let tp = TestPaths::new().unwrap();
    login_as(&tp, "u1", "a@example.com", "r1");
    let sw = Switcher::new(&tp, MemoryStore::new());

    sw.capture_current().unwrap();

    let stored = sw.secrets().get("u1").unwrap().unwrap();
    assert!(
        stored.get("accountUuid").is_none(),
        "the secret store must never receive account fields: {stored}"
    );
    assert!(
        stored.get("emailAddress").is_none(),
        "the secret store must never receive account fields: {stored}"
    );
    // The two checks above alone are narrower than this test's name: a
    // regression to the original bug's exact shape -- put(&uuid,
    // &serde_json::to_value(&snapshot)) instead of put(&uuid,
    // &snapshot.oauth) -- nests accountUuid/emailAddress under a top-level
    // "account" key rather than exposing them directly, so neither check
    // above would fire. Check for the wrapper keys themselves too.
    assert!(
        stored.get("account").is_none() && stored.get("schema").is_none(),
        "the secret store must never receive the whole snapshot: {stored}"
    );
}

#[test]
fn sync_back_captures_an_account_byte_has_never_seen() {
    let tp = TestPaths::new().unwrap();
    login_as(&tp, "u1", "a@example.com", "r1");
    let sw = Switcher::new(&tp, MemoryStore::new());

    match sw.sync_back().unwrap() {
        SyncOutcome::Captured(m) => assert_eq!(m.uuid, "u1"),
        other => panic!("expected Captured, got {other:?}"),
    }
}

#[test]
fn sync_back_refreshes_a_rotated_token() {
    // The scenario spec section 7.1 exists to defend against.
    let tp = TestPaths::new().unwrap();
    login_as(&tp, "u1", "a@example.com", "original");
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.capture_current().unwrap();

    // Claude Code refreshes and rotates the token behind byte's back.
    login_as(&tp, "u1", "a@example.com", "rotated");

    match sw.sync_back().unwrap() {
        SyncOutcome::Updated(m) => assert_eq!(m.uuid, "u1"),
        other => panic!("expected Updated, got {other:?}"),
    }
    assert_eq!(
        sw.secrets().get("u1").unwrap().unwrap()["refreshToken"],
        json!("rotated")
    );
}

#[test]
fn sync_back_reports_logged_out_when_no_account_is_present() {
    let tp = TestPaths::new().unwrap();
    std::fs::write(tp.claude_credentials(), "{}").unwrap();
    std::fs::write(tp.claude_config(), "{}").unwrap();
    let sw = Switcher::new(&tp, MemoryStore::new());

    assert!(matches!(sw.sync_back().unwrap(), SyncOutcome::LoggedOut));
}

#[test]
fn sync_back_still_reports_logged_out_for_a_genuinely_token_less_snapshot() {
    // Identity IS present here (unlike the unidentifiable-live-account
    // tests below) so this isolates the refresh-token condition: a snapshot
    // with an identity but no refresh token is genuinely nothing to lose,
    // and must still degrade to LoggedOut rather than error.
    let tp = TestPaths::new().unwrap();
    std::fs::write(
        tp.claude_credentials(),
        serde_json::to_string(&json!({
            "claudeAiOauth": {"accessToken": "a", "refreshToken": "", "expiresAt": 1i64}
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        tp.claude_config(),
        serde_json::to_string(&json!({
            "oauthAccount": {"accountUuid": "u1", "emailAddress": "a@example.com"}
        }))
        .unwrap(),
    )
    .unwrap();
    let sw = Switcher::new(&tp, MemoryStore::new());

    assert!(matches!(sw.sync_back().unwrap(), SyncOutcome::LoggedOut));
}

#[test]
fn sync_back_aborts_rather_than_losing_an_unidentifiable_live_account() {
    // The Finding-1 scenario at the sync_back layer directly: real,
    // non-empty credentials in .credentials.json, but .claude.json is valid
    // JSON with no oauthAccount at all — e.g. a partially-completed login.
    // capture() still returns Some, but the resulting snapshot has no
    // identity to key it by, so it must NOT be silently reported as
    // LoggedOut.
    let tp = TestPaths::new().unwrap();
    std::fs::write(
        tp.claude_credentials(),
        serde_json::to_string(&json!({
            "claudeAiOauth": {
                "accessToken": "a", "refreshToken": "irreplaceable", "expiresAt": 1i64
            }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        tp.claude_config(),
        serde_json::to_string(&json!({"numStartups": 1})).unwrap(),
    )
    .unwrap();
    let sw = Switcher::new(&tp, MemoryStore::new());

    let err = sw.sync_back().unwrap_err();

    assert!(matches!(err, byte::Error::UnidentifiableLiveAccount { .. }));
    // Nothing was captured under any key — the account was, by definition,
    // unnameable.
    assert!(
        AccountsFile::load(&tp.accounts_file())
            .unwrap()
            .accounts
            .is_empty()
    );
}

#[test]
fn switching_writes_the_target_account_to_the_live_files() {
    let tp = TestPaths::new().unwrap();
    login_as(&tp, "u1", "a@example.com", "r1");
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.capture_current().unwrap();

    login_as(&tp, "u2", "b@example.com", "r2");
    sw.capture_current().unwrap();

    let out: SwitchOutcome = sw.switch_to("a@example.com").unwrap();

    assert_eq!(out.switched_to.uuid, "u1");
    assert_eq!(live_refresh(&tp), "r1");
    let snap = ClaudeFiles::new(&tp).capture().unwrap().unwrap();
    assert_eq!(snap.email(), Some("a@example.com"));
}

#[test]
fn switching_away_saves_the_current_account_first() {
    let tp = TestPaths::new().unwrap();
    login_as(&tp, "u1", "a@example.com", "r1");
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.capture_current().unwrap();
    login_as(&tp, "u2", "b@example.com", "r2");
    sw.capture_current().unwrap();

    // u2 is live; rotate it, then switch away without capturing.
    login_as(&tp, "u2", "b@example.com", "r2-rotated");
    sw.switch_to("a@example.com").unwrap();

    assert_eq!(
        sw.secrets().get("u2").unwrap().unwrap()["refreshToken"],
        json!("r2-rotated")
    );
}

#[test]
fn switching_back_and_forth_round_trips_cleanly() {
    let tp = TestPaths::new().unwrap();
    login_as(&tp, "u1", "a@example.com", "r1");
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.capture_current().unwrap();
    login_as(&tp, "u2", "b@example.com", "r2");
    sw.capture_current().unwrap();

    sw.switch_to("a@example.com").unwrap();
    assert_eq!(live_refresh(&tp), "r1");
    assert_eq!(
        ClaudeFiles::new(&tp).capture().unwrap().unwrap().email(),
        Some("a@example.com")
    );

    sw.switch_to("b@example.com").unwrap();
    assert_eq!(live_refresh(&tp), "r2");
    assert_eq!(
        ClaudeFiles::new(&tp).capture().unwrap().unwrap().email(),
        Some("b@example.com")
    );

    sw.switch_to("a@example.com").unwrap();
    assert_eq!(live_refresh(&tp), "r1");
    assert_eq!(
        ClaudeFiles::new(&tp).capture().unwrap().unwrap().email(),
        Some("a@example.com")
    );
}

#[test]
fn switching_preserves_unrelated_keys_in_both_files() {
    let tp = TestPaths::new().unwrap();
    login_as(&tp, "u1", "a@example.com", "r1");
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.capture_current().unwrap();
    login_as(&tp, "u2", "b@example.com", "r2");
    sw.capture_current().unwrap();

    sw.switch_to("a@example.com").unwrap();

    let creds: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tp.claude_credentials()).unwrap()).unwrap();
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tp.claude_config()).unwrap()).unwrap();
    assert_eq!(creds["mcpOAuth"]["srv"]["accessToken"], json!("keep"));
    assert_eq!(cfg["numStartups"], json!(7));
}

#[test]
fn switching_to_the_active_account_is_a_no_op_that_still_syncs() {
    let tp = TestPaths::new().unwrap();
    login_as(&tp, "u1", "a@example.com", "r1");
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.capture_current().unwrap();

    // Rotate the live token without capturing, so only sync_back — run as
    // part of this "no-op" switch — can be the thing that carries it into
    // the store. An implementation that read the stored secret before
    // calling sync_back would apply the stale value and leave both the live
    // file and the store holding "r1", so this would not catch that
    // ordering bug without the rotation.
    login_as(&tp, "u1", "a@example.com", "r1-rotated");

    let out = sw.switch_to("a@example.com").unwrap();

    assert!(out.already_active);
    assert_eq!(live_refresh(&tp), "r1-rotated");
    assert_eq!(
        sw.secrets().get("u1").unwrap().unwrap()["refreshToken"],
        json!("r1-rotated")
    );
}

#[test]
fn already_active_reflects_the_live_account_not_a_stale_active_pointer() {
    // Finding I2: `accounts.active` is only ever updated by switch_to and
    // capture_current -- sync_back never touches it. So if the user logs
    // out of Claude Code and back in as a different account directly
    // (bypassing byte entirely), `accounts.active` still names whichever
    // account byte last switched to, even though it is no longer live.
    // already_active must reflect reality, not that stale pointer -- it
    // also gates the "sessions must be restarted" warning, so getting this
    // wrong tells the user nothing happened when a switch just did.
    let tp = TestPaths::new().unwrap();
    login_as(&tp, "u1", "a@example.com", "r1");
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.capture_current().unwrap(); // accounts.active = u1

    // u2 logs in directly, without going through byte, so byte never
    // captures it and accounts.active stays "u1".
    login_as(&tp, "u2", "b@example.com", "r2");

    let out = sw.switch_to("a@example.com").unwrap();

    assert!(
        !out.already_active,
        "u2 is live, not u1 -- switching to u1 must not be reported as a no-op"
    );
    assert_eq!(live_refresh(&tp), "r1");
}

#[test]
fn switching_to_an_unknown_account_fails_without_touching_the_files() {
    let tp = TestPaths::new().unwrap();
    login_as(&tp, "u1", "a@example.com", "r1");
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.capture_current().unwrap();

    // Rotate the live token without capturing. If switch_to called
    // sync_back before resolving the (unknown) target, the store would pick
    // up this rotated value even though the switch itself must fail — so
    // this pins the resolve-before-mutate ordering, not just "some error
    // occurred".
    login_as(&tp, "u1", "a@example.com", "r1-rotated");

    let err = sw.switch_to("nobody@example.com").unwrap_err();

    assert!(matches!(err, byte::Error::NoSuchAccount(_)));
    assert_eq!(live_refresh(&tp), "r1-rotated");
    assert_eq!(
        sw.secrets().get("u1").unwrap().unwrap()["refreshToken"],
        json!("r1")
    );
}

#[test]
fn switch_to_aborts_rather_than_losing_an_unidentifiable_live_account() {
    // The full end-to-end consequence Finding 1 described: u1 is a known
    // account with real stored secrets; something else goes live that byte
    // cannot identify. switch_to must refuse rather than silently "succeed"
    // by overwriting the unidentifiable live credentials with u1's.
    let tp = TestPaths::new().unwrap();
    login_as(&tp, "u1", "a@example.com", "r1");
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.capture_current().unwrap();
    login_as(&tp, "u2", "b@example.com", "r2");
    sw.capture_current().unwrap();

    // Something else is now live: real credentials, but .claude.json has no
    // oauthAccount at all (otherwise valid JSON).
    std::fs::write(
        tp.claude_credentials(),
        serde_json::to_string(&json!({
            "claudeAiOauth": {
                "accessToken": "a", "refreshToken": "irreplaceable", "expiresAt": 1i64
            }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        tp.claude_config(),
        serde_json::to_string(&json!({"numStartups": 1})).unwrap(),
    )
    .unwrap();

    let err = sw.switch_to("a@example.com").unwrap_err();

    assert!(matches!(err, byte::Error::UnidentifiableLiveAccount { .. }));
    // The unidentifiable live credentials must survive untouched — apply()
    // must never have run.
    assert_eq!(live_refresh(&tp), "irreplaceable");
}

// The tests below cover the keychain-size fix's storage split: the secret
// `oauth` block now lives only in the OS keychain (`SecretStore`), while
// the non-secret `account` (oauthAccount) / `user_id` / `credential_schema`
// live in `accounts.json` (`AccountMeta`). `Switcher::load_snapshot`
// reassembles the two into a complete `AccountSnapshot`; these tests pin
// both the happy path and every way the halves can disagree.

fn ghost_meta(uuid: &str, email: &str, account: serde_json::Value, schema: u32) -> AccountMeta {
    AccountMeta {
        uuid: uuid.to_string(),
        label: email.to_string(),
        email: Some(email.to_string()),
        organization_name: None,
        subscription_type: None,
        account,
        user_id: None,
        credential_schema: schema,
        added_at: "2026-01-01T00:00:00Z".to_string(),
        last_used_at: None,
        desktop_profile: None,
    }
}

#[test]
fn load_snapshot_fails_cleanly_when_the_keychain_half_is_missing() {
    // accounts.json can list an account whose OS keychain entry is gone --
    // deleted out from under byte, or never written because of a crash
    // between the two writes. Reassembly must refuse loudly rather than
    // handing `apply()` a snapshot built around a missing oauth block.
    let tp = TestPaths::new().unwrap();
    let sw = Switcher::new(&tp, MemoryStore::new());
    let meta = ghost_meta(
        "u1",
        "ghost@example.com",
        json!({"accountUuid": "u1", "emailAddress": "ghost@example.com"}),
        SCHEMA_VERSION,
    );
    // Deliberately never put anything in `sw.secrets()` for "u1".

    let err = sw.load_snapshot(&meta).unwrap_err();

    // Pinned to the *reason text*, not just the variant: an implementation
    // that dropped the "no keychain entry at all" check and instead handed
    // a null/empty oauth `Value` down to `validate()` would also produce an
    // `InvalidSnapshot` -- via validate()'s unrelated "no refresh token"
    // check -- so matching the variant alone would not actually prove
    // load_snapshot's own missing-entry check ever ran.
    match &err {
        byte::Error::InvalidSnapshot { reason, .. } => {
            assert!(
                reason.contains("OS keychain"),
                "expected a reason naming the missing keychain entry specifically, got: {reason}"
            );
        }
        other => panic!("expected InvalidSnapshot, got: {other}"),
    }
}

#[test]
fn load_snapshot_fails_cleanly_when_the_account_block_lost_its_identity() {
    // The other direction of the same disagreement: the OS keychain half
    // is present and valid, but the metadata's `account` field has no
    // accountUuid or email to key it by (hand-edited, or a partial write).
    // validate()'s identity check inside load_snapshot is what catches
    // this -- reassembly itself does not special-case it.
    let tp = TestPaths::new().unwrap();
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.secrets()
        .put("u1", &json!({"refreshToken": "r1", "expiresAt": 1i64}))
        .unwrap();
    let meta = ghost_meta("u1", "ghost@example.com", json!({}), SCHEMA_VERSION);

    let err = sw.load_snapshot(&meta).unwrap_err();

    assert!(matches!(err, byte::Error::InvalidSnapshot { .. }));
}

#[test]
fn load_snapshot_fails_cleanly_when_the_stored_schema_is_unrecognized() {
    // Exercises AccountSnapshot::reassemble's schema handling directly:
    // reassembly must carry forward the schema version the account was
    // captured under, not silently stamp the current one -- otherwise a
    // stored account from an incompatible schema would sail through
    // validate() on every reassembly, defeating the protection a freshly
    // captured snapshot already gets from the same check.
    let tp = TestPaths::new().unwrap();
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.secrets()
        .put("u1", &json!({"refreshToken": "r1", "expiresAt": 1i64}))
        .unwrap();
    let meta = ghost_meta(
        "u1",
        "old@example.com",
        json!({"accountUuid": "u1", "emailAddress": "old@example.com"}),
        999,
    );

    let err = sw.load_snapshot(&meta).unwrap_err();

    assert!(matches!(
        err,
        byte::Error::SchemaMismatch { found: 999, .. }
    ));
}

#[test]
fn load_snapshot_reassembles_every_field_from_both_halves() {
    let tp = TestPaths::new().unwrap();
    let sw = Switcher::new(&tp, MemoryStore::new());
    let oauth = json!({"accessToken": "a1", "refreshToken": "r1", "expiresAt": 1i64});
    sw.secrets().put("u1", &oauth).unwrap();
    let account = json!({
        "accountUuid": "u1",
        "emailAddress": "a@example.com",
        "displayName": "Ada"
    });
    let meta = AccountMeta {
        uuid: "u1".to_string(),
        label: "work".to_string(),
        email: Some("a@example.com".to_string()),
        organization_name: None,
        subscription_type: None,
        account: account.clone(),
        user_id: Some("user-1".to_string()),
        credential_schema: SCHEMA_VERSION,
        added_at: "2026-01-01T00:00:00Z".to_string(),
        last_used_at: None,
        desktop_profile: None,
    };

    let snapshot = sw.load_snapshot(&meta).unwrap();

    assert_eq!(snapshot.oauth, oauth);
    assert_eq!(snapshot.account, account);
    assert_eq!(snapshot.user_id.as_deref(), Some("user-1"));
    assert_eq!(snapshot.schema, SCHEMA_VERSION);
}

#[test]
fn switch_to_fails_cleanly_when_the_keychain_half_is_missing_for_the_target() {
    // The full end-to-end wiring for the reassembly failure above: a
    // target account is resolvable by name (its accounts.json entry is
    // intact) but its OS keychain entry is missing. switch_to must refuse
    // before apply() ever runs, and the live files must show the switch
    // never happened.
    let tp = TestPaths::new().unwrap();
    login_as(&tp, "good-uuid", "good@example.com", "r-good");
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.capture_current().unwrap();

    // Seed "ghost"'s metadata directly -- capture_current is deliberately
    // never called for it, so MemoryStore never receives its oauth half.
    let mut accounts = AccountsFile::load(&tp.accounts_file()).unwrap();
    let ghost = AccountSnapshot::new(
        json!({"refreshToken": "r-ghost", "expiresAt": 1i64}),
        json!({"accountUuid": "ghost-uuid", "emailAddress": "ghost@example.com"}),
        Some("uid-ghost".to_string()),
    );
    accounts.upsert_from("ghost-uuid", &ghost);
    accounts
        .save(&tp.accounts_file(), &tp.backup_dir())
        .unwrap();

    let err = sw.switch_to("ghost@example.com").unwrap_err();

    assert!(matches!(err, byte::Error::InvalidSnapshot { .. }));
    // apply() must never have run.
    assert_eq!(live_refresh(&tp), "r-good");
}

#[test]
fn switching_preserves_every_oauth_account_field_across_the_split_storage() {
    // The correctness property the storage split must not regress:
    // accounts.json's `account` field is the FULL oauthAccount object, not
    // just the handful of fields AccountMeta also flattens out for
    // display. If reassembly only used those flattened fields, a switch
    // would silently drop every other field (displayName, billingType,
    // organizationRole, ...) the next time it patched .claude.json.
    let tp = TestPaths::new().unwrap();
    std::fs::write(
        tp.claude_credentials(),
        serde_json::to_string(&json!({
            "claudeAiOauth": {"accessToken": "a", "refreshToken": "r1", "expiresAt": 1i64}
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        tp.claude_config(),
        serde_json::to_string(&json!({
            "oauthAccount": {
                "accountUuid": "u1",
                "emailAddress": "a@example.com",
                "displayName": "Ada Example",
                "billingType": "organization",
                "organizationRole": "admin",
                "organizationUuid": "org-1"
            },
            "userID": "user-1"
        }))
        .unwrap(),
    )
    .unwrap();
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.capture_current().unwrap();

    // A second account goes live, so switching back to u1 below exercises
    // a real reassembly from storage, not u1 already being the live file
    // content.
    std::fs::write(
        tp.claude_credentials(),
        serde_json::to_string(&json!({
            "claudeAiOauth": {"accessToken": "a", "refreshToken": "r2", "expiresAt": 1i64}
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        tp.claude_config(),
        serde_json::to_string(&json!({
            "oauthAccount": {"accountUuid": "u2", "emailAddress": "b@example.com"},
            "userID": "user-2"
        }))
        .unwrap(),
    )
    .unwrap();
    sw.capture_current().unwrap();

    sw.switch_to("a@example.com").unwrap();

    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tp.claude_config()).unwrap()).unwrap();
    assert_eq!(cfg["oauthAccount"]["displayName"], json!("Ada Example"));
    assert_eq!(cfg["oauthAccount"]["billingType"], json!("organization"));
    assert_eq!(cfg["oauthAccount"]["organizationRole"], json!("admin"));
    assert_eq!(cfg["oauthAccount"]["organizationUuid"], json!("org-1"));
    assert_eq!(cfg["userID"], json!("user-1"));
}
