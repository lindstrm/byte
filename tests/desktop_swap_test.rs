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
