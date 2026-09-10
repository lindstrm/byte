use std::path::Path;

use byte::desktop::journal::{Journal, Move, Stage};
use byte::desktop::swap::{Recovery, execute, recover_if_interrupted, recovery_for};
use byte::paths::{HostPaths, TestPaths};

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
fn a_successful_swap_leaves_no_journal_behind() {
    let tp = TestPaths::new().unwrap();
    let live = tp.root().join("Claude");
    seed(&live, &[("Network", "a")]);
    let j = Journal::plan(&live, Some(&tp.desktop_profile_dir("a")), None).unwrap();

    execute(&tp, j).unwrap();

    assert!(
        !tp.desktop_journal_file().exists(),
        "a stale journal would make the next byte command 'repair' a finished swap"
    );
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

    let outcome = recover_if_interrupted(&tp).unwrap();

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
    assert_eq!(recover_if_interrupted(&tp).unwrap(), None);
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

    let err = recover_if_interrupted(&tp).unwrap_err();
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

    let outcome = recover_if_interrupted(&tp).unwrap();

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
    assert_eq!(recover_if_interrupted(&tp).unwrap(), None);
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

    let outcome = recover_if_interrupted(&tp).unwrap();

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

    let outcome = recover_if_interrupted(&tp).unwrap();

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

    let outcome = recover_if_interrupted(&tp).unwrap();

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

    let outcome = recover_if_interrupted(&tp).unwrap();

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

    assert_eq!(
        recover_if_interrupted(&tp).unwrap(),
        Some(Recovery::Reverse)
    );
    assert_eq!(
        recover_if_interrupted(&tp).unwrap(),
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

    let outcome = recover_if_interrupted(&tp).unwrap();

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
