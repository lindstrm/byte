//! Executing CLI commands and rendering their results.

use std::io::IsTerminal as _;

use crate::autostart;
use crate::claude::detect::{ProcessProbe, SysinfoProbe};
use crate::cli::{AutostartAction, Cli, Command};
use crate::desktop::paths::{DesktopPaths, RealDesktopPaths};
use crate::error::{Error, Result};
use crate::lock::MutationGuard;
use crate::ops::add::AddSession;
use crate::ops::desktop::{DesktopOutcome, switch_desktop};
use crate::ops::manage::{self, AccountListing};
use crate::ops::switch::{SwitchOutcome, Switcher, SyncOutcome};
use crate::output;
use crate::paths::{HostPaths, RealPaths};
use crate::store::secrets::{KeyringStore, SecretStore};
use crate::tray;

/// How often the add flow checks for a completed login.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

pub fn run(cli: Cli) -> Result<()> {
    let paths = RealPaths::discover()?;

    // A half-completed desktop swap must be repaired by whatever byte
    // command runs next, not only by a retry of the `switch` that was
    // interrupted -- so this runs here, before the dispatch below (and
    // before the tray-vs-CLI fork just past it), rather than being folded
    // into `cmd_switch`. Safe to run unconditionally on every command and
    // every platform: `desktop_journal_file` lives under byte's OWN config
    // directory (via `HostPaths`, already resolved above), never under the
    // desktop app's real `%APPDATA%\Claude` -- a machine that has never run
    // a desktop swap has no such file, so this is a cheap `Ok(None)` the
    // rest of the time.
    //
    // A recovery FAILURE (an unreadable journal, or one written by an
    // incompatible format version) must not be allowed to propagate via
    // `?` here: this runs before every command, including read-only ones
    // that never touch the desktop machinery at all (`list`, `current`,
    // `autostart`, even starting the tray with no arguments). Letting it
    // fail `run()` would turn one corrupt file into total unavailability
    // of the whole CLI, with no way to run byte again short of editing the
    // journal by hand -- there is no weaker mode this reduces to. `switch`
    // keeps its own, narrower protection regardless of what happens here:
    // `switch_desktop`'s own precondition check still refuses to start a
    // NEW swap over an unrepaired journal, and that refusal is reported
    // the same "catch and warn" way by `report_desktop_outcome`, without
    // failing the Claude Code switch that already committed.
    match crate::desktop::swap::recover_if_interrupted(&paths) {
        Ok(Some(recovery)) => output::warn(&format!(
            "repaired an interrupted desktop profile swap ({recovery:?})."
        )),
        Ok(None) => {}
        Err(e) => output::warn(&format!(
            "an earlier desktop profile swap could not be repaired automatically, so this \
             command is continuing without touching it: {e}"
        )),
    }

    // No arguments starts the tray rather than listing accounts (see
    // `Cli::long_about`). Handled before `Switcher` even exists: `tray::run`
    // takes `paths` by value, and every other branch below only ever needs a
    // borrow of it.
    let Some(command) = cli.command else {
        return tray::run(paths);
    };

    let switcher = Switcher::new(&paths, KeyringStore::new());
    let probe = SysinfoProbe::new();

    // Read-only commands (list, current, autostart) do not take the
    // mutation lock: they tolerate a concurrent write because every file
    // byte writes is replaced atomically, so there is never a torn read to
    // guard against.
    match command {
        Command::List => cmd_list(&switcher, cli.json),
        Command::Current => cmd_current(&switcher, cli.json),
        Command::Autostart { action } => cmd_autostart(action, cli.json),
        Command::Switch { name } => {
            // Bound to a named variable, not `_`: `let _ = ...` would drop
            // the guard -- and release the lock -- immediately, before
            // `cmd_switch` below ever ran. Naming it (with a leading
            // underscore only to silence the unused-variable lint) keeps it
            // alive until this arm's block ends, which is after the command
            // completes.
            let _guard = MutationGuard::acquire(&paths)?;

            // `RealDesktopPaths::discover` fails when neither
            // `CLAUDE_DESKTOP_DIR` nor `%APPDATA%` is set, which is the
            // ordinary state of every non-Windows machine -- this feature
            // only understands the Windows desktop app's layout (see
            // `src/desktop/paths.rs`). `.ok()` degrades that to "the
            // desktop half is not attempted" rather than failing `byte
            // switch` outright: the desktop switch is an addition to the
            // Claude Code switch, never a precondition for it, and warning
            // about a directory that will never exist on those platforms
            // on every single invocation would be permanent noise, not a
            // fixable problem. A genuine failure to switch the desktop
            // half once paths ARE available is a different matter, and is
            // reported -- see `cmd_switch`'s own handling.
            let desktop = RealDesktopPaths::discover().ok();
            cmd_switch(&switcher, &name, cli.json, &probe, desktop.as_ref())
        }
        Command::Capture => {
            let _guard = MutationGuard::acquire(&paths)?;
            cmd_capture(&switcher, cli.json)
        }
        // The only mutating arm that does NOT take the lock here: `cmd_add`
        // takes it itself, after its confirmation prompt. See the comment at
        // that acquisition for why the prompt must not be held under lock.
        Command::Add { timeout, yes } => cmd_add(&switcher, timeout, yes, cli.json),
        Command::Remove { name, yes } => {
            let _guard = MutationGuard::acquire(&paths)?;
            cmd_remove(&switcher, &name, yes, cli.json)
        }
        Command::Rename { name, label } => {
            let _guard = MutationGuard::acquire(&paths)?;
            cmd_rename(&switcher, &name, &label, cli.json)
        }
    }
}

fn listing_json(l: &AccountListing) -> serde_json::Value {
    serde_json::json!({
        "label": l.meta.label,
        "email": l.meta.email,
        "organization": l.meta.organization_name,
        "subscription": l.meta.subscription_type,
        "uuid": l.meta.uuid,
        "active": l.active,
        "last_used_at": l.meta.last_used_at,
        // `DesktopProfileRecord` derives `Serialize`, so `None` becomes the
        // JSON literal `null` here -- explicitly, as a key every object
        // carries, never omitted. An account with no stored desktop
        // profile is not the same fact as a script that can't tell whether
        // byte's build even knows about the field.
        "desktop_profile": l.meta.desktop_profile,
    })
}

fn cmd_list<P: HostPaths + Copy, S: SecretStore>(sw: &Switcher<P, S>, json: bool) -> Result<()> {
    let listing = manage::list(sw)?;

    if json {
        let payload: Vec<_> = listing.iter().map(listing_json).collect();
        let text =
            serde_json::to_string_pretty(&payload).map_err(|e| Error::Render(e.to_string()))?;
        output::data(&text);
        return Ok(());
    }

    if listing.is_empty() {
        output::info("No accounts stored yet. Run `byte capture` to save the current one.");
        return Ok(());
    }

    output::header("Accounts");
    for l in &listing {
        let mark = if l.active { "*" } else { " " };
        let org = l.meta.organization_name.as_deref().unwrap_or("-");
        match &l.meta.desktop_profile {
            Some(rec) => output::info(&format!(
                "{mark} {}  ({org})  [desktop session: {}]",
                l.meta.label,
                format_size(rec.bytes)
            )),
            None => output::info(&format!("{mark} {}  ({org})", l.meta.label)),
        }
    }
    Ok(())
}

/// A human-readable size for `byte list`'s desktop-session note. Display
/// only: `--json` reports the raw byte count in `desktop_profile.bytes`
/// instead, which is what a script should parse.
///
/// `pub`, like `switch_json` and `running_sessions_warning`, specifically so
/// `tests/cli_run_test.rs` can pin its exact formatting directly.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn cmd_current<P: HostPaths + Copy, S: SecretStore>(sw: &Switcher<P, S>, json: bool) -> Result<()> {
    match manage::current(sw)? {
        Some(meta) if json => {
            output::data(&serde_json::json!({"label": meta.label, "uuid": meta.uuid}).to_string());
        }
        Some(meta) => output::data(&meta.label),
        None if json => output::data("null"),
        None => output::info("No active account."),
    }
    Ok(())
}

fn cmd_autostart(action: AutostartAction, json: bool) -> Result<()> {
    match action {
        AutostartAction::Status => {
            let on = autostart::status()?;
            if json {
                output::data(&serde_json::json!({ "autostart": on }).to_string());
            } else if on {
                output::info(&format!(
                    "byte starts at login (via {}).",
                    autostart::describe_location()
                ));
            } else {
                output::info("byte does not start at login.");
            }
        }
        AutostartAction::Enable => {
            autostart::enable()?;
            output::status(&format!(
                "byte will start at login, via {}.",
                autostart::describe_location()
            ));
        }
        AutostartAction::Disable => {
            autostart::disable()?;
            output::status("byte will no longer start at login.");
        }
    }
    Ok(())
}

fn report_sync(sync: &SyncOutcome) {
    if let SyncOutcome::Captured(meta) = sync {
        output::status(&format!("Saved previously unknown account {}", meta.label));
    }
}

/// Build `switch --json`'s payload. `pub` (like `resolve_add_failure`)
/// specifically so its shape is directly testable without a keychain --
/// see `tests/cli_run_test.rs`.
///
/// Includes `sync`: without it, a script has no way to learn that
/// sync-back just wrote a previously unknown account's refresh token to
/// the keychain (finding M3) -- the non-JSON path already reports this via
/// `report_sync`, but `--json` skipped it entirely.
pub fn switch_json(outcome: &SwitchOutcome) -> serde_json::Value {
    serde_json::json!({
        "switched_to": outcome.switched_to.label,
        "uuid": outcome.switched_to.uuid,
        "already_active": outcome.already_active,
        "sync": sync_json(&outcome.sync),
    })
}

fn sync_json(sync: &SyncOutcome) -> serde_json::Value {
    match sync {
        SyncOutcome::Updated(meta) => {
            serde_json::json!({"outcome": "updated", "label": meta.label, "uuid": meta.uuid})
        }
        SyncOutcome::Captured(meta) => {
            serde_json::json!({"outcome": "captured", "label": meta.label, "uuid": meta.uuid})
        }
        SyncOutcome::LoggedOut => serde_json::json!({"outcome": "logged_out"}),
    }
}

/// The warning shown after a switch, or `None` when nothing is running.
///
/// Claude Code reads credentials at startup, so a session that is already
/// running keeps the previous account until it restarts. `pub` (like
/// `switch_json` and `resolve_add_failure`) specifically so this wording and
/// its zero-count case are directly testable without a keychain -- see
/// `tests/cli_run_test.rs`.
pub fn running_sessions_warning(count: usize) -> Option<String> {
    match count {
        0 => None,
        1 => Some(
            "1 running Claude Code session still uses the previous account. \
             Restart it to pick up the switch."
                .to_string(),
        ),
        n => Some(format!(
            "{n} running Claude Code sessions still use the previous account. \
             Restart them to pick up the switch."
        )),
    }
}

/// `pub`, like `cmd_add`, specifically so `tests/cli_run_test.rs` can drive
/// it end to end against a `MemoryStore` and a `TestDesktopPaths`. A
/// SUCCESSFUL switch through the compiled binary reaches `secrets.get()`,
/// which for that binary is the developer's real OS keychain (see
/// `tests/cli_test.rs`'s own header comment) -- this is the only way to
/// exercise the desktop half's wiring, including its failure handling,
/// without one.
///
/// `desktop` is `None` when no `DesktopPaths` could be discovered for this
/// platform or environment (see `run`'s `Command::Switch` arm) -- the
/// desktop half is then skipped entirely: it is an addition to the switch,
/// never a precondition for it.
pub fn cmd_switch<P: HostPaths + Copy, S: SecretStore, D: DesktopPaths>(
    sw: &Switcher<P, S>,
    name: &str,
    json: bool,
    probe: &impl ProcessProbe,
    desktop: Option<&D>,
) -> Result<()> {
    let outcome = sw.switch_to(name)?;

    // The desktop half is deliberately absent from --json entirely, rather
    // than folded into this payload: `switch_json` is a stable, tested
    // shape (`tests/cli_run_test.rs`), and the desktop outcome would need
    // its own considered field and variants rather than being bolted on
    // here. A script driving --json today gets exactly what it got before
    // this task; a human running a plain `byte switch` gets the new
    // stderr messages below.
    if json {
        output::data(&switch_json(&outcome).to_string());
        return Ok(());
    }

    let SwitchOutcome {
        switched_to,
        sync,
        already_active,
    } = outcome;

    report_sync(&sync);
    if already_active {
        output::info(&format!("{} is already active.", switched_to.label));
    } else {
        output::status(&format!("Switched to {}", switched_to.label));
        if let Some(msg) = running_sessions_warning(probe.running_claude_sessions()) {
            output::warn(&msg);
        }

        // Skipped for a no-op switch (the `already_active` branch above)
        // and skipped outright when no desktop paths are available at all.
        // `switch_desktop` also guards against a self-switch internally,
        // but there is no reason for the CLI to even attempt it for an
        // account that was already active.
        if let Some(desktop) = desktop {
            let outgoing = match &sync {
                SyncOutcome::Updated(meta) | SyncOutcome::Captured(meta) => {
                    Some(meta.uuid.as_str())
                }
                SyncOutcome::LoggedOut => None,
            };
            report_desktop_outcome(switch_desktop(
                sw.paths(),
                desktop,
                probe,
                outgoing,
                &switched_to.uuid,
            ));
        }
    }
    Ok(())
}

/// Report the desktop half's outcome without ever turning it into a command
/// failure. By the time this runs the Claude Code switch has already
/// committed, and undoing that is explicitly out of scope (see
/// `ops::desktop`'s module doc comment) -- this follows the same "never
/// fail an operation that already succeeded" rule `notify::send` and
/// `atomic::prune` already apply elsewhere in this codebase.
fn report_desktop_outcome(outcome: Result<DesktopOutcome>) {
    match outcome {
        Ok(DesktopOutcome::Switched) => output::status("Claude desktop app switched too."),
        Ok(DesktopOutcome::AppRunning) => output::warn(
            "Claude is running, so its desktop session was left on the previous account. \
             Quit Claude and run this switch again to move it too.",
        ),
        Ok(DesktopOutcome::NoProfileForIncoming) => output::warn(
            "No desktop session stored for this account yet, so Claude will open signed out. \
             Sign in there once and byte will remember it.",
        ),
        Ok(DesktopOutcome::NothingToDo) => {}
        Err(e) => output::warn(&format!(
            "the Claude Code switch succeeded, but its desktop app session could not be \
             switched: {e}"
        )),
    }
}

fn cmd_capture<P: HostPaths + Copy, S: SecretStore>(sw: &Switcher<P, S>, json: bool) -> Result<()> {
    let meta = sw.capture_current()?;
    if json {
        output::data(&serde_json::json!({"captured": meta.label}).to_string());
    } else {
        output::status(&format!("Saved {}", meta.label));
    }
    Ok(())
}

/// `pub` (like `resolve_add_failure`) specifically so its wiring -- does it
/// call `abort()` before reporting a `poll_once` failure, does the poll loop
/// terminate and reach the timeout path -- is directly testable against a
/// `MemoryStore` rather than only through the compiled binary, which would
/// require a real keychain (`capture_current` unconditionally calls
/// `secrets.put`). See `tests/cli_run_test.rs`.
pub fn cmd_add<P: HostPaths + Copy, S: SecretStore>(
    sw: &Switcher<P, S>,
    timeout: u64,
    yes: bool,
    json: bool,
) -> Result<()> {
    if !yes {
        // The gate has to precede begin(): begin() is what logs Claude Code
        // out, so confirming after it would be asking permission for
        // something already done. Same non-interactive rule as `byte
        // remove` -- a prompt would corrupt --json's machine-readable
        // stdout, and on any non-terminal stdin it would block forever
        // waiting for an answer nobody is there to give.
        if json || !std::io::stdin().is_terminal() {
            return Err(Error::ConfirmationRequired {
                action: "byte add".into(),
            });
        }

        if !output::confirm(&format!(
            "Add an account? This logs Claude Code out now and waits up to {timeout}s for a new login."
        )) {
            output::info("Aborted; nothing was changed.");
            return Ok(());
        }
    }

    // Taken here rather than in `run`'s dispatch arm, and only once the
    // confirmation above is settled. `output::confirm` blocks on stdin with
    // no timeout, so a lock held across it is pinned open for as long as
    // nobody answers -- and the tray now opens exactly that prompt in a
    // spawned terminal on a single click. An unanswered one would make
    // every tray menu account, and every `byte switch`/`capture`/`remove`/
    // `rename` in any terminal, fail with `Error::Busy` indefinitely, while
    // that error tells the user to "wait for it to finish" and nothing ever
    // finishes. Everything below IS bounded -- by `--timeout` -- so it is
    // legitimate to hold the lock across it.
    let _guard = MutationGuard::acquire(sw.paths())?;

    let session = AddSession::begin(sw)?;

    output::status("Claude Code is now logged out.");
    output::info("Run `claude` in another terminal and log in as the account you want to add.");
    output::info(&format!("Waiting up to {timeout} seconds..."));

    // Safe from overflow: clap's value_parser restricts `timeout` to
    // 1..=86_400 (see Command::Add in src/cli/mod.rs), nowhere near a
    // Duration that could push this addition past Instant's range.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
    while std::time::Instant::now() < deadline {
        match session.poll_once(sw) {
            Ok(Some(meta)) => {
                if json {
                    output::data(&serde_json::json!({"added": meta.label}).to_string());
                } else {
                    output::status(&format!("Added {}", meta.label));
                }
                return Ok(());
            }
            Ok(None) => {}
            // AddSession::begin has already cleared the live credentials by
            // this point, so this failure must not simply propagate: that
            // would leave the user logged out with no attempt to recover.
            // Mirror the timeout path below -- try to restore first, then
            // decide what to report.
            Err(e) => {
                let had_previous = session.previous().is_some();
                let restore_result = session.abort(sw);
                return resolve_add_failure(e, had_previous, restore_result);
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }

    let had_previous = session.previous().is_some();
    if had_previous {
        output::warn("Timed out. Restoring the previous account.");
    } else {
        output::warn(
            "Timed out. Nothing to restore -- you were already logged out before this `byte add` started.",
        );
    }
    let restore_result = session.abort(sw);
    resolve_add_failure(Error::LoginTimeout(timeout), had_previous, restore_result)
}

/// Decide what `cmd_add` reports after a failure, given the outcome of
/// already having tried to restore the previous account.
///
/// Neither failure may go unreported: if the restore succeeded, the
/// *original* cause (a `poll_once` error or a timeout) is still what the
/// user needs to see, so it becomes the returned `Err`. If the restore also
/// failed, the original cause is printed directly here (it would otherwise
/// vanish -- only one `Err` can be returned) and the restore failure -- the
/// more urgent of the two, since it means the account may not actually have
/// been put back -- becomes the returned `Err`, which `main` prints last.
///
/// `had_previous` distinguishes a real restoration from a no-op one:
/// `AddSession::abort` returns `Ok(())` both when it successfully restores a
/// previous account AND when there was never one to restore (a `byte add`
/// that started from an already-logged-out machine) -- without this,
/// finding M7, the success arm below would claim "Restored the previous
/// account" in the second case too, which is false twice over on a timeout
/// (`cmd_add`'s own pre-abort warning made the same claim).
///
/// Takes the restore attempt's `Result` rather than a `Switcher` and
/// performing it itself, so this decision is unit-testable on its own
/// without a `SecretStore` or any file/keychain I/O -- see
/// `tests/cli_run_test.rs`. `pub` (rather than the other `cmd_*` helpers'
/// default privacy) specifically so those tests can reach it.
pub fn resolve_add_failure(
    cause: Error,
    had_previous: bool,
    restore_result: Result<()>,
) -> Result<()> {
    match restore_result {
        Ok(()) if had_previous => {
            output::status("Restored the previous account.");
            Err(cause)
        }
        Ok(()) => {
            output::status(
                "Nothing to restore -- you were already logged out before this `byte add` started.",
            );
            Err(cause)
        }
        Err(abort_err) => {
            output::error(&cause.to_string());
            Err(abort_err)
        }
    }
}

/// `byte remove` is unlike every other write byte performs: the OS keychain
/// entry it deletes has no backup, so a removal is genuinely unrecoverable
/// except by re-authenticating with `byte add`. It must not proceed without
/// explicit confirmation.
fn cmd_remove<P: HostPaths + Copy, S: SecretStore>(
    sw: &Switcher<P, S>,
    name: &str,
    yes: bool,
    json: bool,
) -> Result<()> {
    if !yes {
        // Resolve first, so an unknown name still reports NoSuchAccount
        // rather than demanding confirmation for an account that was never
        // going to be removed anyway.
        let label = sw.load_accounts()?.resolve(name)?.label.clone();

        // --json is for scripts: a prompt would corrupt machine-readable
        // stdout, and would block forever on stdin nobody is watching, so
        // it requires --yes outright instead of prompting. The same applies
        // to any other non-interactive stdin (piped input, cron, CI) even
        // without --json -- prompting there would just hang.
        if json || !std::io::stdin().is_terminal() {
            return Err(Error::ConfirmationRequired {
                action: "byte remove".into(),
            });
        }

        if !output::confirm(&format!(
            "Remove '{label}'? Its stored credentials cannot be recovered afterward."
        )) {
            output::info("Aborted; nothing was removed.");
            return Ok(());
        }
    }

    let meta = manage::remove(sw, name)?;
    if json {
        output::data(&serde_json::json!({"removed": meta.label}).to_string());
    } else {
        output::status(&format!("Removed {}", meta.label));
    }
    Ok(())
}

fn cmd_rename<P: HostPaths + Copy, S: SecretStore>(
    sw: &Switcher<P, S>,
    name: &str,
    label: &str,
    json: bool,
) -> Result<()> {
    let meta = manage::rename(sw, name, label)?;
    if json {
        output::data(&serde_json::json!({"renamed": meta.label}).to_string());
    } else {
        output::status(&format!("Renamed to {}", meta.label));
    }
    Ok(())
}
