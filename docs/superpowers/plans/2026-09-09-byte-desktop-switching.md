# Claude Desktop Switching Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend `byte switch` to move the Windows Claude desktop app's session alongside Claude Code's, so one switch moves both.

**Architecture:** A new `src/desktop/` module layered under `ops/`. The app's data directory splits three ways — directories that move, `config.json` which is patched in place through the existing `JsonDocument`, and entries that stay shared. Because a swap is ~16 directory renames rather than one atomic write, a journal records the intended moves before any run, so an interrupted swap is repaired by whatever byte command runs next.

**Tech Stack:** Rust 2024, `serde_json` (with `preserve_order` + `float_roundtrip`, already configured), `sysinfo` for process detection, `tempfile` for tests. No new dependencies.

## Global Constraints

- **Windows only.** All new platform behaviour is gated `#[cfg(windows)]`; pure logic compiles and is tested on every platform, matching how `src/tray/launch.rs` splits its builders from its spawn.
- **Never write to a real home directory, a real `%APPDATA%\Claude`, a real credential store, or the registry from a test.** Every test uses `TestPaths` / `TestDesktopPaths` over `tempfile`.
- **Never run any `byte` subcommand against the developer's real config** while implementing. `cargo test` and `cargo clippy` are safe; `cargo run` is not.
- All user-facing text goes through `src/output.rs` helpers. No `println!`/`eprintln!` outside that module.
- Every write to a Claude file goes through `atomic.rs`: backup, atomic rename, read-back verification.
- `JsonDocument` never deserializes into a typed struct — every byte it does not explicitly model must survive a capture/apply cycle untouched.
- Tests live in `tests/`, never inline. Assert identity, not cardinality. Pin the error variant, not `.is_err()`.
- Non-test source files stay under 1000 lines.
- New `Error` variants that can reach a user get a row in `docs/troubleshooting.md` **in the same task**.
- Conventional Commits. Nothing here is breaking; `feat:` and `test:` throughout.

**Spec:** [`docs/superpowers/specs/2026-09-09-claude-desktop-switching-design.md`](../specs/2026-09-09-claude-desktop-switching-design.md). Where this plan and the spec disagree, the spec wins — say so rather than silently diverging.

---

## File structure

| File | Responsibility |
|---|---|
| `src/desktop/mod.rs` | Module declarations only |
| `src/desktop/paths.rs` | `DesktopPaths` trait, `RealDesktopPaths`, `TestDesktopPaths`, `CLAUDE_DESKTOP_DIR` |
| `src/desktop/profile.rs` | Entry classification and the denylist — pure |
| `src/desktop/journal.rs` | Journal model, codec, plan generation, recovery decision — pure |
| `src/desktop/config.rs` | The `config.json` OAuth patch, over `JsonDocument` |
| `src/desktop/swap.rs` | Executing a journal against a real filesystem |
| `src/ops/desktop.rs` | The operation `cli` calls: capture, park, install, gating |
| `src/paths.rs` | +2 default methods for the profile store and journal locations |
| `src/store/metadata.rs` | +`desktop_profile` on `AccountMeta` |
| `src/claude/detect.rs` | +desktop-app-running probe |
| `src/cli/run.rs` | Wire the desktop half into `cmd_switch` |

---

## Task 1: DesktopPaths and the CLAUDE_DESKTOP_DIR override

**Files:**
- Create: `src/desktop/mod.rs`, `src/desktop/paths.rs`
- Create: `tests/desktop_paths_test.rs`
- Modify: `src/lib.rs`, `src/paths.rs`

**Interfaces:**
- Produces: `trait DesktopPaths { fn desktop_dir(&self) -> PathBuf; fn config_file(&self) -> PathBuf; }`, `RealDesktopPaths::discover() -> Result<Self>`, `RealDesktopPaths::resolve(appdata: &Path, override_dir: Option<&OsStr>) -> Self`, `TestDesktopPaths::new() -> Result<Self>` with `root() -> &Path`.
- Produces: `HostPaths::desktop_store_dir()` and `HostPaths::desktop_journal_file()` default methods.
- Consumes: `HostPaths` (`src/paths.rs`), `Error::{Io, ClaudeFileMissing}`.

- [ ] **Step 1: Write the failing test**

Create `tests/desktop_paths_test.rs`:

```rust
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use byte::desktop::paths::{DesktopPaths, RealDesktopPaths, TestDesktopPaths};
use byte::paths::{HostPaths, TestPaths};

#[test]
fn the_default_location_is_under_appdata() {
    // Mirrors RealPaths::resolve: pure path arithmetic, so the mapping is
    // testable without touching process-global environment variables.
    //
    // Forward slashes ONLY, and the expectation built with join rather than
    // a second literal. Path PartialEq compares components, and a backslash
    // is an ordinary character on Linux and macOS -- so a backslash literal
    // would be one component there while the joined value is two, failing
    // deterministically on two of CI three platforms. tests/paths_test.rs
    // solves the same hazard the same way.
    let appdata = Path::new("C:/Users/x/AppData/Roaming");
    let p = RealDesktopPaths::resolve(appdata, None);
    assert_eq!(p.desktop_dir(), appdata.join("Claude"));
}

#[test]
fn the_override_replaces_the_whole_directory() {
    // An independent literal here, deliberately: this must fail if resolve
    // ignored the override or appended it to appdata.
    let over = OsString::from("D:/elsewhere/Claude");
    let p = RealDesktopPaths::resolve(Path::new("C:/Users/x/AppData/Roaming"), Some(&over));
    assert_eq!(p.desktop_dir(), Path::new("D:/elsewhere/Claude"));
}

#[test]
fn the_config_file_sits_directly_in_the_desktop_directory() {
    // config.json is patched in place, never moved, so its location must be
    // derived from desktop_dir rather than stored separately.
    let p = RealDesktopPaths::resolve(Path::new("/home/x"), None);
    assert_eq!(p.config_file(), p.desktop_dir().join("config.json"));
}

#[test]
fn test_paths_are_rooted_in_a_temp_directory_that_exists() {
    let p = TestDesktopPaths::new().unwrap();
    assert!(p.desktop_dir().is_dir(), "{:?}", p.desktop_dir());
    assert!(p.desktop_dir().starts_with(p.root()));
}

#[test]
fn the_profile_store_and_journal_live_under_bytes_config_dir() {
    // Parked profiles are byte's own state, not the app's, so they belong
    // beside accounts.json rather than inside %APPDATA%\Claude.
    let tp = TestPaths::new().unwrap();
    assert_eq!(tp.desktop_store_dir(), tp.byte_config_dir().join("desktop"));
    assert_eq!(
        tp.desktop_journal_file(),
        tp.desktop_store_dir().join("journal.json")
    );
}

#[test]
fn each_account_gets_its_own_profile_directory_keyed_by_uuid() {
    let tp = TestPaths::new().unwrap();
    let a = tp.desktop_profile_dir("uuid-1");
    let b = tp.desktop_profile_dir("uuid-2");
    assert_ne!(a, b, "two accounts must not share a profile directory");
    assert!(a.starts_with(tp.desktop_store_dir()));
    assert!(a.ends_with("uuid-1"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test desktop_paths_test`
Expected: FAIL — `unresolved import byte::desktop`.

- [ ] **Step 3: Add the store locations to HostPaths**

In `src/paths.rs`, add to the `HostPaths` trait beside `backup_dir` and `accounts_file`:

```rust
    /// Where parked desktop profiles are stored, one directory per account.
    ///
    /// byte's own state, not the app's, so it sits beside `accounts.json`
    /// rather than inside `%APPDATA%\Claude`. Note that unlike everything
    /// else byte stores, its contents are live session credentials as plain
    /// files -- see §6 of the desktop-switching design for why the keychain
    /// cannot hold them.
    fn desktop_store_dir(&self) -> PathBuf {
        self.byte_config_dir().join("desktop")
    }

    /// The in-progress swap journal. Present only mid-swap.
    fn desktop_journal_file(&self) -> PathBuf {
        self.desktop_store_dir().join("journal.json")
    }

    /// One account's parked profile.
    fn desktop_profile_dir(&self, account_uuid: &str) -> PathBuf {
        self.desktop_store_dir().join(account_uuid)
    }
```

Add the forwarding impls to the `impl<T: HostPaths + ?Sized> HostPaths for &T` block only if the compiler asks — default methods are inherited, so it should not.

- [ ] **Step 4: Write `src/desktop/paths.rs`**

```rust
//! Where the Claude desktop app keeps its data.
//!
//! Mirrors `crate::paths::HostPaths`: production uses [`RealDesktopPaths`],
//! tests use [`TestDesktopPaths`], rooted in a temporary directory. Kept
//! separate from `HostPaths` because it describes *another application's*
//! layout, which byte does not own and which can change out from under it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Where the Claude desktop app stores its session.
pub trait DesktopPaths: Send + Sync {
    /// The app's data directory (`%APPDATA%\Claude`).
    fn desktop_dir(&self) -> PathBuf;

    /// `config.json` -- patched in place, never moved, because it mixes the
    /// account's OAuth cache with the user's theme and window layout.
    fn config_file(&self) -> PathBuf {
        self.desktop_dir().join("config.json")
    }
}

impl<T: DesktopPaths + ?Sized> DesktopPaths for &T {
    fn desktop_dir(&self) -> PathBuf {
        (**self).desktop_dir()
    }
}

/// Production paths.
///
/// `CLAUDE_DESKTOP_DIR` overrides discovery, joining `CLAUDE_CONFIG_DIR` and
/// `BYTE_CONFIG_DIR`. It exists so the swap can be exercised end to end
/// against a synthetic profile tree without touching the real app -- which
/// matters more here than elsewhere, because testing against the real app
/// means being signed out mid-session.
#[derive(Debug, Clone)]
pub struct RealDesktopPaths {
    desktop_dir: PathBuf,
}

impl RealDesktopPaths {
    pub fn discover() -> Result<Self> {
        let override_dir = std::env::var_os("CLAUDE_DESKTOP_DIR");

        // Only consult APPDATA when the override is absent, matching
        // RealPaths::discover's treatment of $HOME.
        let appdata = if override_dir.is_some() {
            PathBuf::new()
        } else {
            std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .ok_or_else(|| Error::ClaudeFileMissing(PathBuf::from("%APPDATA%")))?
        };

        Ok(Self::resolve(&appdata, override_dir.as_deref()))
    }

    /// Pure path arithmetic, split out from [`discover`] so the mapping is
    /// testable against fixed inputs rather than process-global environment
    /// variables shared by every test in the binary.
    pub fn resolve(appdata: &Path, override_dir: Option<&OsStr>) -> Self {
        let desktop_dir = match override_dir {
            Some(dir) => PathBuf::from(dir),
            None => appdata.join("Claude"),
        };
        Self { desktop_dir }
    }
}

impl DesktopPaths for RealDesktopPaths {
    fn desktop_dir(&self) -> PathBuf {
        self.desktop_dir.clone()
    }
}

/// A synthetic desktop directory in a temp dir, deleted when it drops.
#[derive(Debug)]
pub struct TestDesktopPaths {
    dir: tempfile::TempDir,
}

impl TestDesktopPaths {
    pub fn new() -> Result<Self> {
        let dir = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("<tempdir>"),
            source,
        })?;
        let this = Self { dir };
        let d = this.desktop_dir();
        std::fs::create_dir_all(&d).map_err(|source| Error::Io { path: d, source })?;
        Ok(this)
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }
}

impl DesktopPaths for TestDesktopPaths {
    fn desktop_dir(&self) -> PathBuf {
        self.root().join("Claude")
    }
}
```

- [ ] **Step 5: Declare the module**

Create `src/desktop/mod.rs`:

```rust
//! Switching the Claude desktop app's session.
//!
//! See `docs/superpowers/specs/2026-09-09-claude-desktop-switching-design.md`.

pub mod paths;
```

In `src/lib.rs`, add `pub mod desktop;` in alphabetical position (after `claude`, before `error`).

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --test desktop_paths_test`
Expected: PASS, 6 tests.

- [ ] **Step 7: Verify nothing else broke**

Run: `cargo test --all && cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --all -- --check`
Expected: all pass, zero warnings.

- [ ] **Step 8: Commit**

```bash
git add src/desktop src/lib.rs src/paths.rs tests/desktop_paths_test.rs
git commit -m "feat(desktop): add the DesktopPaths seam and profile store locations"
```

---

## Task 2: Entry classification and the denylist

**Files:**
- Create: `src/desktop/profile.rs`, `tests/desktop_profile_test.rs`
- Modify: `src/desktop/mod.rs`

**Interfaces:**
- Consumes: nothing from Task 1 — deliberately pure, operating on entry names.
- Produces: `enum Disposition { Move, Patch, Leave }`, `fn classify(entry_name: &str) -> Disposition`, `fn movable_entries(dir: &Path) -> Result<Vec<String>>`.

**Background the implementer needs:** these names come from measuring a real installation (design §4). `claude-code` is 416 MB of *program binaries*, not account state. `Cache` and `Code Cache` are 690 MB of throwaway. `Partitions` is left alone because its account-scoped entry is already named `cowork-artifact-<accountUuid>-<workspace>` and self-segregates. `Local State` must stay shared: it holds the DPAPI key that decrypts every parked profile's cookies, so moving it breaks all of them at once.

- [ ] **Step 1: Write the failing test**

Create `tests/desktop_profile_test.rs`:

```rust
use std::path::Path;

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
    assert_eq!(movable_entries(&root.join("nope")).unwrap(), Vec::<String>::new());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test desktop_profile_test`
Expected: FAIL — `unresolved import byte::desktop::profile`.

- [ ] **Step 3: Write `src/desktop/profile.rs`**

```rust
//! What in the desktop app's directory belongs to an account.
//!
//! Pure: operates on entry names, so it compiles and is tested on every
//! platform even though the feature is Windows-only.
//!
//! This is a DENYLIST, and that direction is deliberate (design §3). An
//! allowlist would silently strand any session store byte has not heard of
//! -- including one a future app version adds -- producing a half-signed-in
//! profile that looks like a byte bug. Carrying an unknown directory costs
//! disk; leaving one behind costs a broken switch.

use std::path::Path;

use crate::error::{Error, Result};

/// What byte does with one entry in the desktop directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Account state: parked and restored with the account.
    Move,
    /// Mixed: byte owns some keys inside it and patches them in place.
    Patch,
    /// Shared, disposable, or already account-keyed.
    Leave,
}

/// Entries that are junk, shared, or self-segregating. Lowercased.
const LEAVE: &[&str] = &[
    // Throwaway caches -- 690 MB of the measured 1.28 GB.
    "cache",
    "code cache",
    "gpucache",
    "dawngraphitecache",
    "dawnwebgpucache",
    "blob_storage",
    "shared_proto_db",
    "videodecodestats",
    // Diagnostics.
    "logs",
    "crashpad",
    "sentry",
    "lockfile",
    // 416 MB of Claude Code program binaries -- NOT account state.
    "claude-code",
    // Already keyed by account uuid; see the module doc of `swap`.
    "partitions",
    // Holds the DPAPI key that decrypts cookies. Shared, or every parked
    // profile becomes undecryptable at once.
    "local state",
    // The user's own configuration and app preferences.
    "claude_desktop_config.json",
    "preferences",
    "window-state.json",
    "bridge-state.json",
    "ant-device-registry.json",
    "ant-did",
    // 68 bytes, one key `tokens-today`: an LLM usage counter.
    "buddy-tokens.json",
];

/// Decide what happens to one entry.
pub fn classify(entry_name: &str) -> Disposition {
    let name = entry_name.to_ascii_lowercase();

    if name == "config.json" {
        return Disposition::Patch;
    }
    if LEAVE.contains(&name.as_str()) {
        return Disposition::Leave;
    }
    Disposition::Move
}

/// Names of the entries in `dir` that move, in directory order.
///
/// A missing directory yields an empty list rather than an error: capturing
/// from a fresh install, or installing into a location that does not exist
/// yet, are both ordinary.
pub fn movable_entries(dir: &Path) -> Result<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(Error::Io {
                path: dir.to_path_buf(),
                source,
            });
        }
    };

    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| Error::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if classify(&name) == Disposition::Move {
            out.push(name);
        }
    }
    Ok(out)
}
```

- [ ] **Step 4: Declare it**

In `src/desktop/mod.rs`, add `pub mod profile;` after `pub mod paths;`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test desktop_profile_test`
Expected: PASS, 10 tests.

- [ ] **Step 6: Prove the denylist direction is load-bearing**

Temporarily change `classify`'s final line to `Disposition::Leave` and re-run. `an_unrecognised_entry_moves` must fail. Restore it. Record in the commit that you did this — a test that has never been seen to fail is not evidence.

- [ ] **Step 7: Full check and commit**

```bash
cargo test --all && cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --all -- --check
git add src/desktop/profile.rs src/desktop/mod.rs tests/desktop_profile_test.rs
git commit -m "feat(desktop): classify the app's directory entries by disposition"
```

---

## Task 3: The journal — model, codec, and plan generation

**Files:**
- Create: `src/desktop/journal.rs`, `tests/desktop_journal_test.rs`
- Modify: `src/desktop/mod.rs`

**Interfaces:**
- Consumes: `movable_entries` (Task 2).
- Produces: `enum Stage { Park, Install }`, `struct Move { stage, from, to, done }`, `struct Journal { version, outgoing, incoming, moves }`, `Journal::plan(...) -> Result<Journal>`, `Journal::to_bytes`, `Journal::from_bytes`, `Journal::mark_done(index)`, `Journal::is_complete()`.

**Why this exists:** a swap is ~16 directory renames, not one atomic write, so `atomic.rs`'s guarantees do not apply. The failure being guarded is severe and silent: the app appears signed out while the user's real session sits filed under another account's name, with nothing on screen to explain it.

- [ ] **Step 1: Write the failing test**

Create `tests/desktop_journal_test.rs`:

```rust
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
    let stages: Vec<Stage> = j.moves.iter().map(|m| m.stage).collect();
    let first_install = stages.iter().position(|s| *s == Stage::Install).unwrap();

    // Both assertions are needed, and the first one is the point. A prefix
    // form -- "everything before the first install is a park" -- is
    // VACUOUSLY TRUE when the loops are swapped: first_install would be 0,
    // moves[..0] is empty, and .all() on an empty iterator returns true. It
    // would pass against the exact bug this test is named for. Do not
    // simplify it back.
    assert!(
        first_install > 0,
        "at least one park must precede the first install: {stages:?}"
    );
    assert!(
        stages[first_install..].iter().all(|s| *s == Stage::Install),
        "no park may follow an install: {stages:?}"
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

    assert!(j.moves.iter().all(|m| m.stage == Stage::Park), "{:?}", j.moves);
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
    assert!(back.moves[0].done, "the done flag must survive the round trip");
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test desktop_journal_test`
Expected: FAIL — `unresolved import byte::desktop::journal`.

- [ ] **Step 3: Write `src/desktop/journal.rs`**

```rust
//! The record of a swap in progress.
//!
//! A swap is roughly sixteen directory renames, not one atomic write, so it
//! cannot borrow `atomic.rs`'s guarantees. This supplies the equivalent: the
//! whole intended sequence is written down BEFORE the first rename runs,
//! each entry is marked as it completes, and the file is deleted on success.
//!
//! Renames within a volume are effectively instantaneous, so the window is
//! small -- but the failure it guards is severe and silent. Without a
//! journal, an interrupted swap leaves the app apparently signed out while
//! the user's real session sits under another account's directory, with
//! nothing on screen to explain it and no way for byte to tell that from a
//! genuine signed-out state.
//!
//! Pure: plans and codecs only. Executing a journal is `swap.rs`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::desktop::profile::movable_entries;
use crate::error::{Error, Result};

/// Which half of the swap a move belongs to.
///
/// Load-bearing for recovery, not just description: see
/// `swap::recovery_for`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stage {
    /// Live -> the outgoing account's store.
    Park,
    /// The incoming account's store -> live.
    Install,
}

/// One directory rename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Move {
    pub stage: Stage,
    pub from: PathBuf,
    pub to: PathBuf,
    pub done: bool,
}

/// A swap, written down before it happens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    pub version: u32,
    /// Account whose profile is being parked, if any.
    pub outgoing: Option<String>,
    /// Account whose profile is being installed, if any.
    pub incoming: Option<String>,
    pub moves: Vec<Move>,
}

/// Bumped only if the on-disk shape changes incompatibly. A journal from a
/// future version must not be half-understood and acted on.
pub const JOURNAL_VERSION: u32 = 1;

impl Journal {
    /// Work out every rename a swap needs, parks first.
    ///
    /// Order matters: an install that ran before its corresponding park
    /// would overwrite the outgoing account's live session with the
    /// incoming one's, destroying it. `park_to`/`install_from` are `None`
    /// when there is no outgoing account to file or no captured profile to
    /// restore.
    pub fn plan(
        live_dir: &Path,
        park_to: Option<&Path>,
        install_from: Option<&Path>,
    ) -> Result<Self> {
        let mut moves = Vec::new();

        if let Some(park_to) = park_to {
            for name in movable_entries(live_dir)? {
                moves.push(Move {
                    stage: Stage::Park,
                    from: live_dir.join(&name),
                    to: park_to.join(&name),
                    done: false,
                });
            }
        }

        if let Some(install_from) = install_from {
            for name in movable_entries(install_from)? {
                moves.push(Move {
                    stage: Stage::Install,
                    from: install_from.join(&name),
                    to: live_dir.join(&name),
                    done: false,
                });
            }
        }

        Ok(Self {
            version: JOURNAL_VERSION,
            outgoing: None,
            incoming: None,
            moves,
        })
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec_pretty(self).map_err(|source| Error::Parse {
            path: PathBuf::from("<journal>"),
            source,
        })
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).map_err(|source| Error::Parse {
            path: PathBuf::from("<journal>"),
            source,
        })
    }

    pub fn mark_done(&mut self, index: usize) {
        if let Some(m) = self.moves.get_mut(index) {
            m.done = true;
        }
    }

    pub fn is_complete(&self) -> bool {
        self.moves.iter().all(|m| m.done)
    }
}
```

- [ ] **Step 4: Declare it and check serde**

In `src/desktop/mod.rs`, add `pub mod journal;`.

`serde` with `derive` is already a dependency (used by `store::metadata`). Confirm with `grep -n '^serde' Cargo.toml`; if the `derive` feature is absent, add it — do not add a new crate.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test desktop_journal_test`
Expected: PASS, 5 tests.

- [ ] **Step 6: Full check and commit**

```bash
cargo test --all && cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --all -- --check
git add src/desktop/journal.rs src/desktop/mod.rs tests/desktop_journal_test.rs
git commit -m "feat(desktop): plan a swap as a journal of renames"
```

---

## Task 4: Executing and recovering a swap

**Files:**
- Create: `src/desktop/swap.rs`, `tests/desktop_swap_test.rs`
- Modify: `src/desktop/mod.rs`, `src/error.rs`, `docs/troubleshooting.md`

**Interfaces:**
- Consumes: `Journal`, `Move`, `Stage` (Task 3); `HostPaths::desktop_journal_file` (Task 1).
- Produces: `enum Recovery { RollForward, Reverse }`, `fn recovery_for(j: &Journal) -> Recovery`, `fn execute(paths: &impl HostPaths, journal: Journal) -> Result<()>`, `fn recover_if_interrupted(paths: &impl HostPaths) -> Result<Option<Recovery>>`.
- Produces: `Error::DesktopSwapInterrupted { journal: PathBuf, detail: String }`.

**The recovery rule (design §8), stated exactly:** decided by *stage*, not by a count. If any **Install** move completed, roll forward and finish the install — the incoming profile is already partly in place and reversing would have to unpick it. If none did, the failure happened during the park, so reverse the completed parks and leave the outgoing account live.

- [ ] **Step 1: Write the failing test**

Create `tests/desktop_swap_test.rs`:

```rust
use std::path::{Path, PathBuf};

use byte::desktop::journal::{Journal, Move, Stage};
use byte::desktop::swap::{Recovery, execute, recovery_for, recover_if_interrupted};
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
    assert!(!take.join("Network").exists(), "the installed profile is moved, not copied");
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
            Move { stage: Stage::Park, from: "/a".into(), to: "/b".into(), done: true },
            Move { stage: Stage::Install, from: "/c".into(), to: "/d".into(), done: false },
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
            Move { stage: Stage::Park, from: "/a".into(), to: "/b".into(), done: true },
            Move { stage: Stage::Install, from: "/c".into(), to: "/d".into(), done: true },
            Move { stage: Stage::Install, from: "/e".into(), to: "/f".into(), done: false },
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
    seed(&live, &[("Network", "account-a"), ("IndexedDB", "account-a")]);

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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test desktop_swap_test`
Expected: FAIL — `unresolved import byte::desktop::swap`.

- [ ] **Step 3: Add the error variant**

In `src/error.rs`, after `Busy`:

```rust
    #[error(
        "a desktop profile swap was interrupted and could not be repaired automatically.\n\
         Its journal is at {journal}\n\
         {detail}"
    )]
    DesktopSwapInterrupted { journal: PathBuf, detail: String },
```

- [ ] **Step 4: Write `src/desktop/swap.rs`**

```rust
//! Executing a journal against the filesystem.
//!
//! `Partitions` is not moved by any of this, deliberately: its
//! account-scoped entry is already named `cowork-artifact-<accountUuid>-...`
//! and so self-segregates -- distinct accounts get distinct directories, and
//! switching back finds the previous one intact.

use std::path::{Path, PathBuf};

use crate::desktop::journal::{Journal, Stage};
use crate::error::{Error, Result};
use crate::paths::HostPaths;

/// What to do with an interrupted swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Finish the install: the incoming profile is already partly in place.
    RollForward,
    /// Undo the parks: nothing of the incoming profile has landed yet.
    Reverse,
}

/// Decide by stage, never by a count.
///
/// One completed `Install` is enough to commit to rolling forward: the
/// incoming profile is already partly live, and reversing would have to
/// unpick it while the outgoing profile is only half parked.
pub fn recovery_for(journal: &Journal) -> Recovery {
    let any_install_done = journal
        .moves
        .iter()
        .any(|m| m.stage == Stage::Install && m.done);

    if any_install_done {
        Recovery::RollForward
    } else {
        Recovery::Reverse
    }
}

fn rename(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::rename(from, to).map_err(|source| Error::Io {
        path: from.to_path_buf(),
        source,
    })
}

fn write_journal(paths: &impl HostPaths, journal: &Journal) -> Result<()> {
    let file = paths.desktop_journal_file();
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    // Written through `atomic::write` so a torn journal can never be the
    // thing that makes a swap unrecoverable.
    crate::atomic::write(&file, &journal.to_bytes()?)
}

fn clear_journal(paths: &impl HostPaths) -> Result<()> {
    let file = paths.desktop_journal_file();
    match std::fs::remove_file(&file) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Io { path: file, source }),
    }
}

/// Run a planned swap, recording progress as it goes.
pub fn execute(paths: &impl HostPaths, mut journal: Journal) -> Result<()> {
    if journal.moves.is_empty() {
        return Ok(());
    }

    // The whole plan hits disk BEFORE the first rename. This ordering is the
    // entire point: a journal written afterwards would describe a swap that
    // had already partly happened.
    write_journal(paths, &journal)?;

    for index in 0..journal.moves.len() {
        let (from, to) = {
            let m = &journal.moves[index];
            (m.from.clone(), m.to.clone())
        };
        rename(&from, &to)?;
        journal.mark_done(index);
        write_journal(paths, &journal)?;
    }

    clear_journal(paths)
}

/// Repair a swap left behind by a crash, kill, or power loss.
///
/// Called at the start of EVERY byte command, not just `switch`: a half-swap
/// has to be repaired by whatever runs next, not only by a retry of the
/// command that failed.
pub fn recover_if_interrupted(paths: &impl HostPaths) -> Result<Option<Recovery>> {
    let file = paths.desktop_journal_file();
    let bytes = match std::fs::read(&file) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(Error::Io { path: file, source }),
    };

    let journal = Journal::from_bytes(&bytes).map_err(|e| Error::DesktopSwapInterrupted {
        journal: file.clone(),
        detail: format!("Its journal could not be read ({e}), so byte will not guess. Inspect it by hand."),
    })?;

    if journal.version != crate::desktop::journal::JOURNAL_VERSION {
        return Err(Error::DesktopSwapInterrupted {
            journal: file,
            detail: format!(
                "It was written by byte version {} of the journal format, which this build does not understand. \
                 Upgrade byte rather than letting an older build act on it.",
                journal.version
            ),
        });
    }

    let recovery = recovery_for(&journal);
    match recovery {
        Recovery::RollForward => {
            let mut journal = journal;
            for index in 0..journal.moves.len() {
                if journal.moves[index].done {
                    continue;
                }
                let (from, to) = {
                    let m = &journal.moves[index];
                    (m.from.clone(), m.to.clone())
                };
                rename(&from, &to)?;
                journal.mark_done(index);
                write_journal(paths, &journal)?;
            }
        }
        Recovery::Reverse => {
            // Reverse order, so a directory is never restored on top of one
            // still waiting to be moved out of the way.
            let mut journal = journal;
            for index in (0..journal.moves.len()).rev() {
                if !journal.moves[index].done {
                    continue;
                }
                let (from, to) = {
                    let m = &journal.moves[index];
                    (m.from.clone(), m.to.clone())
                };
                rename(&to, &from)?;
                journal.moves[index].done = false;
                write_journal(paths, &journal)?;
            }
        }
    }

    clear_journal(paths)?;
    Ok(Some(recovery))
}
```

- [ ] **Step 5: Declare it**

In `src/desktop/mod.rs`, add `pub mod swap;`.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --test desktop_swap_test`
Expected: PASS, 7 tests.

- [ ] **Step 7: Add the troubleshooting row**

In `docs/troubleshooting.md`, in the error table:

```markdown
| `a desktop profile swap was interrupted and could not be repaired automatically` | byte was interrupted while moving the Claude desktop app's session between accounts, and the journal recording that swap is either unreadable or was written by a newer version of byte. Your session data is still on disk — nothing is deleted by a swap, only moved. | Do not delete the journal. Upgrade byte if the message says the format is newer. Otherwise the journal lists every `from`/`to` pair byte intended, so the move can be completed or reversed by hand. |
```

- [ ] **Step 8: Prove recovery is load-bearing**

Temporarily make `recovery_for` always return `Recovery::RollForward`. `recovery_reverses_when_only_parks_completed` and `an_interruption_during_the_park_is_reversed_and_the_session_comes_back` must both fail. Restore. Note it in the commit body.

- [ ] **Step 9: Full check and commit**

```bash
cargo test --all && cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --all -- --check
git add src/desktop/swap.rs src/desktop/mod.rs src/error.rs docs/troubleshooting.md tests/desktop_swap_test.rs
git commit -m "feat(desktop): execute and recover a journalled profile swap"
```

---

## Task 5: Patching config.json's OAuth keys

**Files:**
- Create: `src/desktop/config.rs`, `tests/desktop_config_test.rs`
- Modify: `src/desktop/mod.rs`

**Interfaces:**
- Consumes: `JsonDocument` (`src/claude/document.rs`), `DesktopPaths` (Task 1).
- Produces: `struct DesktopOauth { token_cache, token_cache_v2, account_uuid }` (all `Option<serde_json::Value>`), `fn capture(config_path: &Path) -> Result<DesktopOauth>`, `fn apply(config_path: &Path, oauth: &DesktopOauth, backup_dir: &Path) -> Result<()>`, `fn clear(config_path: &Path, backup_dir: &Path) -> Result<()>`.

**The three keys, exactly:** `oauth:tokenCache`, `oauth:tokenCacheV2`, `lastKnownAccountUuid`. Everything else in that file — `locale`, `userThemeMode`, `windowSizeWasSignedIn`, updater state, `dxt:allowlist*` — is the user's, and must survive untouched.

- [ ] **Step 1: Write the failing test**

Create `tests/desktop_config_test.rs`:

```rust
use serde_json::json;

use byte::desktop::config::{DesktopOauth, apply, capture, clear};

fn write_config(dir: &std::path::Path) -> std::path::PathBuf {
    let p = dir.join("config.json");
    std::fs::write(
        &p,
        serde_json::to_string_pretty(&json!({
            "locale": "en-GB",
            "oauth:tokenCache": {"access": "aaa"},
            "userThemeMode": "dark",
            "oauth:tokenCacheV2": {"access": "bbb"},
            "lastKnownAccountUuid": "uuid-a",
            "windowSizeWasSignedIn": true
        }))
        .unwrap(),
    )
    .unwrap();
    p
}

#[test]
fn capture_takes_exactly_the_three_owned_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_config(tmp.path());

    let got = capture(&p).unwrap();

    assert_eq!(got.token_cache, Some(json!({"access": "aaa"})));
    assert_eq!(got.token_cache_v2, Some(json!({"access": "bbb"})));
    assert_eq!(got.account_uuid, Some(json!("uuid-a")));
}

#[test]
fn apply_changes_the_owned_keys_and_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_config(tmp.path());
    let backups = tmp.path().join("backups");

    apply(
        &p,
        &DesktopOauth {
            token_cache: Some(json!({"access": "zzz"})),
            token_cache_v2: None,
            account_uuid: Some(json!("uuid-b")),
        },
        &backups,
    )
    .unwrap();

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();

    assert_eq!(after["oauth:tokenCache"], json!({"access": "zzz"}));
    assert_eq!(after["lastKnownAccountUuid"], json!("uuid-b"));
    // Absent in the incoming account: removed, not left holding the old
    // account's token.
    assert!(after.get("oauth:tokenCacheV2").is_none());
    // The user's own settings are untouched.
    assert_eq!(after["locale"], json!("en-GB"));
    assert_eq!(after["userThemeMode"], json!("dark"));
    assert_eq!(after["windowSizeWasSignedIn"], json!(true));
}

#[test]
fn unmodelled_keys_survive_byte_for_byte() {
    // The JsonDocument contract: byte must not reformat a file it does not
    // own. Comparing raw bytes, not parsed values -- a parsed comparison is
    // blind to key order, which is exactly what a naive rewrite destroys.
    let tmp = tempfile::tempdir().unwrap();
    let p = write_config(tmp.path());
    let backups = tmp.path().join("backups");
    let before = std::fs::read_to_string(&p).unwrap();

    let captured = capture(&p).unwrap();
    apply(&p, &captured, &backups).unwrap();

    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        before,
        "a capture/apply round trip must be a no-op on the file"
    );
}

#[test]
fn clear_removes_all_three_and_leaves_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_config(tmp.path());
    let backups = tmp.path().join("backups");

    clear(&p, &backups).unwrap();

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert!(after.get("oauth:tokenCache").is_none());
    assert!(after.get("oauth:tokenCacheV2").is_none());
    assert!(after.get("lastKnownAccountUuid").is_none());
    assert_eq!(after["locale"], json!("en-GB"));
}

#[test]
fn capturing_a_config_without_the_keys_yields_nothing_rather_than_failing() {
    // A fresh install, or one that has never signed in.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("config.json");
    std::fs::write(&p, br#"{"locale":"en-GB"}"#).unwrap();

    let got = capture(&p).unwrap();

    assert_eq!(got.token_cache, None);
    assert_eq!(got.account_uuid, None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test desktop_config_test`
Expected: FAIL — `unresolved import byte::desktop::config`.

- [ ] **Step 3: Write `src/desktop/config.rs`**

```rust
//! The three keys byte owns inside the desktop app's `config.json`.
//!
//! That file is patched, never moved, because it mixes account state with
//! the user's own settings -- `locale`, `userThemeMode`, window sizing,
//! updater state, MCP allowlist caches. Moving it between accounts would
//! drag the user's theme and window layout along with their session.
//!
//! Everything here goes through `JsonDocument`, so every byte it does not
//! explicitly model survives a capture/apply cycle untouched: key order,
//! number formatting, and any key a future app version adds.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::claude::document::JsonDocument;
use crate::error::Result;

const TOKEN_CACHE: &str = "oauth:tokenCache";
const TOKEN_CACHE_V2: &str = "oauth:tokenCacheV2";
const ACCOUNT_UUID: &str = "lastKnownAccountUuid";

/// The desktop app's account identity, as byte stores it.
///
/// Each field is `Option` because an account captured before a given key
/// existed simply has nothing to restore for it -- and because restoring
/// `None` must REMOVE the key rather than leave the previous account's
/// value in place.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopOauth {
    pub token_cache: Option<Value>,
    pub token_cache_v2: Option<Value>,
    pub account_uuid: Option<Value>,
}

/// Read the owned keys out of `config.json`.
pub fn capture(config_path: &Path) -> Result<DesktopOauth> {
    let doc = JsonDocument::load_or_empty(config_path)?;
    Ok(DesktopOauth {
        token_cache: doc.get(TOKEN_CACHE).cloned(),
        token_cache_v2: doc.get(TOKEN_CACHE_V2).cloned(),
        account_uuid: doc.get(ACCOUNT_UUID).cloned(),
    })
}

/// Write the owned keys into `config.json`, removing any the account lacks.
pub fn apply(config_path: &Path, oauth: &DesktopOauth, backup_dir: &Path) -> Result<()> {
    let mut doc = JsonDocument::load_or_empty(config_path)?;

    for (key, value) in [
        (TOKEN_CACHE, &oauth.token_cache),
        (TOKEN_CACHE_V2, &oauth.token_cache_v2),
        (ACCOUNT_UUID, &oauth.account_uuid),
    ] {
        match value {
            Some(v) => doc.set(key, v.clone()),
            // Removal, not "leave it": leaving it would keep the previous
            // account's token cache under the new account's session.
            None => doc.remove(key),
        }
    }

    doc.save(config_path, backup_dir)
}

/// Remove all three, leaving the app signed out.
pub fn clear(config_path: &Path, backup_dir: &Path) -> Result<()> {
    apply(config_path, &DesktopOauth::default(), backup_dir)
}
```

- [ ] **Step 4: Declare it**

In `src/desktop/mod.rs`, add `pub mod config;`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test desktop_config_test`
Expected: PASS, 5 tests.

- [ ] **Step 6: Prove the removal branch is load-bearing**

Temporarily change `None => doc.remove(key)` to `None => {}`. `apply_changes_the_owned_keys_and_nothing_else` must fail on the `oauth:tokenCacheV2` assertion. Restore. Note it in the commit body — leaving that key is precisely how the new account would end up carrying the old account's token.

- [ ] **Step 7: Full check and commit**

```bash
cargo test --all && cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --all -- --check
git add src/desktop/config.rs src/desktop/mod.rs tests/desktop_config_test.rs
git commit -m "feat(desktop): patch the app's OAuth keys in place"
```

---

## Task 6: Detecting that the desktop app is running

**Files:**
- Modify: `src/claude/detect.rs`, `tests/detect_test.rs`

**Interfaces:**
- Consumes: `is_claude_desktop_app` (already exists).
- Produces: `ProcessProbe::desktop_app_running(&self) -> bool` (new trait method), `FakeProbe::with_desktop(count: usize, desktop: bool) -> Self`.

**Note:** `is_claude_desktop_app` already exists to *exclude* the app from Claude Code session counts. This inverts it. Do not duplicate the path matching — call the existing function.

- [ ] **Step 1: Write the failing test**

Append to `tests/detect_test.rs`:

```rust
#[test]
fn a_fake_probe_reports_the_desktop_app_state_it_was_given() {
    assert!(!FakeProbe::with_count(3).desktop_app_running());
    assert!(FakeProbe::with_desktop(0, true).desktop_app_running());
    assert!(!FakeProbe::with_desktop(2, false).desktop_app_running());
    // The two readings are independent: sessions running does not imply the
    // app is, and vice versa.
    assert_eq!(FakeProbe::with_desktop(2, true).running_claude_sessions(), 2);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test detect_test`
Expected: FAIL — no method `desktop_app_running`.

- [ ] **Step 3: Extend the trait and both implementations**

In `src/claude/detect.rs`, add to `ProcessProbe`:

```rust
    /// Whether the Claude desktop app is running.
    ///
    /// The inverse reading of `is_claude_desktop_app`, which exists to
    /// EXCLUDE the app from session counts. A profile swap needs the app
    /// closed -- Chromium corrupts profile state otherwise -- so this gates
    /// the desktop half of a switch.
    fn desktop_app_running(&self) -> bool;
```

In `SysinfoProbe`'s impl:

```rust
    fn desktop_app_running(&self) -> bool {
        use sysinfo::{ProcessRefreshKind, RefreshKind, System, UpdateKind};

        let system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_processes(ProcessRefreshKind::nothing().with_exe(UpdateKind::Always)),
        );

        system
            .processes()
            .values()
            .any(|p| is_claude_desktop_app(p.exe()))
    }
```

In `FakeProbe`, add a `desktop: bool` field, keep `with_count` defaulting it to `false`, and add:

```rust
    pub fn with_desktop(count: usize, desktop: bool) -> Self {
        Self { count, desktop }
    }
```

and

```rust
    fn desktop_app_running(&self) -> bool {
        self.desktop
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test detect_test`
Expected: PASS.

- [ ] **Step 5: Full check and commit**

```bash
cargo test --all && cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --all -- --check
git add src/claude/detect.rs tests/detect_test.rs
git commit -m "feat(detect): report whether the desktop app is running"
```

---

## Task 7: Recording captured profiles in accounts.json

**Files:**
- Modify: `src/store/metadata.rs`, `tests/metadata_test.rs`

**Interfaces:**
- Produces: `AccountMeta.desktop_profile: Option<DesktopProfileRecord>`, `struct DesktopProfileRecord { captured_at: String, bytes: u64 }`.

**Compatibility requirement:** this must NOT bump the accounts schema. The field is optional with `#[serde(default)]`, so an `accounts.json` written by an older byte still loads, and one written by a newer byte still loads in an older build. Bumping the schema would make every existing user's file fail with `AccountsSchemaMismatch` for a purely additive change.

- [ ] **Step 1: Write the failing test**

Append to `tests/metadata_test.rs`:

```rust
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
```

`AccountsFile` has `resolve` returning `&AccountMeta` but no `resolve_mut`. Add one in this task — a three-line mirror of `resolve` returning `&mut AccountMeta` — since Task 8 needs it too, to stamp the record after a capture. If `AccountsFile` does not derive `Default`, add `#[derive(Default)]` rather than inventing a constructor.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test metadata_test`
Expected: FAIL — no field `desktop_profile`.

- [ ] **Step 3: Add the field**

In `src/store/metadata.rs`:

```rust
/// What byte has parked for one account's desktop session.
///
/// Absent means never captured, which is what drives the "park the old
/// profile, leave a fresh one" behaviour on a first switch (design
/// decision 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopProfileRecord {
    /// RFC 3339, matching `added_at`.
    pub captured_at: String,
    /// Size on disk when captured, for `byte list` and for warning about
    /// accumulation.
    pub bytes: u64,
}
```

and on `AccountMeta`:

```rust
    /// The parked desktop profile, if one has been captured.
    ///
    /// `#[serde(default)]` and skipped when absent, deliberately: this is
    /// purely additive, so it must not bump the accounts schema and must not
    /// appear in the files of users who never touch the desktop app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_profile: Option<DesktopProfileRecord>,
```

Every existing construction of `AccountMeta` in `src/` and `tests/` now needs the field. Add `desktop_profile: None` to each — `cargo test --all` will list them.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --all`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/store/metadata.rs tests/
git commit -m "feat(store): record a captured desktop profile per account"
```

---

## Task 8: The desktop switch operation

**Files:**
- Create: `src/ops/desktop.rs`, `tests/ops_desktop_test.rs`
- Modify: `src/ops/mod.rs`

**Interfaces:**
- Consumes: everything from Tasks 1–7.
- Produces: `enum DesktopOutcome { Switched, AppRunning, NoProfileForIncoming, NothingToDo }`, `fn switch_desktop<P: HostPaths, D: DesktopPaths, R: ProcessProbe>(paths: &P, desktop: &D, probe: &R, outgoing: Option<&str>, incoming: &str) -> Result<DesktopOutcome>`.

**The algorithm, exactly (design §7):** Claude Code has already switched by the time this is called — it must never be undone by anything here. If the app is running, return `AppRunning` and change nothing. Otherwise park the outgoing profile *always* (so a session is never lost by switching away), then install the incoming one if it exists, then patch `config.json`.

- [ ] **Step 1: Write the failing test**

Create `tests/ops_desktop_test.rs`:

```rust
use byte::claude::detect::FakeProbe;
use byte::desktop::paths::{DesktopPaths, TestDesktopPaths};
use byte::ops::desktop::{DesktopOutcome, switch_desktop};
use byte::paths::{HostPaths, TestPaths};

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

    let out =
        switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("a"), "b").unwrap();

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

    let out =
        switch_desktop(&tp, &dp, &FakeProbe::with_desktop(0, false), Some("a"), "b").unwrap();

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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test ops_desktop_test`
Expected: FAIL — `unresolved import byte::ops::desktop`.

- [ ] **Step 3: Write `src/ops/desktop.rs`**

```rust
//! Switching the desktop app's session, alongside Claude Code's.
//!
//! Called AFTER Claude Code has already switched. Nothing here may undo
//! that: the CLI switch is the fast, safe, always-available half, and it
//! stays committed whatever happens to the desktop half (design decision 2).

use crate::claude::detect::ProcessProbe;
use crate::desktop::journal::Journal;
use crate::desktop::paths::DesktopPaths;
use crate::desktop::{config, swap};
use crate::error::Result;
use crate::paths::HostPaths;

/// What the desktop half of a switch managed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopOutcome {
    /// Profile parked and the incoming one installed.
    Switched,
    /// The app was running; nothing was touched.
    AppRunning,
    /// Outgoing profile parked, but the incoming account has none stored, so
    /// the app is now signed out and waiting for a login to capture.
    NoProfileForIncoming,
    /// No outgoing account and nothing stored: there was nothing to move.
    NothingToDo,
}

pub fn switch_desktop<P: HostPaths, D: DesktopPaths, R: ProcessProbe>(
    paths: &P,
    desktop: &D,
    probe: &R,
    outgoing: Option<&str>,
    incoming: &str,
) -> Result<DesktopOutcome> {
    // Chromium corrupts profile state if its directories move underneath a
    // running process, so this is a hard gate, not a warning.
    if probe.desktop_app_running() {
        return Ok(DesktopOutcome::AppRunning);
    }

    let live = desktop.desktop_dir();
    let park_to = outgoing.map(|uuid| paths.desktop_profile_dir(uuid));
    let install_from = paths.desktop_profile_dir(incoming);
    let has_incoming = install_from.is_dir();

    // The outgoing account's OAuth keys are captured and written into its
    // parked directory BEFORE any rename runs. Order is load-bearing: once
    // the swap starts, `config.json` is about to be overwritten with the
    // incoming account's values, so capturing afterwards would read back
    // the wrong account -- and a failure mid-swap would lose the outgoing
    // account's identity entirely, leaving its parked cookies unusable.
    if let Some(park_to) = park_to.as_deref() {
        let outgoing_oauth = config::capture(&desktop.config_file())?;
        std::fs::create_dir_all(park_to).map_err(|source| crate::error::Error::Io {
            path: park_to.to_path_buf(),
            source,
        })?;
        let bytes = serde_json::to_vec_pretty(&outgoing_oauth).map_err(|source| {
            crate::error::Error::Parse {
                path: park_to.join("oauth.json"),
                source,
            }
        })?;
        crate::atomic::write(&park_to.join("oauth.json"), &bytes)?;
    }

    let journal = Journal::plan(
        &live,
        park_to.as_deref(),
        has_incoming.then_some(install_from.as_path()),
    )?;

    if journal.moves.is_empty() && !has_incoming && outgoing.is_none() {
        return Ok(DesktopOutcome::NothingToDo);
    }

    swap::execute(paths, journal)?;

    // The config patch follows the moves, not the other way round: if the
    // moves fail, the app's identity should still name whatever session is
    // actually in place.
    let backups = paths.backup_dir();
    if has_incoming {
        let stored = install_from.join("oauth.json");
        let oauth = match std::fs::read(&stored) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => config::DesktopOauth::default(),
        };
        config::apply(&desktop.config_file(), &oauth, &backups)?;
        Ok(DesktopOutcome::Switched)
    } else {
        // Signed out: the user logs in as the new account and byte captures.
        config::clear(&desktop.config_file(), &backups)?;
        Ok(DesktopOutcome::NoProfileForIncoming)
    }
}
```

- [ ] **Step 3b: Test that the outgoing identity is captured before the swap**

Append to `tests/ops_desktop_test.rs`:

```rust
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
```

Run it, then temporarily move the capture block below `swap::execute` and confirm it fails. Restore.

- [ ] **Step 3c: Stamp the parked profile into accounts.json**

The `desktop_profile` record from Task 7 is written here — it is what makes `byte list` able to answer "which accounts have a stored desktop session, and what is it costing me in disk". Without this step the field is dead weight.

After `swap::execute` succeeds, and only when a profile was parked:

```rust
/// Total size of a directory tree, for the stored-profile record.
///
/// Best-effort: an unreadable entry contributes zero rather than failing the
/// switch. This number is for display, and a switch that already committed
/// must never be turned into an `Err` by a reporting detail.
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}
```

and in `switch_desktop`, after the swap:

```rust
    if let (Some(uuid), Some(park_to)) = (outgoing, park_to.as_deref()) {
        let mut accounts = AccountsFile::load(&paths.accounts_file())?;
        if let Ok(meta) = accounts.resolve_mut(uuid) {
            meta.desktop_profile = Some(DesktopProfileRecord {
                captured_at: crate::store::metadata::now_rfc3339(),
                bytes: dir_size(park_to),
            });
            accounts.save(&paths.accounts_file(), &paths.backup_dir())?;
        }
    }
```

Use whatever timestamp helper `metadata.rs` already uses for `added_at` rather than introducing a second one — read the file and match it.

Add to `tests/ops_desktop_test.rs`:

```rust
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
    assert!(rec.bytes > 0, "a parked profile with files in it must record a nonzero size");
    assert!(!rec.captured_at.is_empty());
}
```

Write `sample_snapshot` as a local helper mirroring `snap` in `tests/metadata_test.rs`.

- [ ] **Step 4: Declare it**

In `src/ops/mod.rs`, add `pub mod desktop;`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test ops_desktop_test`
Expected: PASS.

- [ ] **Step 6: Full check and commit**

```bash
cargo test --all && cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --all -- --check
git add src/ops/desktop.rs src/ops/mod.rs tests/ops_desktop_test.rs
git commit -m "feat(ops): switch the desktop session alongside Claude Code"
```

---

## Task 9: Wiring into the CLI

**Files:**
- Modify: `src/cli/run.rs`, `tests/cli_test.rs`
- Modify: `docs/troubleshooting.md`

**Interfaces:**
- Consumes: `switch_desktop`, `DesktopOutcome` (Task 8); `recover_if_interrupted` (Task 4).

- [ ] **Step 1: Write the failing test**

First extend the `byte()` helper in `tests/cli_test.rs` to set `CLAUDE_DESKTOP_DIR` to `tp.root().join("desktop")`, so the compiled binary can never reach a real `%APPDATA%\Claude`:

```rust
        .env("CLAUDE_DESKTOP_DIR", tp.root().join("desktop"))
```

Then append this test:

```rust
#[test]
fn any_command_repairs_an_interrupted_desktop_swap() {
    // Uses `list`, deliberately, for two reasons. First, the requirement is
    // that the journal is repaired by whatever byte command runs NEXT --
    // not only by a retry of `switch` -- and nothing else in this plan
    // tests that. Second, `list` is keychain-free: a SUCCESSFUL `switch`
    // through the compiled binary reaches secrets.get(), which for this
    // binary is the REAL OS keychain. That is why this suite only ever
    // runs `switch <unknown-name>`, which fails at resolution first.
    let tp = TestPaths::new().unwrap();
    seed_one_account(&tp);

    let live = tp.root().join("desktop");
    std::fs::create_dir_all(&live).unwrap();

    // A half-completed park: the directory is already in the store, and the
    // journal records that one move as done.
    let parked = tp.desktop_profile_dir("u1").join("Network");
    std::fs::create_dir_all(&parked).unwrap();
    std::fs::write(parked.join("marker.txt"), "account-1").unwrap();
    std::fs::create_dir_all(tp.desktop_store_dir()).unwrap();
    std::fs::write(
        tp.desktop_journal_file(),
        serde_json::json!({
            "version": 1,
            "outgoing": "u1",
            "incoming": null,
            "moves": [{
                "stage": "Park",
                "from": live.join("Network"),
                "to": parked,
                "done": true
            }]
        })
        .to_string(),
    )
    .unwrap();

    let out = byte(&tp, &["list"]);

    assert!(
        out.status.success(),
        "list failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        live.join("Network").join("marker.txt").exists(),
        "no install move completed, so the park must be reversed and the session restored"
    );
    assert!(
        !tp.desktop_journal_file().exists(),
        "the journal must be cleared once repaired, or every later command repeats the repair"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.to_lowercase().contains("repaired"),
        "a silent repair is indistinguishable from nothing having happened: {stderr}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test cli_test any_command_repairs`
Expected: FAIL — the journal is left in place and the directory is not restored, because nothing checks for a journal yet.

- [ ] **Step 3: Add the journal check to every command**

In `src/cli/run.rs`'s `run`, immediately after `RealPaths::discover()`:

```rust
    // A half-swap must be repaired by whatever byte command runs next, not
    // only by a retry of the one that failed -- so this is here, before the
    // dispatch, rather than inside `cmd_switch`.
    if let Some(recovery) = crate::desktop::swap::recover_if_interrupted(&paths)? {
        output::warn(&format!(
            "repaired an interrupted desktop profile swap ({recovery:?})."
        ));
    }
```

- [ ] **Step 4: Call the desktop half from `cmd_switch`**

After the Claude Code switch has committed and its result is rendered, and only in the non-`--json` path (JSON consumers get a field instead of a message):

```rust
    match ops::desktop::switch_desktop(paths, desktop, probe, outgoing.as_deref(), &meta.uuid)? {
        DesktopOutcome::Switched => output::status("Claude desktop app switched too."),
        DesktopOutcome::AppRunning => output::warn(
            "Claude is running, so its desktop session was left on the previous account. \
             Quit Claude and run this switch again to move it too.",
        ),
        DesktopOutcome::NoProfileForIncoming => output::warn(
            "No desktop session stored for this account yet, so Claude will open signed out. \
             Sign in there once and byte will remember it.",
        ),
        DesktopOutcome::NothingToDo => {}
    }
```

Note: a `switch_desktop` failure must NOT turn an already-committed Claude Code switch into an `Err`. Catch it, report it through `output::warn`, and keep the overall command successful — this is the same "never fail an operation that already succeeded" rule that `notify::send` and `atomic::prune` follow. Add a test that a failing desktop half still exits zero.

- [ ] **Step 4b: Surface stored profiles in `byte list`**

This is what the `desktop_profile` record exists for. In `cmd_list`'s human-readable output, mark accounts with a stored desktop session and its size; in `--json`, include the record as a `desktop_profile` object (or `null`).

Add to `tests/cli_test.rs`, which is keychain-free because `list` only reads `accounts.json`:

```rust
#[test]
fn list_json_reports_a_stored_desktop_profile() {
    let tp = TestPaths::new().unwrap();
    seed_one_account(&tp);

    let before = byte(&tp, &["list", "--json"]);
    let parsed: serde_json::Value = serde_json::from_slice(&before.stdout).unwrap();
    assert!(
        parsed[0]["desktop_profile"].is_null(),
        "an account with no stored profile must report null, not omit the field: {parsed}"
    );
}
```

Pair it with a case that seeds a record and asserts the size is reported, so a `null`-always implementation fails.

- [ ] **Step 5: Full check and commit**

```bash
cargo test --all && cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --all -- --check
git add src/cli/run.rs tests/cli_test.rs docs/troubleshooting.md
git commit -m "feat(cli): switch the desktop session as part of byte switch"
```

---

## Task 10: Docs, store permissions, and the verification gap

**Files:**
- Modify: `README.md`, `docs/configuration.md`, `docs/troubleshooting.md`, `docs/architecture.md`, `AGENTS.md`, `man/byte.md`
- Modify: `src/desktop/swap.rs` (ACL check)

- [ ] **Step 1: Restrict the profile store**

Parked profiles hold live session credentials as plain files (design §6), so the store must not be created with default-permissive permissions.

Add to `src/desktop/swap.rs`, and call it from `switch_desktop` before the first write:

```rust
/// Create the profile store with owner-only access.
///
/// This is a partial measure on Windows and the plan says so rather than
/// implying otherwise. A real per-directory ACL needs the `windows` crate,
/// which this change deliberately does not add; instead the store is created
/// under byte's config directory, which lives beneath `%APPDATA%` and
/// inherits that location's user-scoped ACL. That is the same protection
/// `accounts.json` already relies on -- but `accounts.json` holds metadata,
/// and this holds cookies, so the weaker guarantee is worth naming.
pub fn create_store_dir(paths: &impl HostPaths) -> Result<()> {
    let dir = paths.desktop_store_dir();
    std::fs::create_dir_all(&dir).map_err(|source| Error::Io {
        path: dir.clone(),
        source,
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(&dir, perms).map_err(|source| Error::Io {
            path: dir.clone(),
            source,
        })?;
    }

    Ok(())
}
```

Test it in `tests/desktop_swap_test.rs`:

```rust
#[test]
fn the_profile_store_is_created_owner_only() {
    let tp = TestPaths::new().unwrap();
    byte::desktop::swap::create_store_dir(&tp).unwrap();
    assert!(tp.desktop_store_dir().is_dir());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(tp.desktop_store_dir())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "the store must not be group/world readable");
    }
}
```

Then open a follow-up issue: "Harden the desktop profile store's Windows ACL", noting that it currently inherits `%APPDATA%`'s ACL rather than setting an explicit owner-only one, and that doing so properly needs a Win32 dependency. Do not describe the store as ACL-restricted in the docs until that lands — say it inherits the config directory's permissions, which is what is true.

- [ ] **Step 2: Document the security departure**

In `README.md` and `docs/configuration.md`, state plainly that `<byte config>/desktop/` contains live session credentials as ordinary files, that this is the one place byte stores a secret outside the OS keychain, and why (cookies cannot go in a keychain).

- [ ] **Step 3: Update the docs sync points**

- `README.md`: what `byte switch` now does to the desktop app, including the signed-out-on-first-switch behaviour and that Code-tab history travels with the account.
- `docs/architecture.md`: add `desktop/` to the module map and the dependency tree.
- `AGENTS.md`: name `desktop/` in the architecture summary.
- `man/byte.md`: the desktop half of **switch**, and `CLAUDE_DESKTOP_DIR`.
- `docs/troubleshooting.md`: rows for every new user-visible message from Task 9.

- [ ] **Step 4: Record the verification gap**

Add a short section to `README.md` (or `docs/troubleshooting.md`) stating that desktop switching has not been verified against a real signed-in app, what would verify it, and that until then it should be treated as unproven. Do not describe it as tested.

- [ ] **Step 5: Full check and commit**

```bash
cargo test --all && cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --all -- --check
git add -A
git commit -m "docs: document desktop switching and its credential store"
```

---

## After the plan

**Human verification is required and cannot be delegated** (design §12). No test in this plan answers whether swapping this exact set actually switches the real app, and it cannot be answered from an environment running inside that app. It needs a person, a second Claude account, and a willingness to be signed out mid-session:

1. `byte capture` the current desktop session by switching away and back once.
2. Quit Claude entirely. Confirm no `claude.exe` under `AppData\Local\AnthropicClaude` remains.
3. `byte switch <other account>`.
4. Open Claude. Confirm it is signed in as the other account — chat side *and* Code tab.
5. Switch back. Confirm the original session returns, including conversation history.

If step 4 shows a signed-out app rather than the other account, the OAuth-cache half is insufficient and the finding belongs in the spec before anything ships.
