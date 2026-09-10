use std::path::Path;

use byte::desktop::journal::{Journal, Move, Stage};
use byte::desktop::paths::{DesktopPaths, TestDesktopPaths};
use byte::desktop::swap::{
    IdentityRepair, Recovery, Repair, clear_journal, create_store_dir, execute,
    recover_if_interrupted, recovery_for,
};
use byte::paths::{HostPaths, TestPaths};

/// Repair with a throwaway desktop directory attached.
///
/// A swap is not only its renames: the desktop app decides which account it
/// is signed in as from keys in its own `config.json`, and recovery finishes
/// that half too. So even a test that asserts nothing about `config.json`
/// has to give the repair somewhere to put the identity -- without it the
/// repair reports `Deferred` and deliberately KEEPS the journal, because
/// only a later command could finish it. Tests that do care about
/// `config.json`'s contents pass their own paths instead.
fn try_recover(tp: &TestPaths) -> byte::Result<Option<Repair>> {
    let dp = TestDesktopPaths::new().unwrap();
    recover_if_interrupted(tp, Some(&dp))
}

/// [`try_recover`], reporting only which way the repair went.
fn recovered(tp: &TestPaths) -> Option<Recovery> {
    try_recover(tp).unwrap().map(|r| r.recovery)
}

fn seed(dir: &Path, names: &[(&str, &str)]) {
    std::fs::create_dir_all(dir).unwrap();
    for (n, marker) in names {
        std::fs::create_dir_all(dir.join(n)).unwrap();
        std::fs::write(dir.join(n).join("marker.txt"), marker).unwrap();
    }
}

fn marker(p: &Path) -> Option<String> {
    std::fs::read_to_string(p.join("marker.txt")).ok()
}

/// Locate the move whose original entry name is `name`, for a given stage.
///
/// A journal's `moves` order for a directory comes from
/// `std::fs::read_dir` (see `movable_entries`), which makes no ordering
/// promise, so any test seeding more than one entry into the same source
/// directory must look moves up by name rather than assume a position.
fn move_index(j: &Journal, stage: Stage, name: &str) -> usize {
    j.moves
        .iter()
        .position(|m| m.stage == stage && m.from.file_name().and_then(|n| n.to_str()) == Some(name))
        .unwrap_or_else(|| panic!("no {stage:?} move named {name:?} in the plan"))
}

#[test]
fn the_profile_store_is_created_owner_only() {
    let tp = TestPaths::new().unwrap();
    create_store_dir(&tp).unwrap();
    assert!(tp.desktop_store_dir().is_dir());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(tp.desktop_store_dir())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            "the store must not be group/world readable"
        );
    }
}

#[test]
fn creating_the_store_twice_is_harmless() {
    // `switch_desktop` calls this unconditionally on every invocation, so it
    // must tolerate a directory that already exists -- including one that
    // already holds a parked profile -- rather than erroring the second time.
    let tp = TestPaths::new().unwrap();
    create_store_dir(&tp).unwrap();
    let marker = tp.desktop_store_dir().join("uuid-a");
    std::fs::create_dir_all(&marker).unwrap();

    create_store_dir(&tp).unwrap();

    assert!(marker.is_dir(), "an existing parked profile must survive");
}

#[test]
fn a_completed_swap_moves_the_live_session_out_and_the_stored_one_in() {
    let tp = TestPaths::new().unwrap();
    let live = tp.root().join("Claude");
    let park = tp.desktop_profile_dir("uuid-a");
    let take = tp.desktop_profile_dir("uuid-b");
    seed(&live, &[("Network", "account-a")]);
    seed(&take, &[("Network", "account-b")]);

    let j = Journal::plan(&live, Some(&park), Some(&take)).unwrap();
    execute(&tp, j).unwrap();

    assert_eq!(marker(&live.join("Network")).as_deref(), Some("account-b"));
    assert_eq!(marker(&park.join("Network")).as_deref(), Some("account-a"));
    assert!(
        !take.join("Network").exists(),
        "the installed profile is moved, not copied"
    );
}

#[test]
fn execute_leaves_the_journal_for_the_caller_that_finishes_the_swap() {
    // The commit boundary. `execute` runs the renames; the desktop app's own
    // `config.json` still names the outgoing account at this point, and
    // patching it is the rest of the same swap. Clearing here would put that
    // patch outside the record, so a crash -- or an ordinary `Err` from the
    // backup copy `JsonDocument::save` makes first -- would leave one
    // account's identity over another's cookie jar with nothing on disk
    // saying so. `ops::desktop::switch_desktop` clears it once both halves
    // have landed.
    let tp = TestPaths::new().unwrap();
    let live = tp.root().join("Claude");
    seed(&live, &[("Network", "a")]);
    let j = Journal::plan(&live, Some(&tp.desktop_profile_dir("a")), None).unwrap();

    execute(&tp, j).unwrap();

    let left = Journal::from_bytes(&std::fs::read(tp.desktop_journal_file()).unwrap()).unwrap();
    assert!(
        left.is_complete(),
        "every rename landed; the journal survives for the identity half alone"
    );

    clear_journal(&tp).unwrap();
    assert!(
        !tp.desktop_journal_file().exists(),
        "a stale journal would make the next byte command 'repair' a finished swap"
    );
    // Idempotent: `switch_desktop` calls this on paths that may have had no
    // journal written at all, and a repeat must not fail.
    clear_journal(&tp).unwrap();
}

#[test]
fn a_plan_with_no_moves_still_records_the_swap() {
    // "No directory moves" does not mean "no work": a park that finds
    // nothing movable still has to clear the app's account keys, and that
    // patch needs a record for the same reason every other one does.
    let tp = TestPaths::new().unwrap();
    let live = tp.root().join("Claude");
    std::fs::create_dir_all(&live).unwrap();
    let j = Journal::plan(&live, Some(&tp.desktop_profile_dir("a")), None).unwrap();
    assert!(j.moves.is_empty(), "the plan under test must be empty");

    execute(&tp, j).unwrap();

    assert!(tp.desktop_journal_file().exists());
}

#[test]
fn recovery_reverses_when_only_parks_completed() {
    let j = Journal {
        version: 1,
        outgoing: None,
        incoming: None,
        moves: vec![
            Move {
                stage: Stage::Park,
                from: "/a".into(),
                to: "/b".into(),
                done: true,
            },
            Move {
                stage: Stage::Install,
                from: "/c".into(),
                to: "/d".into(),
                done: false,
            },
        ],
    };
    assert_eq!(recovery_for(&j), Recovery::Reverse);
}

#[test]
fn recovery_rolls_forward_once_any_install_completed() {
    // Even ONE completed install means the incoming profile is partly in
    // place; reversing would have to unpick it.
    let j = Journal {
        version: 1,
        outgoing: None,
        incoming: None,
        moves: vec![
            Move {
                stage: Stage::Park,
                from: "/a".into(),
                to: "/b".into(),
                done: true,
            },
            Move {
                stage: Stage::Install,
                from: "/c".into(),
                to: "/d".into(),
                done: true,
            },
            Move {
                stage: Stage::Install,
                from: "/e".into(),
                to: "/f".into(),
                done: false,
            },
        ],
    };
    assert_eq!(recovery_for(&j), Recovery::RollForward);
}

#[test]
fn an_interruption_during_the_park_is_reversed_and_the_session_comes_back() {
    // The scenario that matters: byte is killed mid-swap. Simulated by
    // writing a journal whose parks are half done and then running recovery,
    // which is exactly the state a kill would leave on disk.
    let tp = TestPaths::new().unwrap();
    let live = tp.root().join("Claude");
    let park = tp.desktop_profile_dir("uuid-a");
    seed(
        &live,
        &[("Network", "account-a"), ("IndexedDB", "account-a")],
    );

    let mut j = Journal::plan(&live, Some(&park), None).unwrap();
    // Perform the first move for real, and record it, then stop.
    std::fs::create_dir_all(&park).unwrap();
    std::fs::rename(&j.moves[0].from, &j.moves[0].to).unwrap();
    j.mark_done(0);
    std::fs::create_dir_all(tp.desktop_store_dir()).unwrap();
    std::fs::write(tp.desktop_journal_file(), j.to_bytes().unwrap()).unwrap();

    let outcome = recovered(&tp);

    assert_eq!(outcome, Some(Recovery::Reverse));
    assert_eq!(
        marker(&live.join("Network")).as_deref(),
        Some("account-a"),
        "the parked directory must come back to the live location"
    );
    assert!(
        !tp.desktop_journal_file().exists(),
        "recovery must clear the journal or the next command repeats it"
    );
}

#[test]
fn no_journal_means_nothing_to_recover() {
    let tp = TestPaths::new().unwrap();
    assert_eq!(recovered(&tp), None);
}

#[test]
fn a_journal_from_a_future_version_is_refused_rather_than_half_understood() {
    let tp = TestPaths::new().unwrap();
    std::fs::create_dir_all(tp.desktop_store_dir()).unwrap();
    std::fs::write(
        tp.desktop_journal_file(),
        br#"{"version":99,"outgoing":null,"incoming":null,"moves":[]}"#,
    )
    .unwrap();

    let err = try_recover(&tp).unwrap_err();
    assert!(
        matches!(err, byte::Error::DesktopSwapInterrupted { .. }),
        "expected DesktopSwapInterrupted, got {err:?}"
    );
}

#[test]
fn rollforward_across_the_rename_journal_write_window_completes_and_clears_the_journal() {
    // Reproduces the crash window directly: a process killed between a
    // rename succeeding and the following journal write persisting leaves
    // the directory already moved on disk while the journal still says
    // `done: false` for it. Simulated by performing the real rename and
    // deliberately never calling `mark_done` for the "IndexedDB" move,
    // while "Network" is a normal, fully completed install (so
    // `recovery_for` sees a completed `Install` and chooses RollForward).
    let tp = TestPaths::new().unwrap();
    let live = tp.root().join("Claude");
    let take = tp.desktop_profile_dir("uuid-b");
    seed(
        &take,
        &[("Network", "account-b"), ("IndexedDB", "account-b")],
    );

    let mut j = Journal::plan(&live, None, Some(&take)).unwrap();
    let done_index = move_index(&j, Stage::Install, "Network");
    let windowed_index = move_index(&j, Stage::Install, "IndexedDB");

    std::fs::create_dir_all(&live).unwrap();

    // "Network": a normal, fully completed install.
    std::fs::rename(&j.moves[done_index].from, &j.moves[done_index].to).unwrap();
    j.mark_done(done_index);

    // "IndexedDB": the crash window. The rename already happened on disk...
    std::fs::rename(&j.moves[windowed_index].from, &j.moves[windowed_index].to).unwrap();
    // ...but `done` is deliberately left `false`, exactly what a kill in
    // the window leaves behind.

    std::fs::create_dir_all(tp.desktop_store_dir()).unwrap();
    std::fs::write(tp.desktop_journal_file(), j.to_bytes().unwrap()).unwrap();

    let outcome = recovered(&tp);

    assert_eq!(outcome, Some(Recovery::RollForward));
    assert_eq!(marker(&live.join("Network")).as_deref(), Some("account-b"));
    assert_eq!(
        marker(&live.join("IndexedDB")).as_deref(),
        Some("account-b"),
        "the already-moved directory must still be recognized and marked done"
    );
    assert!(
        !tp.desktop_journal_file().exists(),
        "recovery must clear the journal even when a move crossed the rename/write window"
    );

    // Safe to run again: nothing left to recover.
    assert_eq!(recovered(&tp), None);
}

#[test]
fn reverse_across_the_rename_journal_write_window_restores_the_stranded_directory() {
    // Same crash window as above, but during a park: the directory has
    // already moved to the store, yet the journal still says `done: false`.
    // A reversal loop that skips any move it doesn't believe is `done`
    // would leave this directory stranded in the store forever while still
    // reporting a clean repair.
    let tp = TestPaths::new().unwrap();
    let live = tp.root().join("Claude");
    let park = tp.desktop_profile_dir("uuid-a");
    seed(&live, &[("Network", "account-a")]);

    let j = Journal::plan(&live, Some(&park), None).unwrap();
    std::fs::create_dir_all(&park).unwrap();
    // Perform the real rename, but deliberately never call `mark_done` --
    // that omission is exactly the state a kill in the window would leave.
    std::fs::rename(&j.moves[0].from, &j.moves[0].to).unwrap();

    std::fs::create_dir_all(tp.desktop_store_dir()).unwrap();
    std::fs::write(tp.desktop_journal_file(), j.to_bytes().unwrap()).unwrap();

    let outcome = recovered(&tp);

    assert_eq!(outcome, Some(Recovery::Reverse));
    assert_eq!(
        marker(&live.join("Network")).as_deref(),
        Some("account-a"),
        "the stranded directory must come back to the live location even though \
         the journal never recorded the move as done"
    );
    assert!(!tp.desktop_journal_file().exists());
}

#[test]
fn an_interruption_after_the_park_rolls_forward_and_finishes_the_remaining_install() {
    // The RollForward twin of `an_interruption_during_the_park_is_reversed_
    // and_the_session_comes_back`: the park is fully complete and one
    // install has already landed, so recovery must finish the *remaining*
    // install rather than reverse anything. This drives the actual rename
    // performed by RollForward, not just `recovery_for`'s decision.
    let tp = TestPaths::new().unwrap();
    let live = tp.root().join("Claude");
    let park = tp.desktop_profile_dir("uuid-a");
    let take = tp.desktop_profile_dir("uuid-b");
    seed(&live, &[("Network", "account-a")]);
    seed(
        &take,
        &[("Network", "account-b"), ("IndexedDB", "account-b")],
    );

    let mut j = Journal::plan(&live, Some(&park), Some(&take)).unwrap();

    // The park completes fully.
    let park_index = move_index(&j, Stage::Park, "Network");
    std::fs::create_dir_all(&park).unwrap();
    std::fs::rename(&j.moves[park_index].from, &j.moves[park_index].to).unwrap();
    j.mark_done(park_index);

    // One install completes fully...
    let done_install = move_index(&j, Stage::Install, "Network");
    std::fs::rename(&j.moves[done_install].from, &j.moves[done_install].to).unwrap();
    j.mark_done(done_install);

    // ...the other is left completely untouched: still at `take`, still
    // `done: false`. This is the move RollForward must actually execute.
    let remaining_install = move_index(&j, Stage::Install, "IndexedDB");
    assert!(!j.moves[remaining_install].done);

    std::fs::create_dir_all(tp.desktop_store_dir()).unwrap();
    std::fs::write(tp.desktop_journal_file(), j.to_bytes().unwrap()).unwrap();

    let outcome = recovered(&tp);

    assert_eq!(outcome, Some(Recovery::RollForward));
    assert_eq!(
        marker(&live.join("IndexedDB")).as_deref(),
        Some("account-b"),
        "recovery must actually perform the remaining install's rename, not just decide to"
    );
    assert_eq!(marker(&live.join("Network")).as_deref(), Some("account-b"));
    assert_eq!(marker(&park.join("Network")).as_deref(), Some("account-a"));
    assert!(!tp.desktop_journal_file().exists());
}

#[test]
fn execute_refuses_to_start_when_an_earlier_journal_is_still_on_disk() {
    let tp = TestPaths::new().unwrap();
    std::fs::create_dir_all(tp.desktop_store_dir()).unwrap();
    std::fs::write(
        tp.desktop_journal_file(),
        b"leftover from an unrepaired swap",
    )
    .unwrap();

    let live = tp.root().join("Claude");
    seed(&live, &[("Network", "account-a")]);
    let j = Journal::plan(&live, Some(&tp.desktop_profile_dir("uuid-a")), None).unwrap();

    let err = execute(&tp, j).unwrap_err();

    assert!(
        matches!(err, byte::Error::DesktopSwapInterrupted { .. }),
        "expected DesktopSwapInterrupted, got {err:?}"
    );
    assert_eq!(
        std::fs::read(tp.desktop_journal_file()).unwrap(),
        b"leftover from an unrepaired swap",
        "the pre-existing journal must be left untouched, not overwritten by the new plan"
    );
}

#[test]
fn reverse_with_installs_pending_and_no_rename_yet_leaves_both_profiles_where_they_were() {
    // The shape a real `byte switch` always produces -- parks AND installs
    // in one plan -- caught in the widest crash window of all: the journal
    // is on disk and not one rename has run yet.
    //
    // Reverse visits installs first (they are planned last), and every
    // backward install here finds BOTH ends occupied: its source is the live
    // directory, not parked yet, and its destination is the incoming
    // profile's directory, not installed yet. Renaming into an occupied
    // destination is an OS error, and an error escaping the reversal loop
    // leaves the journal on disk -- which makes every subsequent byte
    // command reproduce it, forever.
    let tp = TestPaths::new().unwrap();
    let live = tp.root().join("Claude");
    let park = tp.desktop_profile_dir("uuid-a");
    let take = tp.desktop_profile_dir("uuid-b");
    seed(
        &live,
        &[("Network", "account-a"), ("IndexedDB", "account-a")],
    );
    seed(
        &take,
        &[("Network", "account-b"), ("IndexedDB", "account-b")],
    );

    let j = Journal::plan(&live, Some(&park), Some(&take)).unwrap();
    assert!(
        j.moves.iter().any(|m| m.stage == Stage::Install),
        "this test is only meaningful if the plan really contains installs"
    );
    assert!(
        j.moves.iter().all(|m| !m.done),
        "the window under test is the one before any rename has run"
    );
    std::fs::create_dir_all(tp.desktop_store_dir()).unwrap();
    std::fs::write(tp.desktop_journal_file(), j.to_bytes().unwrap()).unwrap();

    let outcome = recovered(&tp);

    assert_eq!(outcome, Some(Recovery::Reverse));
    assert_eq!(marker(&live.join("Network")).as_deref(), Some("account-a"));
    assert_eq!(
        marker(&live.join("IndexedDB")).as_deref(),
        Some("account-a"),
        "an untouched live directory must still be live after the reversal"
    );
    assert_eq!(
        marker(&take.join("Network")).as_deref(),
        Some("account-b"),
        "the incoming profile was never installed, so it stays in its store"
    );
    assert_eq!(
        marker(&take.join("IndexedDB")).as_deref(),
        Some("account-b"),
        "the incoming profile was never installed, so it stays in its store"
    );
    assert!(
        !park.join("Network").exists(),
        "no park ran, so nothing may be left behind in the outgoing store"
    );
    assert!(
        !park.join("IndexedDB").exists(),
        "no park ran, so nothing may be left behind in the outgoing store"
    );
    assert!(
        !tp.desktop_journal_file().exists(),
        "recovery must clear the journal or the next command repeats it"
    );
}

#[test]
fn reverse_mid_park_with_installs_pending_restores_every_live_directory() {
    // The same parks-and-installs shape, interrupted one step later: a
    // single park has completed and been recorded, the rest of the park has
    // not, and no install has run. The still-unparked live directory and the
    // incoming profile's matching directory are both present, which is the
    // collision Reverse has to absorb rather than fail on.
    let tp = TestPaths::new().unwrap();
    let live = tp.root().join("Claude");
    let park = tp.desktop_profile_dir("uuid-a");
    let take = tp.desktop_profile_dir("uuid-b");
    seed(
        &live,
        &[("Network", "account-a"), ("IndexedDB", "account-a")],
    );
    seed(
        &take,
        &[("Network", "account-b"), ("IndexedDB", "account-b")],
    );

    let mut j = Journal::plan(&live, Some(&park), Some(&take)).unwrap();

    // "Network" is parked for real and recorded; the swap is killed there.
    let parked = move_index(&j, Stage::Park, "Network");
    std::fs::create_dir_all(&park).unwrap();
    std::fs::rename(&j.moves[parked].from, &j.moves[parked].to).unwrap();
    j.mark_done(parked);

    assert!(
        !j.moves[move_index(&j, Stage::Park, "IndexedDB")].done,
        "the second park must still be outstanding for this to be mid-park"
    );
    assert!(
        j.moves.iter().all(|m| m.stage != Stage::Install || !m.done),
        "no install may have run, or recovery would roll forward instead"
    );

    std::fs::create_dir_all(tp.desktop_store_dir()).unwrap();
    std::fs::write(tp.desktop_journal_file(), j.to_bytes().unwrap()).unwrap();

    let outcome = recovered(&tp);

    assert_eq!(outcome, Some(Recovery::Reverse));
    assert_eq!(
        marker(&live.join("Network")).as_deref(),
        Some("account-a"),
        "the parked directory must come back to the live location"
    );
    assert_eq!(
        marker(&live.join("IndexedDB")).as_deref(),
        Some("account-a"),
        "the directory that was never parked must still be live"
    );
    assert_eq!(
        marker(&take.join("Network")).as_deref(),
        Some("account-b"),
        "the incoming profile was never installed, so it stays in its store"
    );
    assert_eq!(
        marker(&take.join("IndexedDB")).as_deref(),
        Some("account-b"),
        "the incoming profile was never installed, so it stays in its store"
    );
    assert!(
        !park.join("Network").exists(),
        "the reversed park must not leave a copy in the outgoing store"
    );
    assert!(!tp.desktop_journal_file().exists());
}

#[test]
fn a_reversed_swap_with_installs_pending_is_not_recovered_a_second_time() {
    // The wedge test. If Reverse fails on the parks-and-installs shape, the
    // journal survives and every later byte command -- recovery runs at the
    // start of all of them -- hits the identical error, while `execute`
    // simultaneously refuses to start a new swap for as long as that journal
    // exists. A second call returning `Ok(None)` is what proves both exits
    // are open again.
    let tp = TestPaths::new().unwrap();
    let live = tp.root().join("Claude");
    let park = tp.desktop_profile_dir("uuid-a");
    let take = tp.desktop_profile_dir("uuid-b");
    seed(&live, &[("Network", "account-a")]);
    seed(&take, &[("Network", "account-b")]);

    let j = Journal::plan(&live, Some(&park), Some(&take)).unwrap();
    std::fs::create_dir_all(tp.desktop_store_dir()).unwrap();
    std::fs::write(tp.desktop_journal_file(), j.to_bytes().unwrap()).unwrap();

    assert_eq!(recovered(&tp), Some(Recovery::Reverse));
    assert_eq!(
        recovered(&tp),
        None,
        "the repair must be finished after one pass, not repeated forever"
    );
    assert_eq!(marker(&live.join("Network")).as_deref(), Some("account-a"));
}

#[test]
fn reverse_undoes_an_install_whose_rename_landed_but_was_never_recorded() {
    // The other half of the same classification, and the reason Reverse may
    // never skip a move merely because `done` is false. Here the park is
    // complete and one install's rename has already landed -- but the write
    // recording it did not, so `recovery_for` still sees no completed
    // install and reverses. That install's two ends are the mirror image of
    // the never-ran case: its source (the live directory) is occupied and
    // its destination (the incoming profile's store) is empty. It must be
    // renamed back, or the incoming account's directory stays stranded in
    // the outgoing account's live location.
    let tp = TestPaths::new().unwrap();
    let live = tp.root().join("Claude");
    let park = tp.desktop_profile_dir("uuid-a");
    let take = tp.desktop_profile_dir("uuid-b");
    seed(
        &live,
        &[("Network", "account-a"), ("IndexedDB", "account-a")],
    );
    seed(&take, &[("Network", "account-b")]);

    let mut j = Journal::plan(&live, Some(&park), Some(&take)).unwrap();

    // The park completes fully and is recorded.
    std::fs::create_dir_all(&park).unwrap();
    for name in ["Network", "IndexedDB"] {
        let index = move_index(&j, Stage::Park, name);
        std::fs::rename(&j.moves[index].from, &j.moves[index].to).unwrap();
        j.mark_done(index);
    }

    // The install's rename lands, and the process dies before `mark_done`.
    let windowed = move_index(&j, Stage::Install, "Network");
    std::fs::rename(&j.moves[windowed].from, &j.moves[windowed].to).unwrap();
    assert!(
        !j.moves[windowed].done,
        "the window under test is the one where `done` never persisted"
    );

    std::fs::write(tp.desktop_journal_file(), j.to_bytes().unwrap()).unwrap();

    let outcome = recovered(&tp);

    assert_eq!(outcome, Some(Recovery::Reverse));
    assert_eq!(
        marker(&take.join("Network")).as_deref(),
        Some("account-b"),
        "the install that landed unrecorded must be undone, not left stranded live"
    );
    assert_eq!(
        marker(&live.join("Network")).as_deref(),
        Some("account-a"),
        "the outgoing session must be back in the live location"
    );
    assert_eq!(
        marker(&live.join("IndexedDB")).as_deref(),
        Some("account-a"),
        "the outgoing session must be back in the live location"
    );
    assert!(
        !park.join("Network").exists(),
        "a fully reversed park leaves nothing behind in the outgoing store"
    );
    assert!(!tp.desktop_journal_file().exists());
}

// ---------------------------------------------------------------------------
// The journal's commit boundary. A swap is roughly sixteen renames AND a
// patch of three keys in the desktop app's `config.json`; recovery that
// finishes only the renames leaves the app authenticating as one account
// over another account's cookie jar, with no journal left to say so.
// ---------------------------------------------------------------------------

#[test]
fn rolling_forward_leaves_the_config_naming_the_incoming_account() {
    // The window: a crash after the first completed install and before the
    // journal is cleared. Recovery finishes the remaining installs, so the
    // incoming account's cookies are live -- but `config.json` still holds
    // the OUTGOING account's token cache and `lastKnownAccountUuid`, and the
    // command that ran the repair tells the user it is done.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    let live = dp.desktop_dir();
    let park = tp.desktop_profile_dir("uuid-a");
    let take = tp.desktop_profile_dir("uuid-b");
    seed(&live, &[("Network", "account-a")]);
    seed(
        &take,
        &[("Network", "account-b"), ("IndexedDB", "account-b")],
    );

    // The live config, as the interrupted swap left it: still account a's.
    std::fs::write(
        dp.config_file(),
        serde_json::json!({
            "lastKnownAccountUuid": "uuid-a",
            "oauth:tokenCacheV2": {"accessToken": "a-token"},
            "locale": "en-GB"
        })
        .to_string(),
    )
    .unwrap();
    // b's stored identity, waiting in its profile directory for the patch
    // that never ran.
    std::fs::write(
        take.join("oauth.json"),
        serde_json::json!({
            "oauth": {"oauth:tokenCacheV2": {"accessToken": "b-token"}},
            "account_uuid": "uuid-b"
        })
        .to_string(),
    )
    .unwrap();

    let mut j = Journal::plan(&live, Some(&park), Some(&take)).unwrap();
    j.outgoing = Some("uuid-a".to_string());
    j.incoming = Some("uuid-b".to_string());

    // Park completes, one install completes, then the process dies.
    let park_index = move_index(&j, Stage::Park, "Network");
    std::fs::create_dir_all(&park).unwrap();
    std::fs::rename(&j.moves[park_index].from, &j.moves[park_index].to).unwrap();
    j.mark_done(park_index);
    let done_install = move_index(&j, Stage::Install, "Network");
    std::fs::rename(&j.moves[done_install].from, &j.moves[done_install].to).unwrap();
    j.mark_done(done_install);

    std::fs::create_dir_all(tp.desktop_store_dir()).unwrap();
    std::fs::write(tp.desktop_journal_file(), j.to_bytes().unwrap()).unwrap();

    let repair = recover_if_interrupted(&tp, Some(&dp)).unwrap().unwrap();

    assert_eq!(repair.recovery, Recovery::RollForward);
    assert_eq!(
        marker(&live.join("IndexedDB")).as_deref(),
        Some("account-b")
    );
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dp.config_file()).unwrap()).unwrap();
    assert_eq!(
        cfg["lastKnownAccountUuid"],
        serde_json::json!("uuid-b"),
        "the repair is not finished while the app still reports the outgoing account"
    );
    assert_eq!(
        cfg["oauth:tokenCacheV2"],
        serde_json::json!({"accessToken": "b-token"}),
        "the app must authenticate from the account whose cookies are now live"
    );
    assert_eq!(cfg["locale"], serde_json::json!("en-GB"));
    assert!(!tp.desktop_journal_file().exists());
}

#[test]
fn rolling_forward_into_an_account_with_nothing_stored_signs_the_app_out() {
    // The `NoProfileForIncoming` shape, interrupted: the journal names no
    // incoming account because there was no stored profile to install. The
    // repair must finish what `switch_desktop` would have done -- clear the
    // keys -- rather than leave the outgoing account's identity in place.
    //
    // Reached with a hand-built journal because a real plan with no incoming
    // account has no `Install` moves, and so always reverses.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    let live = dp.desktop_dir();
    let park = tp.desktop_profile_dir("uuid-a");
    seed(&live, &[("Network", "account-a")]);
    std::fs::write(
        dp.config_file(),
        serde_json::json!({"lastKnownAccountUuid": "uuid-a", "locale": "en-GB"}).to_string(),
    )
    .unwrap();

    std::fs::create_dir_all(&park).unwrap();
    let j = Journal {
        version: 1,
        outgoing: Some("uuid-a".to_string()),
        incoming: None,
        moves: vec![Move {
            stage: Stage::Install,
            from: park.join("nothing"),
            to: live.join("nothing"),
            done: true,
        }],
    };
    std::fs::create_dir_all(tp.desktop_store_dir()).unwrap();
    std::fs::write(tp.desktop_journal_file(), j.to_bytes().unwrap()).unwrap();

    let repair = recover_if_interrupted(&tp, Some(&dp)).unwrap().unwrap();

    assert_eq!(repair.recovery, Recovery::RollForward);
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dp.config_file()).unwrap()).unwrap();
    assert!(
        cfg.get("lastKnownAccountUuid").is_none(),
        "nothing was installed, so the app must end up signed out, not still naming a"
    );
    assert_eq!(cfg["locale"], serde_json::json!("en-GB"));
    assert!(!tp.desktop_journal_file().exists());
}

#[test]
fn reversing_leaves_a_config_that_already_describes_the_restored_session_alone() {
    // A reversal's `config.json` was never patched -- the patch runs after
    // every rename, so a swap that did not get that far still names the
    // outgoing account. Restoring its parked keys over it is then a no-op,
    // and byte must not rewrite a file it does not own to achieve nothing:
    // a repair runs at the start of every command, so an unnecessary write
    // here is an unnecessary backup, rewrite and verify every time, on a
    // file that may not even be writable.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    let live = dp.desktop_dir();
    let park = tp.desktop_profile_dir("uuid-a");
    seed(
        &live,
        &[("Network", "account-a"), ("IndexedDB", "account-a")],
    );

    let before = serde_json::json!({
        "lastKnownAccountUuid": "uuid-a",
        "oauth:tokenCacheV2": {"accessToken": "a-token"},
        "locale": "en-GB"
    })
    .to_string();
    std::fs::write(dp.config_file(), &before).unwrap();
    std::fs::create_dir_all(&park).unwrap();
    std::fs::write(
        park.join("oauth.json"),
        serde_json::json!({
            "oauth": {"oauth:tokenCacheV2": {"accessToken": "a-token"}},
            "account_uuid": "uuid-a"
        })
        .to_string(),
    )
    .unwrap();

    let mut j = Journal::plan(&live, Some(&park), None).unwrap();
    j.outgoing = Some("uuid-a".to_string());
    // One entry parked, one still outstanding: the park is genuinely
    // half-done, which is what makes this a reversal at all.
    let parked = move_index(&j, Stage::Park, "Network");
    std::fs::rename(&j.moves[parked].from, &j.moves[parked].to).unwrap();
    j.mark_done(parked);
    assert!(
        !j.is_complete(),
        "the window under test is a half-done park"
    );
    std::fs::create_dir_all(tp.desktop_store_dir()).unwrap();
    std::fs::write(tp.desktop_journal_file(), j.to_bytes().unwrap()).unwrap();

    let repair = recover_if_interrupted(&tp, Some(&dp)).unwrap().unwrap();

    assert_eq!(repair.recovery, Recovery::Reverse);
    assert_eq!(repair.identity, IdentityRepair::Restored);
    assert_eq!(marker(&live.join("Network")).as_deref(), Some("account-a"));
    assert_eq!(
        std::fs::read_to_string(dp.config_file()).unwrap(),
        before,
        "a config that already describes the restored session must be byte-for-byte untouched"
    );
    assert!(
        !tp.backup_dir().join("config.json").exists()
            && std::fs::read_dir(tp.backup_dir())
                .map(|d| d.flatten().count())
                .unwrap_or(0)
                == 0,
        "and no backup churn for a write that did not happen"
    );
}

#[test]
fn a_repair_that_cannot_reach_the_config_keeps_the_journal_and_says_so() {
    // `RealDesktopPaths::discover` fails when neither `CLAUDE_DESKTOP_DIR`
    // nor `%APPDATA%` is set. Recovery must still put the directories back
    // where they belong -- that half needs no desktop paths at all -- and
    // must NOT clear the journal, because the identity half is still
    // outstanding and only a later command can finish it.
    let tp = TestPaths::new().unwrap();
    let live = tp.root().join("Claude");
    let park = tp.desktop_profile_dir("uuid-a");
    seed(
        &live,
        &[("Network", "account-a"), ("IndexedDB", "account-a")],
    );

    let mut j = Journal::plan(&live, Some(&park), None).unwrap();
    j.outgoing = Some("uuid-a".to_string());
    std::fs::create_dir_all(&park).unwrap();
    let parked = move_index(&j, Stage::Park, "Network");
    std::fs::rename(&j.moves[parked].from, &j.moves[parked].to).unwrap();
    j.mark_done(parked);
    assert!(
        !j.is_complete(),
        "the window under test is a half-done park"
    );
    std::fs::create_dir_all(tp.desktop_store_dir()).unwrap();
    std::fs::write(tp.desktop_journal_file(), j.to_bytes().unwrap()).unwrap();

    let repair = recover_if_interrupted(&tp, None::<&TestDesktopPaths>)
        .unwrap()
        .unwrap();

    assert_eq!(repair.recovery, Recovery::Reverse);
    assert_eq!(
        repair.identity,
        IdentityRepair::Deferred,
        "the caller has to be able to say what was left unfinished"
    );
    assert_eq!(
        marker(&live.join("Network")).as_deref(),
        Some("account-a"),
        "the directory half needs no desktop paths and must still be repaired"
    );
    assert!(
        tp.desktop_journal_file().exists(),
        "an unfinished identity half must leave the journal for a later command"
    );
}

#[test]
fn a_forward_move_with_neither_end_present_is_an_error() {
    // `apply_move` classifies the pair (source present, destination present)
    // before renaming, and only the exact "source gone, destination there"
    // shape may be treated as an already-applied no-op. With NEITHER end
    // present something is genuinely wrong and the OS's own error is the
    // honest answer -- papering over it would mark a move done that never
    // happened and clear the journal that records it.
    let tp = TestPaths::new().unwrap();
    let j = Journal {
        version: 1,
        outgoing: None,
        incoming: None,
        moves: vec![Move {
            stage: Stage::Park,
            from: tp.root().join("gone/Network"),
            to: tp.root().join("store/Network"),
            done: false,
        }],
    };

    let err = execute(&tp, j).unwrap_err();

    assert!(
        matches!(err, byte::Error::Io { .. }),
        "expected the missing source's io error, got: {err:?}"
    );
}

#[test]
fn a_backward_move_with_neither_end_present_is_an_error() {
    // The undo direction's half of the same classification. Reversing a move
    // whose two ends are both empty cannot be a no-op success: the directory
    // that move was carrying is not at either end, so recovery has lost it
    // and must say so rather than clear the journal that names it.
    let tp = TestPaths::new().unwrap();
    // A second, outstanding move keeps this a reversal (a journal with
    // nothing left outstanding rolls forward instead) and is itself an
    // ordinary never-ran park, both of whose ends are still occupied.
    seed(&tp.root().join("live"), &[("IndexedDB", "account-a")]);
    seed(&tp.root().join("store"), &[("IndexedDB", "stale")]);
    let j = Journal {
        version: 1,
        outgoing: None,
        incoming: None,
        moves: vec![
            Move {
                stage: Stage::Park,
                from: tp.root().join("gone/Network"),
                to: tp.root().join("store/Network"),
                done: true,
            },
            Move {
                stage: Stage::Park,
                from: tp.root().join("live/IndexedDB"),
                to: tp.root().join("store/IndexedDB"),
                done: false,
            },
        ],
    };
    std::fs::create_dir_all(tp.desktop_store_dir()).unwrap();
    std::fs::write(tp.desktop_journal_file(), j.to_bytes().unwrap()).unwrap();

    let err = recover_if_interrupted(&tp, None::<&TestDesktopPaths>).unwrap_err();

    assert!(
        matches!(err, byte::Error::Io { .. }),
        "expected the missing source's io error, got: {err:?}"
    );
}

#[test]
fn a_journal_with_nothing_outstanding_rolls_forward_even_with_no_installs() {
    // Switching into an account with nothing stored plans parks and no
    // installs at all, so the "any install completed" test can never fire
    // for it. Without a completeness test as well, such a journal reverses
    // -- and the window it can be found in is no longer only a crash: the
    // journal now also covers the `config.json` patch, which fails on
    // ordinary conditions. Reversing there would put the outgoing account's
    // session back and quietly undo a switch that had already happened.
    let j = Journal {
        version: 1,
        outgoing: Some("uuid-a".to_string()),
        incoming: None,
        moves: vec![Move {
            stage: Stage::Park,
            from: "/live/Network".into(),
            to: "/store/Network".into(),
            done: true,
        }],
    };

    assert_eq!(recovery_for(&j), Recovery::RollForward);
}

#[test]
fn a_park_only_swap_whose_patch_failed_is_finished_not_undone() {
    // The same rule end to end, in the shape that produces it: every rename
    // landed, the app's account keys did not get cleared, and the next
    // command has to finish that -- not put the outgoing account's profile
    // back where it started.
    let tp = TestPaths::new().unwrap();
    let dp = TestDesktopPaths::new().unwrap();
    let live = dp.desktop_dir();
    let park = tp.desktop_profile_dir("uuid-a");
    seed(&live, &[("Network", "account-a")]);
    std::fs::write(
        dp.config_file(),
        serde_json::json!({
            "lastKnownAccountUuid": "uuid-a",
            "oauth:tokenCacheV2": {"accessToken": "a-token"},
            "locale": "en-GB"
        })
        .to_string(),
    )
    .unwrap();

    let mut j = Journal::plan(&live, Some(&park), None).unwrap();
    j.outgoing = Some("uuid-a".to_string());
    std::fs::create_dir_all(&park).unwrap();
    for index in 0..j.moves.len() {
        std::fs::rename(&j.moves[index].from, &j.moves[index].to).unwrap();
        j.mark_done(index);
    }
    assert!(j.is_complete());
    std::fs::create_dir_all(tp.desktop_store_dir()).unwrap();
    std::fs::write(tp.desktop_journal_file(), j.to_bytes().unwrap()).unwrap();

    let repair = recover_if_interrupted(&tp, Some(&dp)).unwrap().unwrap();

    assert_eq!(repair.recovery, Recovery::RollForward);
    assert_eq!(
        marker(&park.join("Network")).as_deref(),
        Some("account-a"),
        "the completed park must stay parked, not be undone"
    );
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dp.config_file()).unwrap()).unwrap();
    assert!(
        cfg.get("lastKnownAccountUuid").is_none(),
        "the patch that never ran is what the repair had to finish"
    );
    assert!(cfg.get("oauth:tokenCacheV2").is_none());
    assert_eq!(cfg["locale"], serde_json::json!("en-GB"));
    assert!(!tp.desktop_journal_file().exists());
}
