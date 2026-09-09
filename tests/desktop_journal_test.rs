use std::path::{Path, PathBuf};

use byte::desktop::journal::{Journal, Stage};

fn seed(dir: &Path, names: &[&str]) {
    std::fs::create_dir_all(dir).unwrap();
    for n in names {
        std::fs::create_dir_all(dir.join(n)).unwrap();
    }
}

#[test]
fn a_plan_parks_the_outgoing_profile_then_installs_the_incoming_one() {
    let tmp = tempfile::tempdir().unwrap();
    let live = tmp.path().join("Claude");
    let park = tmp.path().join("store/out");
    let take = tmp.path().join("store/in");
    seed(&live, &["Network", "Cache"]);
    seed(&take, &["Network"]);

    let j = Journal::plan(&live, Some(&park), Some(&take)).unwrap();

    // Cache is denylisted, so it is not in the plan at all.
    assert_eq!(j.moves.len(), 2, "{:?}", j.moves);
    assert_eq!(j.moves[0].stage, Stage::Park);
    assert_eq!(j.moves[0].from, live.join("Network"));
    assert_eq!(j.moves[0].to, park.join("Network"));
    assert_eq!(j.moves[1].stage, Stage::Install);
    assert_eq!(j.moves[1].from, take.join("Network"));
    assert_eq!(j.moves[1].to, live.join("Network"));
}

#[test]
fn every_park_precedes_every_install() {
    // Order is load-bearing: installing before parking would overwrite the
    // outgoing account's live session with the incoming one's.
    let tmp = tempfile::tempdir().unwrap();
    let live = tmp.path().join("Claude");
    let park = tmp.path().join("out");
    let take = tmp.path().join("in");
    seed(&live, &["Network", "Local Storage", "IndexedDB"]);
    seed(&take, &["Network", "Local Storage"]);

    let j = Journal::plan(&live, Some(&park), Some(&take)).unwrap();
    let first_install = j
        .moves
        .iter()
        .position(|m| m.stage == Stage::Install)
        .unwrap();

    // Assert at least one park exists before any install (first_install > 0).
    // If the park loop were swapped before the install loop, first_install would be 0,
    // and the original prefix-slice form (j.moves[..0].all(...)) would vacuously pass
    // because .all() on an empty iterator returns true. This assertion catches that bug.
    assert!(
        first_install > 0,
        "at least one park must precede the first install: {:?}",
        j.moves
    );

    // Assert every move from the first install onward is an install.
    // This catches any interleaving of parks and installs after the first install.
    assert!(
        j.moves[first_install..]
            .iter()
            .all(|m| m.stage == Stage::Install),
        "all moves after the first install must be installs, not interleaved: {:?}",
        j.moves
    );
}

#[test]
fn a_switch_to_an_uncaptured_account_parks_but_installs_nothing() {
    // Design decision 5: the app is left signed out, the user signs in, and
    // byte captures. The outgoing session is still parked, never discarded.
    let tmp = tempfile::tempdir().unwrap();
    let live = tmp.path().join("Claude");
    let park = tmp.path().join("out");
    seed(&live, &["Network"]);

    let j = Journal::plan(&live, Some(&park), None).unwrap();

    assert!(
        j.moves.iter().all(|m| m.stage == Stage::Park),
        "{:?}",
        j.moves
    );
    assert_eq!(j.moves.len(), 1);
}

#[test]
fn a_journal_round_trips_through_its_codec() {
    let j = Journal {
        version: 1,
        outgoing: Some("uuid-a".into()),
        incoming: Some("uuid-b".into()),
        moves: vec![byte::desktop::journal::Move {
            stage: Stage::Park,
            from: PathBuf::from("/a"),
            to: PathBuf::from("/b"),
            done: true,
        }],
    };
    let back = Journal::from_bytes(&j.to_bytes().unwrap()).unwrap();
    assert_eq!(back.outgoing.as_deref(), Some("uuid-a"));
    assert_eq!(back.incoming.as_deref(), Some("uuid-b"));
    assert_eq!(back.moves.len(), 1);
    assert!(
        back.moves[0].done,
        "the done flag must survive the round trip"
    );
    assert_eq!(back.moves[0].from, PathBuf::from("/a"));
}

#[test]
fn a_journal_is_complete_only_when_every_move_is_done() {
    let mut j = Journal {
        version: 1,
        outgoing: None,
        incoming: None,
        moves: vec![
            byte::desktop::journal::Move {
                stage: Stage::Park,
                from: PathBuf::from("/a"),
                to: PathBuf::from("/b"),
                done: false,
            },
            byte::desktop::journal::Move {
                stage: Stage::Install,
                from: PathBuf::from("/c"),
                to: PathBuf::from("/d"),
                done: false,
            },
        ],
    };
    assert!(!j.is_complete());
    j.mark_done(0);
    assert!(!j.is_complete(), "one of two done is not complete");
    j.mark_done(1);
    assert!(j.is_complete());
}
