//! Unit-level coverage for `src/cli/run.rs`'s decision logic, as opposed to
//! `tests/cli_test.rs`, which drives the compiled binary as a black box.
//!
//! `resolve_add_failure` takes the outcome of an already-attempted restore
//! rather than a `Switcher` and performing the restore itself, specifically
//! so this branching is testable with no `SecretStore`, no file I/O, and no
//! keychain at all -- see the comment on the function itself for why this
//! matters: it is the exact piece of logic task-11 review Finding 1 found
//! broken (a bare `?` that skipped recovery entirely on a `poll_once`
//! error).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use byte::Error;
use byte::claude::detect::FakeProbe;
use byte::claude::files::ClaudeFiles;
use byte::claude::snapshot::SCHEMA_VERSION;
use byte::cli::run::{
    cmd_add, cmd_switch, desktop_paths_if_installed, format_size, identity_mismatch_message,
    recover_under_lock, remove_prompt, repair_message, resolve_add_failure,
    running_sessions_warning, switch_json,
};
use byte::desktop::paths::{DesktopPaths, TestDesktopPaths};
use byte::desktop::swap::{IdentityRepair, Recovery, Repair};
use byte::lock::MutationGuard;
use byte::ops::desktop::DesktopOutcome;
use byte::ops::manage;
use byte::ops::switch::{SwitchOutcome, Switcher, SyncOutcome};
use byte::paths::{HostPaths, TestPaths};
use byte::store::metadata::AccountMeta;
use byte::store::secrets::MemoryStore;
use serde_json::json;

fn meta(uuid: &str, label: &str) -> AccountMeta {
    AccountMeta {
        uuid: uuid.to_string(),
        label: label.to_string(),
        email: None,
        organization_name: None,
        subscription_type: None,
        account: json!({}),
        user_id: None,
        credential_schema: SCHEMA_VERSION,
        added_at: "2026-01-01T00:00:00Z".to_string(),
        last_used_at: None,
        desktop_profile: None,
    }
}

#[test]
fn switch_json_reports_a_captured_sync_outcome() {
    // Finding M3: `switch --json` silently dropped sync-back's outcome, so
    // a script had no way to learn that sync-back just wrote a previously
    // unknown account's refresh token to the keychain.
    let outcome = SwitchOutcome {
        switched_to: meta("target-uuid", "work"),
        sync: SyncOutcome::Captured(meta("live-uuid", "personal")),
        already_active: false,
    };

    let value = switch_json(&outcome, None);

    assert_eq!(value["sync"]["outcome"], "captured");
    assert_eq!(value["sync"]["uuid"], "live-uuid");
    assert_eq!(value["sync"]["label"], "personal");
    assert_eq!(value["switched_to"], "work");
    assert_eq!(value["already_active"], false);
}

#[test]
fn switch_json_reports_an_updated_sync_outcome() {
    let outcome = SwitchOutcome {
        switched_to: meta("target-uuid", "work"),
        sync: SyncOutcome::Updated(meta("target-uuid", "work")),
        already_active: true,
    };

    let value = switch_json(&outcome, None);

    assert_eq!(value["sync"]["outcome"], "updated");
    assert_eq!(value["already_active"], true);
}

#[test]
fn switch_json_reports_a_logged_out_sync_outcome_without_an_account() {
    let outcome = SwitchOutcome {
        switched_to: meta("target-uuid", "work"),
        sync: SyncOutcome::LoggedOut,
        already_active: false,
    };

    let value = switch_json(&outcome, None);

    assert_eq!(value["sync"]["outcome"], "logged_out");
    // LoggedOut carries no account -- confirm sync_json doesn't fabricate a
    // uuid/label field for it the way the other two variants have.
    assert!(value["sync"].get("uuid").is_none());
}

// `cmd_switch`'s success path needs a keychain (it is generic over
// `SecretStore`, but the compiled binary always wires it to `KeyringStore`
// via `run()`), so the running-sessions warning is asserted here at the
// level of the pure message builder rather than by driving `cmd_switch` (or
// the binary) end to end -- see the file header and
// `running_sessions_warning`'s own doc comment.

#[test]
fn no_warning_when_nothing_is_running() {
    assert_eq!(running_sessions_warning(0), None);
}

#[test]
fn one_session_is_described_in_the_singular() {
    let msg = running_sessions_warning(1).expect("a warning");
    assert!(msg.contains('1'), "should name the count: {msg}");
    assert!(!msg.contains("sessions"), "should be singular: {msg}");
}

#[test]
fn several_sessions_are_described_in_the_plural() {
    let msg = running_sessions_warning(3).expect("a warning");
    assert!(msg.contains('3'), "should name the count: {msg}");
    assert!(msg.contains("sessions"), "should be plural: {msg}");
}

// The three tests above pin count-formatting and singular/plural wording via
// substring checks alone, which a sloppy (but technically passing) message
// could still satisfy -- e.g. "1 thing needs attention" contains '1' and
// omits "sessions" without saying anything useful. Pin the exact wording too
// so a regression there (dropped restart instruction, wrong verb, mangled
// punctuation) fails a test instead of shipping silently.
#[test]
fn one_session_message_is_worded_exactly() {
    assert_eq!(
        running_sessions_warning(1).as_deref(),
        Some(
            "1 running Claude Code session still uses the previous account. \
             Restart it to pick up the switch."
        )
    );
}

#[test]
fn plural_session_message_is_worded_exactly() {
    assert_eq!(
        running_sessions_warning(3).as_deref(),
        Some(
            "3 running Claude Code sessions still use the previous account. \
             Restart them to pick up the switch."
        )
    );
}

// 3 alone leaves the singular/plural boundary at 2 unexercised -- the match
// arm covering `n` starts at 2, not 3, so pin that boundary explicitly
// rather than trusting it's covered by a test for a larger count.
#[test]
fn two_sessions_is_already_the_plural_boundary() {
    assert_eq!(
        running_sessions_warning(2).as_deref(),
        Some(
            "2 running Claude Code sessions still use the previous account. \
             Restart them to pick up the switch."
        )
    );
}

#[test]
fn reports_the_original_cause_when_the_restore_succeeds() {
    let result = resolve_add_failure(Error::LoginTimeout(5), true, Ok(()));

    assert!(matches!(result, Err(Error::LoginTimeout(5))));
}

#[test]
fn reports_the_restore_failure_rather_than_the_original_cause_when_both_fail() {
    // This is the failure mode the fix exists to prevent: if only the
    // *original* cause were returned here, the caller (and the user) would
    // never learn that the restore attempt -- their one path back to being
    // logged in -- also failed.
    let result = resolve_add_failure(Error::LoginTimeout(5), true, Err(Error::NotLoggedIn));

    assert!(matches!(result, Err(Error::NotLoggedIn)));
}

#[test]
fn distinguishes_a_poll_error_cause_from_a_timeout_cause() {
    // Not just "some Err comes back" -- confirms the specific cause passed
    // in is the one that surfaces when the restore succeeds, for the other
    // shape of failure cmd_add can report (a poll_once error, not just a
    // timeout).
    let result = resolve_add_failure(Error::NotLoggedIn, true, Ok(()));

    assert!(matches!(result, Err(Error::NotLoggedIn)));
}

#[test]
fn had_previous_does_not_change_which_error_propagates() {
    // Finding M7: resolve_add_failure claimed "Restored the previous
    // account" whenever restore_result was Ok(()), even when there was no
    // previous account to restore -- AddSession::abort()'s None branch also
    // returns Ok(()) unconditionally. had_previous exists to fix the STATUS
    // TEXT for that case (see tests/cli_test.rs for an end-to-end
    // assertion on the actual stderr wording, which this module-level
    // function can't observe on its own -- see the file header). What this
    // test pins is the property `#[test]` code CAN observe here: had_previous
    // must only change the message, never which Err propagates.
    let with_previous = resolve_add_failure(Error::LoginTimeout(5), true, Ok(()));
    let without_previous = resolve_add_failure(Error::LoginTimeout(5), false, Ok(()));

    assert!(matches!(with_previous, Err(Error::LoginTimeout(5))));
    assert!(matches!(without_previous, Err(Error::LoginTimeout(5))));
}

fn login_as(tp: &TestPaths, uuid: &str, email: &str, refresh: &str) {
    std::fs::write(
        tp.claude_credentials(),
        serde_json::to_string(&json!({
            "claudeAiOauth": {
                "accessToken": "a", "refreshToken": refresh, "expiresAt": 1i64
            }
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

/// A `HostPaths` that corrupts `.claude.json` the moment it is asked for
/// that path for the third time, then "finishes the write" (heals it) on
/// the fourth -- timed to land on `cmd_add`'s first `poll_once` call (calls
/// 1 and 2 are `AddSession::begin`'s own `capture_current` and `clear`),
/// simulating catching Claude Code mid-write without needing real
/// concurrency. Mirrors the corrupt-then-fix sequence
/// `tests/add_test.rs::poll_failure_does_not_prevent_recovering_the_previous_account`
/// drives by hand; this drives it through the real `cmd_add`, end to end.
struct FlakyPaths<'a> {
    inner: &'a TestPaths,
    claude_config_calls: AtomicU32,
}

impl HostPaths for FlakyPaths<'_> {
    fn claude_config(&self) -> PathBuf {
        let n = self.claude_config_calls.fetch_add(1, Ordering::SeqCst);
        let path = self.inner.claude_config();
        if n == 2 {
            std::fs::write(&path, "{ not valid json").unwrap();
        } else if n == 3 {
            // The interrupted write completes a moment later, same as it
            // would in reality -- this models Claude Code's own write
            // finishing, not byte fixing anything.
            std::fs::write(&path, "{}").unwrap();
        }
        path
    }

    fn claude_credentials(&self) -> PathBuf {
        self.inner.claude_credentials()
    }

    fn byte_config_dir(&self) -> PathBuf {
        self.inner.byte_config_dir()
    }
}

#[test]
fn cmd_add_restores_the_previous_account_when_poll_once_fails() {
    // Finding I8: the CLI layer's wiring around cmd_add -- does it call
    // abort() before reporting a poll_once failure, does the poll loop
    // terminate -- had zero automated coverage, because every cmd_*
    // function was concretely typed over KeyringStore, forcing a real
    // keychain write to reach it at all. Now that they're generic over
    // `S: SecretStore`, this drives the real `cmd_add` end to end against a
    // `MemoryStore`, reproducing the shape of the original task-11 Finding
    // F26 bug it guards against: a `poll_once` failure must not leave the
    // user logged out with no attempt to recover.
    let tp = TestPaths::new().unwrap();
    login_as(&tp, "u1", "a@example.com", "r1");
    let paths = FlakyPaths {
        inner: &tp,
        claude_config_calls: AtomicU32::new(0),
    };
    let sw = Switcher::new(&paths, MemoryStore::new());

    // `yes = true`: this test is about the poll-failure path, which sits
    // past the confirmation gate added for the tray's Add account item.
    let result = cmd_add(&sw, 300, true, false);

    // The poll_once error (Error::Parse, from the corrupted .claude.json)
    // is what must be reported -- proving cmd_add actually observed the
    // failure, rather than looping past it or hanging until the 300s
    // timeout this test would otherwise be at the mercy of.
    assert!(
        matches!(result, Err(Error::Parse { .. })),
        "expected the poll_once Parse error to propagate, got: {result:?}"
    );

    // The property that actually matters: abort() ran and put u1 back as
    // the live account, so the user is not left logged out.
    let restored = ClaudeFiles::new(&paths).capture().unwrap().unwrap();
    assert_eq!(restored.email(), Some("a@example.com"));
}

#[test]
fn cmd_add_settles_confirmation_before_taking_the_mutation_lock() {
    // Ordering, pinned by holding the lock from underneath. The
    // confirmation prompt blocks on stdin with no timeout, so a lock taken
    // before it is pinned open for as long as nobody answers -- and the
    // tray opens exactly that prompt, in a spawned terminal, on one click.
    // An unanswered prompt would then make every tray menu account and
    // every CLI mutation fail with `Error::Busy` indefinitely.
    //
    // With another guard held, an implementation that locks first reports
    // Busy; one that checks confirmation first reports ConfirmationRequired.
    // `json = true` short-circuits before `is_terminal()`, so this cannot
    // block on an interactive stdin.
    let tp = TestPaths::new().unwrap();
    let _held = MutationGuard::acquire(&tp).expect("first guard should acquire");

    let sw = Switcher::new(&tp, MemoryStore::new());
    let result = cmd_add(&sw, 300, false, true);

    assert!(
        matches!(result, Err(Error::ConfirmationRequired { .. })),
        "the confirmation gate must precede the lock; got {result:?}"
    );
}

// The tests below cover Task 9's desktop wiring in `cmd_switch`: the
// desktop half runs after the Claude Code switch commits, is skipped when
// there is no `DesktopPaths` to attempt it against, and never turns a
// failure of its own into a failure of the switch that already committed.
// Driven directly against `cmd_switch` with a `MemoryStore`, exactly like
// `cmd_add_restores_the_previous_account_when_poll_once_fails` above --
// `tests/cli_test.rs` cannot exercise a SUCCESSFUL switch through the
// compiled binary at all, since that reaches a real OS keychain (see that
// file's own header comment).

#[test]
fn cmd_switch_moves_the_desktop_profile_too() {
    // Proves the actual wiring this task adds: cmd_switch must call
    // switch_desktop with the outgoing/incoming uuids the Claude Code
    // switch just settled, not skip it or get the direction backwards.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    login_as(&tp, "u1", "a@example.com", "r1");
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.capture_current().unwrap();
    login_as(&tp, "u2", "b@example.com", "r2");
    sw.capture_current().unwrap();

    // u2 is live right now, with a session in the desktop app; u1 has a
    // previously stored desktop profile waiting to be installed.
    let live = dp.desktop_dir().join("Network");
    std::fs::create_dir_all(&live).unwrap();
    std::fs::write(live.join("marker.txt"), "account-u2").unwrap();
    let stored = tp.desktop_profile_dir("u1").join("Network");
    std::fs::create_dir_all(&stored).unwrap();
    std::fs::write(stored.join("marker.txt"), "account-u1").unwrap();

    let result = cmd_switch(
        &sw,
        "a@example.com",
        false,
        &FakeProbe::with_count(0),
        Some(&dp),
    );

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        std::fs::read_to_string(dp.desktop_dir().join("Network/marker.txt")).unwrap(),
        "account-u1",
        "the desktop half must have installed u1's stored session"
    );
    assert_eq!(
        std::fs::read_to_string(tp.desktop_profile_dir("u2").join("Network/marker.txt")).unwrap(),
        "account-u2",
        "u2's session must have been parked, not discarded"
    );
}

#[test]
fn a_failing_desktop_half_does_not_fail_the_already_committed_switch() {
    // Regression guard for the rule stated on `switch_desktop`'s call site
    // in `cmd_switch`: a desktop failure must never turn an
    // already-committed Claude Code switch into an `Err` -- the same
    // "never fail an operation that already succeeded" rule `notify::send`
    // and `atomic::prune` already follow elsewhere in this codebase.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    login_as(&tp, "u1", "a@example.com", "r1");
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.capture_current().unwrap();
    login_as(&tp, "u2", "b@example.com", "r2");
    sw.capture_current().unwrap();

    // Force switch_desktop to fail deterministically, before it writes
    // anything: an unrepaired journal already on disk, which switch_desktop
    // refuses to start a new swap over (see its precondition check in
    // src/ops/desktop.rs).
    std::fs::create_dir_all(tp.desktop_store_dir()).unwrap();
    std::fs::write(tp.desktop_journal_file(), "not a real journal").unwrap();

    let result = cmd_switch(
        &sw,
        "a@example.com",
        false,
        &FakeProbe::with_count(0),
        Some(&dp),
    );

    assert!(
        result.is_ok(),
        "a failing desktop half must not fail an already-committed Claude Code switch: {result:?}"
    );
    // The property the rule actually protects: the Claude Code switch
    // really did commit, not just "some Ok(()) came back".
    let creds: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tp.claude_credentials()).unwrap()).unwrap();
    assert_eq!(creds["claudeAiOauth"]["refreshToken"], json!("r1"));
}

#[test]
fn cmd_switch_skips_the_desktop_half_when_no_desktop_paths_are_available() {
    // Mirrors production: `RealDesktopPaths::discover()` fails on every
    // platform with no `%APPDATA%\Claude` concept (every non-Windows
    // machine, absent CLAUDE_DESKTOP_DIR), and `run()` degrades that to
    // `None` rather than failing `byte switch` outright. `cmd_switch` must
    // then not create or touch anything under byte's desktop store.
    let tp = TestPaths::new().unwrap();
    login_as(&tp, "u1", "a@example.com", "r1");
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.capture_current().unwrap();
    login_as(&tp, "u2", "b@example.com", "r2");
    sw.capture_current().unwrap();

    let no_desktop: Option<&TestDesktopPaths> = None;
    let result = cmd_switch(
        &sw,
        "a@example.com",
        false,
        &FakeProbe::with_count(0),
        no_desktop,
    );

    assert!(result.is_ok(), "{result:?}");
    let creds: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tp.claude_credentials()).unwrap()).unwrap();
    assert_eq!(creds["claudeAiOauth"]["refreshToken"], json!("r1"));
    assert!(
        !tp.desktop_store_dir().exists(),
        "no desktop paths means the desktop half must not run at all"
    );
}

#[test]
fn a_machine_without_claude_desktop_installed_has_no_state_manufactured_for_it() {
    // `RealDesktopPaths::discover()` succeeds whenever `%APPDATA%` is set,
    // which is every interactive Windows user -- installed app or not. Acting
    // on that alone made `byte switch` create `%APPDATA%\Claude\config.json`
    // holding `{}` for an application that has never run (via
    // `config::clear` -> `JsonDocument::save` -> `atomic::write`'s
    // `create_dir_all`), park a junk all-nulls `oauth.json` for the outgoing
    // account, stamp a `desktop_profile` record so `byte list` reported a
    // desktop session that does not exist, and print a message on every
    // switch -- while `docs/troubleshooting.md` promised silence.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::uninstalled().unwrap();
    login_as(&tp, "u1", "a@example.com", "r1");
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.capture_current().unwrap();
    login_as(&tp, "u2", "b@example.com", "r2");
    sw.capture_current().unwrap();

    let desktop = desktop_paths_if_installed(Some(&dp));
    assert!(
        desktop.is_none(),
        "a directory that is not there is not an installed app"
    );

    cmd_switch(
        &sw,
        "a@example.com",
        false,
        &FakeProbe::with_count(0),
        desktop,
    )
    .unwrap();

    assert!(
        !dp.desktop_dir().exists(),
        "byte must not create the desktop app's data directory for an app that is not installed"
    );
    assert!(
        !tp.desktop_store_dir().exists(),
        "nor a profile store for a desktop session that cannot exist"
    );
    let listing = manage::list(&sw).unwrap();
    assert!(
        listing.iter().all(|l| l.meta.desktop_profile.is_none()),
        "and `byte list` must not report a stored desktop session either"
    );
}

#[test]
fn an_installed_claude_desktop_is_not_gated_away() {
    // The other half: the gate must only exclude a machine where the app's
    // data directory is genuinely absent, or it would silently disable the
    // whole desktop half.
    let dp = TestDesktopPaths::new().unwrap();
    assert!(desktop_paths_if_installed(Some(&dp)).is_some());
    let nothing: Option<&TestDesktopPaths> = None;
    assert!(desktop_paths_if_installed(nothing).is_none());
}

#[test]
fn switch_json_reports_an_uninstalled_desktop_app_as_null() {
    // The documented `--json` contract for this case: `desktop` is `null`,
    // exactly as for a no-op switch, and the key is still present so a
    // script can tell it from an older byte that never emitted the field.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::uninstalled().unwrap();
    login_as(&tp, "u1", "a@example.com", "r1");
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.capture_current().unwrap();
    login_as(&tp, "u2", "b@example.com", "r2");
    sw.capture_current().unwrap();

    let outcome = sw.switch_to("a@example.com").unwrap();
    let value = switch_json(&outcome, None);

    assert!(desktop_paths_if_installed(Some(&dp)).is_none());
    assert!(value.get("desktop").is_some());
    assert_eq!(value["desktop"], serde_json::Value::Null);
}

#[test]
fn remove_names_the_desktop_session_only_when_there_is_one() {
    // `byte remove` deletes the parked desktop profile as well, and that is
    // the most consequential thing it destroys -- a live claude.ai session as
    // plain files. A prompt that leaves it inside "its stored credentials"
    // is asking for consent the user has not knowingly given.
    let with = remove_prompt("work", true);
    assert!(
        with.contains("Claude Desktop session"),
        "the session has to be named when one exists: {with}"
    );
    assert!(with.contains("cannot be recovered afterward"));

    let without = remove_prompt("work", false);
    assert!(
        !without.contains("Claude Desktop"),
        "and must not be mentioned when there is none: {without}"
    );
    assert_eq!(
        without,
        "Remove 'work'? Its stored credentials cannot be recovered afterward."
    );
}

#[test]
fn cmd_switch_moves_the_desktop_profile_in_json_mode_too() {
    // Review finding: `--json` used to return before the desktop block was
    // reached, so a script driving `byte switch --json` was left with Claude
    // Desktop still signed in as the previous account -- and nothing in the
    // payload said so. The desktop half now runs for both output modes; only
    // the reporting differs.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    login_as(&tp, "u1", "a@example.com", "r1");
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.capture_current().unwrap();
    login_as(&tp, "u2", "b@example.com", "r2");
    sw.capture_current().unwrap();

    let live = dp.desktop_dir().join("Network");
    std::fs::create_dir_all(&live).unwrap();
    std::fs::write(live.join("marker.txt"), "account-u2").unwrap();
    let stored = tp.desktop_profile_dir("u1").join("Network");
    std::fs::create_dir_all(&stored).unwrap();
    std::fs::write(stored.join("marker.txt"), "account-u1").unwrap();

    let result = cmd_switch(
        &sw,
        "a@example.com",
        true,
        &FakeProbe::with_count(0),
        Some(&dp),
    );

    assert!(result.is_ok(), "{result:?}");
    // The Claude Code switch itself still committed under --json.
    let creds: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tp.claude_credentials()).unwrap()).unwrap();
    assert_eq!(creds["claudeAiOauth"]["refreshToken"], json!("r1"));
    assert_eq!(
        std::fs::read_to_string(dp.desktop_dir().join("Network/marker.txt")).unwrap(),
        "account-u1",
        "--json must install the incoming account's stored desktop session too"
    );
    assert_eq!(
        std::fs::read_to_string(tp.desktop_profile_dir("u2").join("Network/marker.txt")).unwrap(),
        "account-u2",
        "...and park the outgoing account's, not discard it"
    );
}

#[test]
fn a_failing_desktop_half_does_not_fail_a_json_switch_either() {
    // The desktop half must never fail the command in EITHER output mode:
    // under --json the failure becomes `"desktop": "failed"` (pinned by
    // `switch_json_reports_a_failed_desktop_half` below) plus the same
    // stderr warning, and stdout still carries a valid payload.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    login_as(&tp, "u1", "a@example.com", "r1");
    let sw = Switcher::new(&tp, MemoryStore::new());
    sw.capture_current().unwrap();
    login_as(&tp, "u2", "b@example.com", "r2");
    sw.capture_current().unwrap();

    // The same deterministic failure the non-JSON test above uses: an
    // unrepaired journal, which switch_desktop refuses to swap over.
    std::fs::create_dir_all(tp.desktop_store_dir()).unwrap();
    std::fs::write(tp.desktop_journal_file(), "not a real journal").unwrap();

    let result = cmd_switch(
        &sw,
        "a@example.com",
        true,
        &FakeProbe::with_count(0),
        Some(&dp),
    );

    assert!(
        result.is_ok(),
        "a failing desktop half must not fail a --json switch either: {result:?}"
    );
    let creds: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tp.claude_credentials()).unwrap()).unwrap();
    assert_eq!(creds["claudeAiOauth"]["refreshToken"], json!("r1"));
}

// `desktop_json`'s mapping is pinned through `switch_json`, the payload a
// script actually parses, rather than as a private helper: what matters is
// the field's name and its value in the real envelope, not that some
// function returns a string.

#[test]
fn switch_json_payload_parses_and_carries_the_desktop_outcome() {
    let outcome = SwitchOutcome {
        switched_to: meta("target-uuid", "work"),
        sync: SyncOutcome::Updated(meta("live-uuid", "personal")),
        already_active: false,
    };

    // Round-tripped through text on purpose: `--json` writes this to stdout
    // and a script parses it back, so the contract is what survives that.
    let text = switch_json(&outcome, Some(&Ok(DesktopOutcome::Switched))).to_string();
    let parsed: serde_json::Value =
        serde_json::from_str(&text).expect("--json stdout must stay parseable");

    assert_eq!(parsed["desktop"], json!("switched"));
    assert_eq!(parsed["switched_to"], json!("work"));
    assert_eq!(parsed["sync"]["outcome"], json!("updated"));
}

#[test]
fn switch_json_names_every_desktop_outcome_in_lower_snake_case() {
    let outcome = SwitchOutcome {
        switched_to: meta("target-uuid", "work"),
        sync: SyncOutcome::LoggedOut,
        already_active: false,
    };
    let field = |d| switch_json(&outcome, Some(&Ok(d)))["desktop"].clone();

    assert_eq!(field(DesktopOutcome::Switched), json!("switched"));
    assert_eq!(field(DesktopOutcome::AppRunning), json!("app_running"));
    assert_eq!(
        field(DesktopOutcome::NoProfileForIncoming),
        json!("no_profile_for_incoming")
    );
    assert_eq!(
        field(DesktopOutcome::IdentityMismatch),
        json!("identity_mismatch")
    );
    // Distinct from `switched`, and deliberately so: the profile moved but
    // the app will open signed out, and a script driving `--json` has to be
    // able to tell those apart.
    assert_eq!(
        field(DesktopOutcome::SwitchedWithoutIdentity),
        json!("switched_without_identity")
    );
    assert_eq!(field(DesktopOutcome::NothingToDo), json!("nothing_to_do"));
    // Distinct from `identity_mismatch`: this is byte refusing to use the
    // INCOMING account's own identifier, not a live-session disagreement
    // about the outgoing side, and a script needs to be able to tell them
    // apart too.
    assert_eq!(
        field(DesktopOutcome::IncomingIdentifierInvalid),
        json!("incoming_identifier_invalid")
    );
}

// `identity_mismatch_message` is reached for two families of reason (see its
// own doc comment): the desktop app disagrees with an account Claude Code
// already knows about, or Claude Code itself had none to compare against.
// Only the second is fixed by logging in to Claude Code -- the first is
// fixed by fixing the desktop app's own sign-in -- so the two must not say
// the same thing.

#[test]
fn identity_mismatch_message_names_the_desktop_app_fix_when_claude_code_had_an_account() {
    let msg = identity_mismatch_message(false);

    assert!(
        msg.contains("sign out of Claude Desktop and sign in again"),
        "Claude Code already had an account to compare against, so the fix is in the app: {msg}"
    );
    assert!(
        !msg.contains("Log in to Claude Code"),
        "that advice is a no-op here -- Claude Code was never the problem: {msg}"
    );
}

#[test]
fn identity_mismatch_message_names_logging_into_claude_code_when_it_had_no_account() {
    // The refusal case this message previously left unaddressed: Claude
    // Code logged out, the desktop app reporting no identity byte can use,
    // over a live directory that still holds a session. Signing out of an
    // app that already reports no identity is a no-op; what actually
    // restores the capability is logging in to Claude Code, so byte has an
    // account to attribute the live session to on the next switch.
    let msg = identity_mismatch_message(true);

    assert!(
        msg.contains("Log in to Claude Code"),
        "the route that actually restores the capability must be named on screen: {msg}"
    );
    assert!(
        !msg.contains("sign out of Claude Desktop and sign in again"),
        "that advice is a no-op when the app already reports no identity: {msg}"
    );
}

// A repair now has two halves -- the renames and the patch of the desktop
// app's own `config.json` -- which can finish independently. The message has
// to say which, because `docs/troubleshooting.md` reads the plain wording as
// "nothing further is needed".

#[test]
fn a_fully_repaired_swap_is_reported_plainly() {
    let msg = repair_message(&Repair {
        recovery: Recovery::RollForward,
        identity: IdentityRepair::Restored,
    });

    assert_eq!(
        msg,
        "repaired an interrupted desktop profile swap (RollForward)."
    );
    assert_eq!(
        repair_message(&Repair {
            recovery: Recovery::Reverse,
            identity: IdentityRepair::NotNeeded,
        }),
        "repaired an interrupted desktop profile swap (Reverse).",
        "nothing to restore is still a finished repair"
    );
}

#[test]
fn a_repair_that_could_not_reach_the_config_says_what_is_outstanding() {
    let msg = repair_message(&Repair {
        recovery: Recovery::Reverse,
        identity: IdentityRepair::Deferred,
    });

    assert!(
        msg.contains("directory half"),
        "the user must not be told the whole swap was repaired: {msg}"
    );
    assert!(
        msg.contains("journal has been kept"),
        "and must be told why a journal is still on disk: {msg}"
    );
}

#[test]
fn a_repair_naming_an_unusable_account_identifier_keeps_the_journal_and_says_so() {
    let msg = repair_message(&Repair {
        recovery: Recovery::RollForward,
        identity: IdentityRepair::AccountIdentifierInvalid("..".to_string()),
    });

    assert!(
        msg.contains("journal has been kept"),
        "this is not a finished repair -- the journal must not read as resolved: {msg}"
    );
    assert!(
        msg.contains(".."),
        "the unusable identifier itself belongs in the message: {msg}"
    );
}

#[test]
fn a_repair_with_an_unreadable_saved_sign_in_names_the_file() {
    let msg = repair_message(&Repair {
        recovery: Recovery::RollForward,
        identity: IdentityRepair::Unreadable(PathBuf::from("/store/u1/oauth.json")),
    });

    assert!(
        msg.contains("oauth.json"),
        "the file the user has to look at must be in the message: {msg}"
    );
    assert!(
        msg.contains("signed out"),
        "and what they will actually see when they open the app: {msg}"
    );
}

#[test]
fn a_reversal_with_an_unreadable_saved_sign_in_does_not_predict_a_signed_out_app() {
    // The same `IdentityRepair` means opposite things in the two directions,
    // and `restore_identity` says so: a roll-forward CLEARS the app's account
    // keys rather than leave the outgoing account's over the incoming
    // account's cookies, so the app really does open signed out. A reversal
    // writes nothing at all -- `config.json` still describes the very session
    // the reversal just put back -- so the app opens as that account and
    // telling the user otherwise sends them to sign in over a working
    // session.
    let msg = repair_message(&Repair {
        recovery: Recovery::Reverse,
        identity: IdentityRepair::Unreadable(PathBuf::from("/store/u1/oauth.json")),
    });

    assert!(
        msg.contains("oauth.json"),
        "the file the user has to look at must still be named: {msg}"
    );
    assert!(
        !msg.contains("signed out"),
        "a reversal left config.json describing the restored session, so the app opens as that \
         account: {msg}"
    );
}

#[test]
fn switch_json_reports_a_failed_desktop_half() {
    // A desktop failure never fails the command, so the payload is the only
    // machine-readable place it can appear. The error's own text is
    // deliberately NOT in the payload: `Error`'s Display is for humans and
    // its wording is not a contract.
    let outcome = SwitchOutcome {
        switched_to: meta("target-uuid", "work"),
        sync: SyncOutcome::LoggedOut,
        already_active: false,
    };

    let value = switch_json(&outcome, Some(&Err(Error::NotLoggedIn)));

    assert_eq!(value["desktop"], json!("failed"));
}

#[test]
fn switch_json_reports_a_desktop_half_that_was_not_attempted_as_null() {
    // "Not attempted" (an already-active no-op, or a platform with no
    // desktop paths at all) is a different fact from "this build has never
    // heard of the field", so the key is always present -- explicitly null,
    // exactly like `listing_json`'s `desktop_profile`.
    let outcome = SwitchOutcome {
        switched_to: meta("target-uuid", "work"),
        sync: SyncOutcome::Updated(meta("target-uuid", "work")),
        already_active: true,
    };

    let value = switch_json(&outcome, None);

    assert!(
        value.get("desktop").is_some(),
        "the key must be present even when the desktop half was not attempted"
    );
    assert_eq!(value["desktop"], serde_json::Value::Null);
}

#[test]
fn recovery_is_skipped_while_another_process_holds_the_mutation_lock() {
    // Review finding: recovery used to run before any lock was taken, and
    // `list`/`current`/`autostart` never take one at all -- so a `byte list`
    // during an ordinary `byte switch` would read the SWITCH'S OWN live
    // journal (written before the first rename, cleared only after the
    // last), see no completed install, compute `Recovery::Reverse`, rename
    // the directories backwards underneath the running swap, and then clear
    // the only record of it.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();

    // Exactly the state `swap::execute` is in mid-swap: the park has landed,
    // no install has, and the full journal is on disk.
    let parked = tp.desktop_profile_dir("u1").join("Network");
    std::fs::create_dir_all(&parked).unwrap();
    std::fs::write(parked.join("marker.txt"), "account-1").unwrap();
    let journal = json!({
        "version": 1,
        "outgoing": "u1",
        "incoming": null,
        "moves": [{
            "stage": "Park",
            "from": dp.desktop_dir().join("Network"),
            "to": parked,
            "done": true
        }]
    })
    .to_string();
    std::fs::write(tp.desktop_journal_file(), &journal).unwrap();

    // Stands in for the other byte process that is mid-swap right now: the
    // lock is per open file handle, so this refuses a second acquisition
    // even from the same process (see tests/lock_test.rs).
    let _held = MutationGuard::acquire(&tp).expect("the lock must start free");

    recover_under_lock(&tp, Some(&dp));

    assert_eq!(
        std::fs::read_to_string(tp.desktop_journal_file()).unwrap_or_default(),
        journal,
        "the in-flight swap's journal must be left byte-for-byte as it was, not cleared"
    );
    assert_eq!(
        std::fs::read_to_string(parked.join("marker.txt")).unwrap(),
        "account-1",
        "the parked directory must not be renamed backwards under the running swap"
    );
    assert!(
        !dp.desktop_dir().join("Network").exists(),
        "nothing may be restored into the live directory while the swap owning it runs"
    );
}

#[test]
fn recovery_still_runs_when_the_lock_is_free() {
    // The other half of the gate: with no other process mid-mutation, a
    // journal on disk really is wreckage and must still be repaired -- the
    // fix must not have turned recovery off altogether.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();

    let parked = tp.desktop_profile_dir("u1").join("Network");
    std::fs::create_dir_all(&parked).unwrap();
    std::fs::write(parked.join("marker.txt"), "account-1").unwrap();
    // A second entry still sitting in the live directory: the park is
    // genuinely half-done, which is what makes this an interrupted swap
    // rather than a finished one whose journal outlived it. A journal with
    // nothing outstanding rolls forward instead -- see
    // `swap::recovery_for`.
    let still_live = dp.desktop_dir().join("IndexedDB");
    std::fs::create_dir_all(&still_live).unwrap();
    std::fs::write(still_live.join("marker.txt"), "account-1").unwrap();
    std::fs::write(
        tp.desktop_journal_file(),
        json!({
            "version": 1,
            "outgoing": "u1",
            "incoming": null,
            "moves": [
                {
                    "stage": "Park",
                    "from": dp.desktop_dir().join("Network"),
                    "to": parked,
                    "done": true
                },
                {
                    "stage": "Park",
                    "from": still_live,
                    "to": tp.desktop_profile_dir("u1").join("IndexedDB"),
                    "done": false
                }
            ]
        })
        .to_string(),
    )
    .unwrap();

    recover_under_lock(&tp, Some(&dp));

    assert_eq!(
        std::fs::read_to_string(dp.desktop_dir().join("Network/marker.txt")).unwrap(),
        "account-1",
        "no install completed and the park is unfinished, so it must be reversed \
         and the session restored"
    );
    assert_eq!(
        std::fs::read_to_string(still_live.join("marker.txt")).unwrap(),
        "account-1",
        "the entry that was never parked must still be live"
    );
    assert!(
        !tp.desktop_journal_file().exists(),
        "a repaired journal must be cleared, or every later command repeats the repair"
    );
}

#[test]
fn format_size_reports_bytes_plainly_under_a_kilobyte() {
    assert_eq!(format_size(0), "0 B");
    assert_eq!(format_size(512), "512 B");
    // The exact boundary, from below: one byte short of a kilobyte is still
    // a plain byte count, with no decimal point.
    assert_eq!(format_size(1023), "1023 B");
}

#[test]
fn format_size_reports_larger_sizes_with_one_decimal_and_a_unit() {
    // Every unit the table carries gets named, in order. Without the
    // kilobyte case a table whose second entry was wrong -- or missing
    // entirely -- would still satisfy the megabyte assertion below.
    assert_eq!(format_size(1024), "1.0 KB");
    assert_eq!(format_size(1536), "1.5 KB");
    assert_eq!(format_size(2 * 1024 * 1024), "2.0 MB");
    assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    assert_eq!(format_size(4 * 1024 * 1024 * 1024 * 1024), "4.0 TB");
}
