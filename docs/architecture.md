# Architecture of byte

byte moves a small, self-contained `AccountSnapshot` — one account's OAuth
credentials plus its identity — between Claude Code's live config files and
two per-account stores: an OS keychain entry for the secret half, and an
entry in byte's own `accounts.json` for the rest. Everything else in Claude
Code's files is opaque to byte and passes through untouched.

## Module layout

```
src/
  main.rs          entry point: parse argv, dispatch, map errors to an exit code
  lib.rs           public module tree
  error.rs         Error enum (thiserror) + Result alias, used everywhere
  output.rs        the only place allowed to call println!/eprintln!
  paths.rs         HostPaths trait; RealPaths (env + home dir) and TestPaths (tempdir)
  atomic.rs        atomic file replace, timestamped backup, backup pruning
  lock.rs          MutationGuard (per-write) and InstanceGuard (per-tray-lifetime) advisory locks
  autostart.rs     register/unregister byte's tray as a login item, per platform
  claude/
    document.rs    JsonDocument — load/patch/save JSON preserving key order and style
    snapshot.rs    AccountSnapshot type, field accessors, validation
    files.rs       ClaudeFiles — capture/apply/clear a snapshot against the live files
    detect.rs      ProcessProbe trait + SysinfoProbe — counts running Claude Code sessions
  store/
    metadata.rs    AccountsFile, AccountMeta — accounts.json, name resolution
    secrets.rs     SecretStore trait (oauth block only), KeyringStore (OS keychain), MemoryStore (tests)
  desktop/         Switching the Claude DESKTOP app's session (Windows only)
    paths.rs       DesktopPaths trait; RealDesktopPaths (the app's real %APPDATA%\Claude) and TestDesktopPaths (tempdir)
    profile.rs     classifies one entry in the app's data directory as Move / Patch / Leave (pure, denylist-based)
    journal.rs     Journal/Move/Stage types, Journal::plan (parks before installs), JSON (de)serialization (pure)
    config.rs      the three config.json keys byte owns (oauth:tokenCache, oauth:tokenCacheV2, lastKnownAccountUuid), patched via JsonDocument
    swap.rs        create_store_dir; executing a planned journal (execute) and repairing an interrupted one (recover_if_interrupted) against the real filesystem
  ops/
    switch.rs      Switcher — capture, sync-back, switch (the core algorithm)
    add.rs         AddSession — logout-and-watch add flow
    manage.rs      list / current / rename / remove
    desktop.rs     switch_desktop — the desktop app's half of a switch, run after ops::switch has already committed the Claude Code half
  cli/
    mod.rs         clap command definitions (the Cli and Command types)
    run.rs         dispatches a parsed Cli to an op and renders the result
  tray/
    mod.rs         platform gate: re-exports app::run on Windows/macOS, an honest Error::Tray elsewhere
    app.rs         the winit/tray-icon event loop (Windows/macOS only): builds the tray, dispatches menu and click events
    menu.rs        MenuModel — turns an account listing into the menu's entries, independent of any UI toolkit
    events.rs      classifies a raw menu/tray event into an Action, independent of winit/tray-icon types
    launch.rs      builds the cmd.exe / AppleScript command that opens a terminal running `byte add`
    notify.rs      best-effort desktop notifications after a tray-driven switch
    watch.rs       AccountsWatcher — watches accounts.json for external changes and triggers a menu rebuild
```

## Dependency direction

Dependencies flow one way, from the CLI and the tray down to the primitives.
Nothing in a lower layer imports from a higher one:

```
main.rs
  └── cli::run::run
        ├── every command: desktop::swap::recover_if_interrupted
        │     (repairs a crashed swap, gated by lock::MutationGuard —
        │      see recover_under_lock and "Cross-cutting concerns" below)
        ├── no arguments: tray::run
        │     │  (== app::run on Windows/macOS; an immediate Error::Tray elsewhere)
        │     ├── tray (menu, events, launch, notify, watch)
        │     ├── ops (switch, manage)
        │     ├── claude::detect   (the running-sessions notification)
        │     └── lock (MutationGuard, InstanceGuard)
        └── a command: ops (switch, add, manage, desktop) / autostart
              ├── desktop::paths   (RealDesktopPaths::discover, Command::Switch only)
              ├── claude::detect   (the running-sessions warning)
              └── lock (MutationGuard, mutating commands only)

ops (switch, add, manage)
  ├── claude (document, snapshot, files)
  ├── store (metadata, secrets)
  └── paths

ops::desktop (switch_desktop — called from cli::run::cmd_switch only after
              ops::switch has already committed the Claude Code half; never
              undoes that half, whatever happens to the desktop half)
  ├── desktop (paths, profile, journal, config, swap)
  ├── claude::detect   (ProcessProbe — refuses to touch a running app)
  ├── store::metadata  (the desktop_profile bookkeeping record)
  └── paths            (HostPaths — byte's OWN config dir, not the app's)

desktop::config
  └── claude::document (JsonDocument — patches config.json's three owned keys)

error, output, paths, atomic
  are used from every layer above them
```

- **`cli/`** and **`tray/`** sit at the same layer: both are entry points
  that call down into `ops`, `lock`, `claude::detect`, and (`cli/` only)
  `autostart` and `desktop` (`recover_under_lock`'s crash recovery on every
  command, and `RealDesktopPaths`/`ops::desktop::switch_desktop` for
  `Command::Switch` specifically) — `tray/`'s own switch never touches
  `desktop` at all, so a tray-driven switch changes Claude Code only. Apart
  from that asymmetry neither is a dependency of the other. `main.rs` picks
  between them once, based on whether any command-line arguments were given
  (`cli::run::run` itself makes that choice and calls `tray::run` directly
  for the no-arguments case) — nothing downstream needs to know which one is
  driving it. `cli/` renders to stdout/stderr via `output.rs`; `tray/`
  renders to the tray icon, its menu, and best-effort OS notifications
  (`tray::notify`), but both call the same `ops` functions to actually
  change anything, and both take the same `lock::MutationGuard` around a
  write for the same reason: so a CLI switch and a tray switch can never
  interleave.
- **`ops/`** composes `claude/` and `store/` into the operations both
  front ends expose (switch, add, list, ...). It is generic over the
  `HostPaths` and `SecretStore` traits rather than depending on their
  concrete implementations, which is what lets the test suite exercise the
  full algorithm against a `TempDir` and an in-memory store with no
  keychain and no real Claude Code installation.
- **`claude/`** knows the shape of Claude Code's two files and how to
  capture and apply an `AccountSnapshot` against them, and (`detect.rs`)
  how to count running Claude Code processes on the host — but nothing
  about where the config files live or how accounts are stored between
  switches.
- **`store/`** knows how to persist account metadata and secrets, but
  nothing about Claude Code's file formats.
- **`desktop/`** (Windows only) knows the shape of the Claude *desktop*
  app's data directory — which entries move with an account, which single
  file is patched in place, and which are left alone — and how to execute
  and recover a multi-directory swap between two accounts' profiles via a
  journal (`desktop::journal`, `desktop::swap`). It depends only on
  `claude::document` (to patch `config.json` the same way Claude Code's own
  files are patched), never on `claude::snapshot` or `claude::files`: the
  desktop app's session and Claude Code's credentials are switched by
  entirely separate mechanisms that happen to run back to back.
  **`ops::desktop::switch_desktop`** is the seam that calls into it: it is
  invoked from `cli::run::cmd_switch` only *after* `ops::switch::Switcher`
  has already committed the Claude Code half, and nothing in `desktop/` or
  `ops::desktop` may undo that commit — a failure here is reported, never
  propagated as an undo of a switch that already happened.
- **`lock.rs`** and **`autostart.rs`** are used by `cli/` and/or `tray/`
  but depend on nothing above `paths` and `error`; `tray/`'s own submodules
  (`menu.rs`, `events.rs`) are in turn kept independent of `ops` and the
  windowing toolkit so they stay unit-testable without either.
- **`error.rs`**, **`output.rs`**, **`paths.rs`**, and **`atomic.rs`** are
  primitives with no dependency on anything above them.

## Major data types

- **`AccountSnapshot`** (`claude/snapshot.rs`) — the unit of account identity
  byte moves around: the `claudeAiOauth` object, the `oauthAccount` object,
  and `userID`, held as opaque `serde_json::Value` so unknown upstream fields
  survive a capture/apply cycle untouched. Short-lived and never persisted
  as a whole: `capture()` builds one from the live files, and it is
  immediately split into its two stored halves (below). `Switcher` puts one
  back together with `AccountSnapshot::reassemble` whenever an op needs a
  complete snapshot to hand to `ClaudeFiles::apply`.
- **`JsonDocument`** (`claude/document.rs`) — a parsed JSON object plus the
  formatting details (pretty vs. compact, trailing newline) needed to write
  it back without producing a spurious diff.
- **`AccountsFile` / `AccountMeta`** (`store/metadata.rs`) — the persisted,
  non-secret half of an account: display fields (label, email, organization,
  subscription tier) plus the raw `oauthAccount` object, `userID`, and the
  captured schema version that reassembly needs to rebuild a complete
  snapshot. This is what `accounts.json` serializes to and what `byte list`
  reads; it never touches the keychain, and never carries the secret `oauth`
  block (`accessToken`/`refreshToken`) — that split is what keeps a single
  account's keychain entry under the OS credential store's size limit (see
  `store/secrets.rs` and [Configuration](configuration.md)).
- **`Switcher<P, S>`** (`ops/switch.rs`) — generic over `P: HostPaths` and
  `S: SecretStore`, constructed once per CLI invocation and passed by
  reference into every op. This is the seam that makes the algorithm
  testable: production code instantiates `Switcher<&RealPaths,
  KeyringStore>`, tests instantiate `Switcher<&TestPaths, MemoryStore>`.
  Its `load_snapshot` method is where the two storage halves above are
  reassembled and validated together — see "Command flow" below.
- **`Journal`** (`desktop/journal.rs`) — the whole planned sequence of
  directory renames for one desktop-profile swap, as `Move { stage, from,
  to, done }` entries. Written to disk in full, via `desktop::swap`, before
  the first rename runs, and deleted only once every move has completed;
  this is what lets `desktop::swap::recover_if_interrupted` repair a swap a
  crash left half-done, on whichever byte command runs next rather than
  only a retry of `switch`.
- **`DesktopOauth`** (`desktop/config.rs`) — the three keys byte owns inside
  the desktop app's `config.json` (`oauth:tokenCache`, `oauth:tokenCacheV2`,
  `lastKnownAccountUuid`), each `Option` so that restoring an account that
  predates a given key removes it rather than leaving the previous
  account's value in place.
- **`DesktopOutcome`** (`ops/desktop.rs`) — what `switch_desktop` managed to
  do: `Switched`, `AppRunning`, `NoProfileForIncoming`, `IdentityMismatch`,
  or `NothingToDo`. Returned as `Ok`, never as a failure that would
  propagate past the Claude Code switch that already committed by the time
  `switch_desktop` runs — see the `ops::desktop` bullet above.

## Command flow, end to end

`byte switch work`, on Windows, with the desktop app closed:

1. `main.rs` parses argv into a `Cli` via `clap::Parser`.
2. `cli::run::run` first calls `recover_under_lock`, which repairs any
   desktop-profile swap an earlier crash left behind — but only while it can
   take `lock::MutationGuard` without contending with a swap actually in
   progress elsewhere; see "Cross-cutting concerns" below. This runs before
   every command, not just `switch`.
3. `cli::run::run` builds a `Switcher<&RealPaths, KeyringStore>` from
   `RealPaths::discover()` (honoring `CLAUDE_CONFIG_DIR` / `BYTE_CONFIG_DIR`),
   and dispatches on the parsed `Command`. For the mutating commands only
   (`switch`, `capture`, `add`, `remove`, `rename`) that arm first takes
   `lock::MutationGuard`, so a concurrent tray-driven switch cannot
   interleave its writes with this one; `list`, `current`, and `autostart`
   never take it. For `Command::Switch` specifically, it also tries
   `RealDesktopPaths::discover()`, degrading to "no desktop half attempted"
   rather than failing the command when no desktop app directory can be
   found (the ordinary state on every non-Windows machine).
4. `cmd_switch` calls `Switcher::switch_to("work")`, which: resolves `"work"`
   against `accounts.json` before touching anything; syncs the currently
   live account back to the store so a token Claude Code rotated isn't lost;
   reassembles the target account's complete snapshot via `load_snapshot`
   (the `oauth` block from `SecretStore`, combined with the `account` /
   `user_id` / schema recorded alongside it in `accounts.json`, then
   validated as a unit); applies it to the live files via `ClaudeFiles::apply`
   (credentials written first, config second, with a credentials rollback if
   the config write fails); and updates `accounts.json` to mark the new
   account active. This half is unconditional and unaffected by anything
   below it.
5. If desktop paths were found (step 3) and this was not a no-op switch,
   `cmd_switch` calls `ops::desktop::switch_desktop`, which parks the
   outgoing account's live desktop session into its store, installs the
   incoming account's stored session if one exists (or leaves the app
   signed out if not), and patches `config.json`'s three owned keys to
   match — all through a `Journal` (above) so a crash partway through is
   repaired rather than left half-applied. Its `DesktopOutcome` is reported
   but can never turn the switch in step 4 into a failure — see the
   `ops::desktop` bullet under "Dependency direction" above.
6. `cmd_switch` renders the outcome of both halves — either as JSON on
   stdout (`--json`, with the desktop half's outcome under a `desktop` key)
   or as status/warning lines on stderr via `output.rs`.
7. `main` maps `Ok`/`Err` to a process exit code, printing any error through
   `output::error`.

## Cross-cutting concerns

- **Errors.** A single `thiserror` enum (`error.rs`) covers every fallible
  path in the crate. Ops functions return `Result<T, Error>`; `main.rs` is
  the only place an `Error` is turned into user-facing text and an exit
  code.
- **Output.** All user-facing text goes through `output.rs`'s semantic
  helpers (`status`, `warn`, `info`, `header`, `error`, `confirm` on stderr;
  `data` on stdout). No other module calls `println!`/`eprintln!` directly,
  which is what keeps `--json` output pipeable — machine-readable data never
  shares a stream with a status message.
- **Configuration.** Three environment variables, resolved once each in
  `paths::RealPaths::discover()` (`CLAUDE_CONFIG_DIR`, `BYTE_CONFIG_DIR`) and
  `desktop::paths::RealDesktopPaths::discover()` (`CLAUDE_DESKTOP_DIR`); see
  [Configuration](configuration.md).
- **Safety.** Every write to a Claude Code file goes through
  `atomic.rs` (temp file in the same directory, fsync, atomic rename) and is
  preceded by a timestamped backup and followed by a read-back verification,
  regardless of which op triggered it.
- **Desktop crash safety.** A profile swap is many separate directory
  renames, not one file write, so it cannot use `atomic.rs`'s single-write
  guarantee. `desktop::journal::Journal` supplies the equivalent for that
  shape: the entire planned sequence is written to disk before the first
  rename runs, updated as each one completes, and cleared only on success.
  `desktop::swap::recover_if_interrupted` replays or reverses whatever it
  finds at the start of every byte command (via `cli::run::recover_under_lock`),
  not only on a retry of `switch` — see the `Journal` bullet above.
