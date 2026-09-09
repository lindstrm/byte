use byte::desktop::profile::{Disposition, classify, movable_entries};

#[test]
fn session_bearing_directories_move() {
    for name in [
        "Network",
        "Local Storage",
        "IndexedDB",
        "Session Storage",
        "WebStorage",
        "Shared Dictionary",
        "claude-code-sessions",
        "local-agent-mode-sessions",
    ] {
        assert_eq!(classify(name), Disposition::Move, "{name} should move");
    }
}

#[test]
fn caches_and_binaries_and_logs_are_left_behind() {
    for name in [
        "Cache",
        "Code Cache",
        "GPUCache",
        "DawnGraphiteCache",
        "DawnWebGPUCache",
        "blob_storage",
        "logs",
        "Crashpad",
        "sentry",
        "lockfile",
        "claude-code",
    ] {
        assert_eq!(classify(name), Disposition::Leave, "{name} should stay");
    }
}

#[test]
fn the_claude_code_binaries_are_never_moved() {
    // 416 MB of program files, not account state. Spec §14's original
    // "rename the whole directory" would have duplicated them per account.
    assert_eq!(classify("claude-code"), Disposition::Leave);
    // Distinct from the session history directory, which does move -- these
    // two differ by one word and have opposite dispositions.
    assert_eq!(classify("claude-code-sessions"), Disposition::Move);
}

#[test]
fn local_state_stays_shared() {
    // It holds the DPAPI key that decrypts cookies. Moving it with one
    // account's profile would make every OTHER parked profile's cookies
    // undecryptable -- a single wrong classification breaking all accounts
    // at once, not just the one being switched.
    assert_eq!(classify("Local State"), Disposition::Leave);
}

#[test]
fn partitions_stays_because_it_is_already_account_keyed() {
    // Its account-scoped entry is named cowork-artifact-<accountUuid>-...,
    // so distinct accounts already get distinct directories; the rest is
    // launch-preview-* sandboxes that are ~90% cache.
    assert_eq!(classify("Partitions"), Disposition::Leave);
}

#[test]
fn the_mcp_config_and_usage_counter_stay() {
    assert_eq!(classify("claude_desktop_config.json"), Disposition::Leave);
    // 68 bytes, one key `tokens-today` -- an LLM usage counter, not
    // credentials, despite the name.
    assert_eq!(classify("buddy-tokens.json"), Disposition::Leave);
}

#[test]
fn config_json_is_patched_not_moved_or_left() {
    // It mixes oauth:tokenCache with locale, userThemeMode and window
    // sizing, so neither moving it nor ignoring it is correct.
    assert_eq!(classify("config.json"), Disposition::Patch);
}

#[test]
fn an_unrecognised_entry_moves() {
    // The denylist decision (design §3): state byte has not identified --
    // including whatever a future app version adds -- travels with the
    // account rather than being silently left behind. An allowlist would
    // return Leave here and quietly strand a future session store.
    assert_eq!(classify("SomethingNewInVersion42"), Disposition::Move);
}

#[test]
fn classification_ignores_case() {
    assert_eq!(classify("cache"), Disposition::Leave);
    assert_eq!(classify("CACHE"), Disposition::Leave);
}

#[test]
fn movable_entries_lists_only_what_moves_and_skips_a_missing_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir(root.join("Network")).unwrap();
    std::fs::create_dir(root.join("Cache")).unwrap();
    std::fs::create_dir(root.join("claude-code")).unwrap();
    std::fs::write(root.join("config.json"), b"{}").unwrap();

    let mut got = movable_entries(root).unwrap();
    got.sort();
    assert_eq!(got, vec!["Network".to_string()]);

    // A directory that does not exist yet is not an error: a fresh install
    // or a first capture has nothing to enumerate.
    assert_eq!(
        movable_entries(&root.join("nope")).unwrap(),
        Vec::<String>::new()
    );
}
