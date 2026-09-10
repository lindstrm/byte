use byte::Error;
use byte::claude::snapshot::AccountSnapshot;
use byte::paths::{HostPaths, TestPaths};
use byte::store::metadata::{AccountsFile, DesktopProfileRecord};
use serde_json::json;

fn snap(uuid: &str, email: &str) -> AccountSnapshot {
    AccountSnapshot::new(
        json!({"refreshToken": "r", "subscriptionType": "max"}),
        json!({"accountUuid": uuid, "emailAddress": email, "organizationName": "Org"}),
        Some("uid".to_string()),
    )
}

#[test]
fn upsert_adds_a_new_account_labelled_by_email() {
    let mut file = AccountsFile::default();
    let meta = file.upsert_from("u1", &snap("u1", "a@example.com"));

    assert_eq!(meta.label, "a@example.com");
    assert_eq!(meta.uuid, "u1");
    assert_eq!(meta.subscription_type.as_deref(), Some("max"));
    assert_eq!(file.accounts.len(), 1);
}

#[test]
fn upsert_updates_in_place_and_keeps_a_custom_label() {
    let mut file = AccountsFile::default();
    file.upsert_from("u1", &snap("u1", "a@example.com"));
    file.rename("u1", "work").unwrap();

    file.upsert_from("u1", &snap("u1", "a@example.com"));

    assert_eq!(file.accounts.len(), 1);
    assert_eq!(file.accounts[0].label, "work");
}

#[test]
fn resolve_matches_label_then_email_then_uuid_prefix() {
    let mut file = AccountsFile::default();
    file.upsert_from("abcdef123456", &snap("abcdef123456", "a@example.com"));
    file.rename("abcdef123456", "personal").unwrap();
    file.upsert_from("999999999999", &snap("999999999999", "b@example.com"));

    assert_eq!(file.resolve("personal").unwrap().uuid, "abcdef123456");
    assert_eq!(file.resolve("b@example.com").unwrap().uuid, "999999999999");
    assert_eq!(file.resolve("abcdef").unwrap().uuid, "abcdef123456");
}

#[test]
fn resolve_is_case_insensitive() {
    let mut file = AccountsFile::default();
    file.upsert_from("u1", &snap("u1", "Alice@Example.com"));

    assert_eq!(file.resolve("alice@example.com").unwrap().uuid, "u1");
}

#[test]
fn resolve_reports_an_unknown_name() {
    let file = AccountsFile::default();
    assert!(matches!(
        file.resolve("nobody"),
        Err(Error::NoSuchAccount(_))
    ));
}

#[test]
fn resolve_reports_ambiguity_rather_than_guessing() {
    let mut file = AccountsFile::default();
    file.upsert_from("aaa111", &snap("aaa111", "x@example.com"));
    file.upsert_from("aaa222", &snap("aaa222", "y@example.com"));

    assert!(matches!(
        file.resolve("aaa"),
        Err(Error::AmbiguousAccount { count: 2, .. })
    ));
}

#[test]
fn resolve_rejects_an_empty_query_even_with_exactly_one_account() {
    let mut file = AccountsFile::default();
    file.upsert_from("u1", &snap("u1", "a@example.com"));

    // With a single stored account, "" and uuid.starts_with("") are both
    // true, so an unguarded prefix fallback would resolve a blank query to
    // that account. Zero or 2+ accounts already error correctly; this is
    // the one case that previously slipped through.
    assert!(file.resolve("").is_err());
    assert!(file.resolve("   ").is_err());
}

#[test]
fn resolve_mut_reports_an_unknown_name() {
    let mut file = AccountsFile::default();
    assert!(matches!(
        file.resolve_mut("nobody"),
        Err(Error::NoSuchAccount(_))
    ));
}

#[test]
fn resolve_mut_reports_ambiguity_rather_than_guessing() {
    let mut file = AccountsFile::default();
    file.upsert_from("aaa111", &snap("aaa111", "x@example.com"));
    file.upsert_from("aaa222", &snap("aaa222", "y@example.com"));

    assert!(matches!(
        file.resolve_mut("aaa"),
        Err(Error::AmbiguousAccount { count: 2, .. })
    ));
}

#[test]
fn remove_deletes_the_account_and_clears_active_when_it_matches() {
    let mut file = AccountsFile::default();
    file.upsert_from("u1", &snap("u1", "a@example.com"));
    file.set_active("u1");

    let removed = file.remove("u1").unwrap();

    assert_eq!(removed.uuid, "u1");
    assert!(file.accounts.is_empty());
    assert!(file.active.is_none());
}

#[test]
fn accounts_survive_a_save_and_load_cycle() {
    let tp = TestPaths::new().unwrap();
    let mut file = AccountsFile::default();
    file.upsert_from("u1", &snap("u1", "a@example.com"));
    file.set_active("u1");
    file.save(&tp.accounts_file(), &tp.backup_dir()).unwrap();

    let loaded = AccountsFile::load(&tp.accounts_file()).unwrap();

    assert_eq!(loaded.accounts.len(), 1);
    assert_eq!(loaded.active.as_deref(), Some("u1"));
    assert_eq!(loaded.accounts[0].uuid, "u1");
    assert_eq!(loaded.accounts[0].label, "a@example.com");
    assert_eq!(loaded.accounts[0].email.as_deref(), Some("a@example.com"));
    // The keychain-size fix's whole reason for being: the non-secret half
    // of a snapshot (account block, userID, credential schema) must
    // survive the same save/load cycle as the display fields above, since
    // `load_snapshot` depends on reading it back intact to reassemble a
    // complete `AccountSnapshot` later.
    assert_eq!(
        loaded.accounts[0].account,
        json!({"accountUuid": "u1", "emailAddress": "a@example.com", "organizationName": "Org"})
    );
    assert_eq!(loaded.accounts[0].user_id.as_deref(), Some("uid"));
    assert_eq!(loaded.accounts[0].credential_schema, 1);
}

#[test]
fn upsert_from_carries_the_full_account_block_and_user_id_for_reassembly() {
    // Direct unit coverage of the keychain-size fix's core data-flow change:
    // upsert_from must copy `account`, `user_id`, and `schema` off the
    // snapshot verbatim, not just the flattened display fields (email,
    // organization_name, subscription_type) it already extracted before
    // this fix. Nested, multi-key account object so a field silently
    // dropped in transit would be caught, not masked by an empty value.
    let mut file = AccountsFile::default();
    let snapshot = AccountSnapshot::new(
        json!({"refreshToken": "r", "expiresAt": 1i64}),
        json!({
            "accountUuid": "u1",
            "emailAddress": "a@example.com",
            "displayName": "Ada",
            "billingType": "organization",
            "organizationRole": "admin"
        }),
        Some("user-1".to_string()),
    );

    let meta = file.upsert_from("u1", &snapshot);

    assert_eq!(meta.account, snapshot.account);
    assert_eq!(meta.user_id, snapshot.user_id);
    assert_eq!(meta.credential_schema, snapshot.schema);
}

#[test]
fn saved_accounts_json_never_contains_the_oauth_tokens() {
    // Invariant: accounts.json must never carry a secret. After the
    // keychain-size fix, only the `oauth` block (claudeAiOauth, holding
    // accessToken/refreshToken) stays keychain-only -- the non-secret
    // `account` (oauthAccount) and `user_id` now travel with AccountMeta,
    // so this is the one place a regression could leak a token into the
    // metadata file. Checked at the only boundary that actually matters:
    // the literal bytes written to disk, not the in-memory struct.
    let tp = TestPaths::new().unwrap();
    let mut file = AccountsFile::default();

    let snapshot = AccountSnapshot::new(
        json!({
            "accessToken": "super-secret-access-token-value",
            "refreshToken": "super-secret-refresh-token-value",
            "expiresAt": 1234567890i64,
        }),
        json!({"accountUuid": "u1", "emailAddress": "a@example.com"}),
        Some("user-1".to_string()),
    );
    file.upsert_from("u1", &snapshot);
    file.save(&tp.accounts_file(), &tp.backup_dir()).unwrap();

    let text = std::fs::read_to_string(tp.accounts_file()).unwrap();
    assert!(
        !text.contains("super-secret-access-token-value"),
        "accounts.json must never contain the access token:\n{text}"
    );
    assert!(
        !text.contains("super-secret-refresh-token-value"),
        "accounts.json must never contain the refresh token:\n{text}"
    );
    // Belt and suspenders: the field names themselves should not appear
    // either, since their presence would mean the whole oauth block leaked
    // in, not just a token value that happens to be checked above.
    assert!(!text.contains("accessToken"), "leaked field name:\n{text}");
    assert!(!text.contains("refreshToken"), "leaked field name:\n{text}");
}

#[test]
fn loading_an_unrecognized_schema_version_is_rejected_with_a_clear_error() {
    // accounts.json must fail closed on a schema it does not understand,
    // rather than silently misparsing (e.g. every AccountMeta ending up
    // with an empty `account` block because the fields just didn't match)
    // or crashing with a raw serde error that doesn't say what's wrong.
    let tp = TestPaths::new().unwrap();
    std::fs::write(
        tp.accounts_file(),
        json!({"schema": 1, "active": null, "accounts": []}).to_string(),
    )
    .unwrap();

    let err = AccountsFile::load(&tp.accounts_file()).unwrap_err();

    assert!(matches!(
        err,
        Error::AccountsSchemaMismatch {
            found: 1,
            expected: 2
        }
    ));
}

#[test]
fn loading_accounts_json_with_no_schema_field_at_all_is_rejected_not_misparsed() {
    // A degenerate case of the same guard: a `schema` key that is missing
    // entirely (not just an old version) must not be treated as valid --
    // it reads as version 0, which can never match the current schema.
    let tp = TestPaths::new().unwrap();
    std::fs::write(
        tp.accounts_file(),
        json!({"active": null, "accounts": []}).to_string(),
    )
    .unwrap();

    let err = AccountsFile::load(&tp.accounts_file()).unwrap_err();

    assert!(matches!(
        err,
        Error::AccountsSchemaMismatch { found: 0, .. }
    ));
}

#[test]
fn loading_a_missing_file_yields_an_empty_store() {
    let tp = TestPaths::new().unwrap();
    let loaded = AccountsFile::load(&tp.accounts_file()).unwrap();
    assert!(loaded.accounts.is_empty());
}

#[test]
fn save_prunes_accounts_json_backups_to_the_ten_newest() {
    // Regression test for task-11 review Finding 4: unlike .claude.json and
    // .credentials.json (pruned by JsonDocument::save), accounts.json's own
    // save() never called atomic::prune at all, so its backups grew
    // unbounded -- one more per capture, switch, rename, and remove.
    //
    // Finding M5: the original version of this test asserted only
    // `remaining.len() == 10`, which a REVERSED sort in prune() -- deleting
    // the ten newest and keeping the three oldest -- would also satisfy.
    // This asserts identity: which twelve names survive and which three are
    // gone, not merely how many files remain.
    let tp = TestPaths::new().unwrap();
    let path = tp.accounts_file();
    let backup_dir = tp.backup_dir();
    let stem = path.file_name().unwrap().to_string_lossy().to_string();

    // Seed the accounts file so this save()'s own backup() call has
    // something to back up, plus 12 synthetic pre-existing backups already
    // over the retention limit. Synthetic filenames (rather than driving 12
    // real save() calls in a loop) sidestep atomic::backup's
    // millisecond-granularity timestamps, which a tight loop could
    // otherwise collide on and silently produce fewer than 12 real files.
    // Their zero-padded 13-digit timestamps (0..11) are far smaller than any
    // real millisecond-since-epoch value, so save()'s own backup() call --
    // using the real current time -- is always the single newest entry.
    std::fs::write(&path, "{}").unwrap();
    for i in 0..12u64 {
        std::fs::write(backup_dir.join(format!("{stem}.{i:013}.bak")), "old").unwrap();
    }

    let mut file = AccountsFile::default();
    file.upsert_from("u1", &snap("u1", "a@example.com"));
    file.save(&path, &backup_dir).unwrap();

    let mut remaining: Vec<String> = std::fs::read_dir(&backup_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with(&format!("{stem}.")))
        .collect();
    remaining.sort();

    // 12 synthetic backups plus 1 real one from this save()'s own backup()
    // call = 13 candidates; without prune wired up, all 13 survive.
    assert_eq!(
        remaining.len(),
        10,
        "expected accounts.json backups capped at 10, found {}: {remaining:?}",
        remaining.len()
    );

    // The three OLDEST synthetic backups (i = 0, 1, 2) must be gone. A
    // reversed sort would instead prune the three NEWEST synthetic backups
    // (i = 9, 10, 11) plus the real one, still leaving exactly 10 files --
    // indistinguishable from the correct outcome under a cardinality-only
    // assertion.
    for i in 0..3u64 {
        let doomed = format!("{stem}.{i:013}.bak");
        assert!(
            !remaining.contains(&doomed),
            "{doomed} should have been pruned as one of the three oldest, but survived: {remaining:?}"
        );
    }
    for i in 3..12u64 {
        let survivor = format!("{stem}.{i:013}.bak");
        assert!(
            remaining.contains(&survivor),
            "{survivor} should have survived pruning, but is missing: {remaining:?}"
        );
    }
    // Every synthetic name is accounted for above (9 survivors + 3 pruned =
    // 12), so exactly one of the 10 remaining entries is not a synthetic
    // name at all -- this save()'s own real-timestamp backup.
    let non_synthetic = remaining
        .iter()
        .filter(|n| !(3..12u64).any(|i| **n == format!("{stem}.{i:013}.bak")))
        .count();
    assert_eq!(
        non_synthetic, 1,
        "expected exactly one non-synthetic (real) backup among the survivors: {remaining:?}"
    );
}

#[test]
fn an_accounts_file_without_desktop_profiles_still_loads() {
    // Purely additive: an accounts.json written before this feature must
    // load unchanged, with no schema bump.
    let tp = TestPaths::new().unwrap();
    std::fs::write(
        tp.accounts_file(),
        serde_json::json!({
            "schema": 2,
            "active": null,
            "accounts": [{
                "uuid": "u1",
                "label": "work",
                "email": "w@example.com",
                "organization_name": null,
                "subscription_type": null,
                "account": {"accountUuid": "u1"},
                "user_id": null,
                "credential_schema": 1,
                "added_at": "2026-01-01T00:00:00Z",
                "last_used_at": null
            }]
        })
        .to_string(),
    )
    .unwrap();

    let loaded = AccountsFile::load(&tp.accounts_file()).unwrap();
    let acct = loaded.resolve("work").unwrap();
    assert_eq!(acct.label, "work");
    assert!(acct.desktop_profile.is_none());
}

#[test]
fn a_desktop_profile_record_round_trips() {
    // Uses this file's existing `snap` helper and the real AccountsFile API:
    // `upsert_from(uuid, &snapshot)`, and `save`/`load` taking paths rather
    // than a HostPaths.
    let tp = TestPaths::new().unwrap();
    let mut file = AccountsFile::default();
    file.upsert_from("u1", &snap("u1", "w@example.com"));
    file.resolve_mut("u1").unwrap().desktop_profile = Some(DesktopProfileRecord {
        captured_at: "2026-09-09T12:00:00Z".to_string(),
        bytes: 54_000_000,
    });
    file.save(&tp.accounts_file(), &tp.backup_dir()).unwrap();

    let back = AccountsFile::load(&tp.accounts_file()).unwrap();
    let got = back.resolve("u1").unwrap().desktop_profile.clone().unwrap();
    assert_eq!(got.bytes, 54_000_000);
    assert_eq!(got.captured_at, "2026-09-09T12:00:00Z");
}

#[test]
fn upsert_from_preserves_an_existing_desktop_profile_record() {
    // desktop_profile is stamped by a completely separate operation (the
    // desktop swap in task 8's resolve_mut call) from upsert_from (the CLI
    // sync-back/switch path, task-agnostic and far more frequent). If
    // upsert_from clobbered it back to None on every ordinary CLI-only
    // switch, "absent means never captured" (the design's decision-5 signal)
    // would stop meaning that the moment any unrelated switch touched this
    // account again -- even though the parked profile is still sitting on
    // disk, untouched. The record must survive exactly like `added_at` and
    // `last_used_at` already do above.
    let mut file = AccountsFile::default();
    file.upsert_from("u1", &snap("u1", "w@example.com"));
    file.resolve_mut("u1").unwrap().desktop_profile = Some(DesktopProfileRecord {
        captured_at: "2026-09-09T12:00:00Z".to_string(),
        bytes: 54_000_000,
    });

    // Simulate a later, unrelated sync-back/switch touching the same
    // account -- e.g. Claude Code rotated its token and byte re-synced.
    file.upsert_from("u1", &snap("u1", "w@example.com"));

    let record = file
        .resolve("u1")
        .unwrap()
        .desktop_profile
        .clone()
        .expect("a later upsert_from must not erase an existing desktop_profile record");
    assert_eq!(record.bytes, 54_000_000);
    assert_eq!(record.captured_at, "2026-09-09T12:00:00Z");
}

#[test]
fn saved_accounts_json_omits_desktop_profile_key_when_absent() {
    // skip_serializing_if is what keeps an accounts.json written by this
    // code indistinguishable, for an account with no desktop profile, from
    // one written by a byte that predates the field entirely -- so this
    // must be a literal absence of the "desktop_profile" key, not a present
    // key holding `null`. A `null` would still deserialize fine today, but
    // it is not what "never touched the desktop app" is supposed to look
    // like on disk, and it would make future key-presence checks (e.g. a
    // hand migration script) see the key where it should see nothing.
    let tp = TestPaths::new().unwrap();
    let mut file = AccountsFile::default();
    file.upsert_from("u1", &snap("u1", "a@example.com"));
    file.save(&tp.accounts_file(), &tp.backup_dir()).unwrap();

    let text = std::fs::read_to_string(tp.accounts_file()).unwrap();
    assert!(
        !text.contains("desktop_profile"),
        "accounts.json must omit desktop_profile entirely when absent, not write it as null:\n{text}"
    );
}
