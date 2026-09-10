//! Executing CLI commands and rendering their results.

use std::io::IsTerminal as _;

use crate::autostart;
use crate::claude::detect::{ProcessProbe, SysinfoProbe};
use crate::cli::{AutostartAction, Cli, Command};
use crate::desktop::paths::{DesktopPaths, RealDesktopPaths};
use crate::desktop::swap::{IdentityRepair, Recovery, Repair};
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

    // Resolved BEFORE recovery, not just before the switch: a half-finished
    // swap includes a half-finished patch of the app's own `config.json`,
    // and recovery cannot finish that without knowing where the app lives.
    //
    // `RealDesktopPaths::discover` fails when neither `CLAUDE_DESKTOP_DIR`
    // nor `%APPDATA%` is set, which is the ordinary state of every
    // non-Windows machine -- this feature only understands the Windows
    // desktop app's layout (see `src/desktop/paths.rs`). `.ok()` degrades
    // that to "the desktop half is not attempted" rather than failing every
    // `byte` invocation outright: the desktop switch is an addition to the
    // Claude Code switch, never a precondition for it, and warning about a
    // directory that will never exist on those platforms on every single
    // invocation would be permanent noise, not a fixable problem. A genuine
    // failure to switch the desktop half once paths ARE available is a
    // different matter, and is reported -- see `cmd_switch`'s own handling,
    // and `recover_under_lock`'s `Deferred` case.
    //
    // Discovery alone is not enough, though: `%APPDATA%` is set for every
    // interactive Windows user, so it succeeds on every Windows machine
    // whether or not Claude Desktop is installed. See
    // `desktop_paths_if_installed`.
    let desktop = desktop_paths_if_installed(RealDesktopPaths::discover().ok());

    // Every command begins by repairing a desktop swap that an earlier run
    // left half-finished -- but only while it can take the mutation lock,
    // since an ungated repair would rename directories backwards underneath
    // a swap another byte process is running right now. See
    // `recover_under_lock`, which owns the whole rationale.
    recover_under_lock(&paths, desktop.as_ref());

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
    // mutation lock for themselves: they tolerate a concurrent write because
    // every file byte writes is replaced atomically, so there is never a
    // torn read to guard against. `recover_under_lock` above still takes it
    // momentarily on their behalf -- that is about never WRITING over
    // another process's in-flight swap, not about reading.
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

/// The desktop paths to use, or `None` when Claude Desktop is not installed.
///
/// `RealDesktopPaths::discover()` answers "where WOULD the app keep its
/// data", and on Windows that succeeds for every interactive user, installed
/// or not. Acting on that alone made byte manufacture an entire desktop half
/// for an application that is not there: a `<byte>/desktop/<uuid>/oauth.json`
/// of nulls, a `%APPDATA%\Claude\config.json` holding `{}` created by
/// `config::clear`'s own write, a `desktop_profile` record that makes `byte
/// list` report a session that does not exist, and a message on every single
/// switch. `docs/troubleshooting.md` promises the opposite -- silence, and a
/// `null` `desktop` field under `--json` -- and this is what makes that
/// true.
///
/// Recovery is gated the same way, and safely: a journal can only exist
/// because a swap ran, which needs these same paths, so a machine without
/// the app has nothing to repair. In the one degenerate case -- the app
/// uninstalled while a swap was interrupted -- `recover_if_interrupted`
/// still repairs the DIRECTORY half from the journal's own absolute paths
/// and reports `Deferred`, which is exactly the "only a command that can
/// find the app can finish this" state that variant exists for.
///
/// Takes the discovered value rather than discovering itself, so the gate is
/// testable against a `TestDesktopPaths` instead of only through
/// process-global environment variables -- the same split as
/// `RealPaths::resolve` and `RealDesktopPaths::resolve`.
pub fn desktop_paths_if_installed<D: DesktopPaths>(discovered: Option<D>) -> Option<D> {
    discovered.filter(DesktopPaths::is_installed)
}

/// Repair a desktop swap left behind by a crash -- but only while this
/// process can take the mutation lock.
///
/// A half-completed swap must be repaired by whatever byte command runs
/// next, not only by a retry of the `switch` that was interrupted -- so this
/// runs at the very start of `run`, before the dispatch (and before the
/// tray-vs-CLI fork just past it), rather than being folded into
/// `cmd_switch`. Safe to run on every command and every platform:
/// `desktop_journal_file` lives under byte's OWN config directory (via
/// `HostPaths`), never under the desktop app's real `%APPDATA%\Claude` -- a
/// machine that has never run a desktop swap has no such file, so this is a
/// cheap `Ok(None)` the rest of the time.
///
/// THE LOCK IS THE WHOLE POINT, so do not "simplify" it away: a journal is
/// on disk for the whole of every HEALTHY swap, not only after a crash.
/// `swap::execute` writes the complete journal before its first rename and
/// clears it only after its last, so a `byte list` (or `current`, or
/// `autostart`, none of which take the mutation lock for themselves) running
/// during an ordinary `byte switch` would read that live journal, see no
/// completed install, compute `Recovery::Reverse`, and rename the profile
/// directories BACKWARDS while the switching process is still renaming them
/// forwards -- then `clear_journal` the only record of a swap that is still
/// in flight. That is precisely the state `swap::execute` and
/// `switch_desktop` both document themselves as being protected from.
///
/// So each answer from `try_acquire` means something different:
///
/// - `Ok(Some(guard))` -- no other byte process is mutating, so any journal
///   on disk really is wreckage. Repair it, then release the lock before
///   returning: `run`'s own `Command::Switch`/`Capture`/... arms acquire it
///   again for themselves, and a guard still held here would deadlock them
///   against this process.
/// - `Ok(None)` -- another byte process holds the lock, so it is mid-mutation
///   and that journal is ITS in-flight swap. Skip silently: this is the
///   ordinary concurrent case, not an error, and warning about it would
///   report a fault on every perfectly healthy concurrent command.
/// - `Err(e)` -- warn and continue, exactly as a recovery failure does
///   below. A lock problem must not fail an unrelated command.
///
/// A recovery FAILURE (an unreadable journal, or one written by an
/// incompatible format version) is caught here rather than propagated, for
/// the same reason: this runs before every command, including read-only ones
/// that never touch the desktop machinery at all. Letting it fail `run()`
/// would turn one corrupt file into total unavailability of the whole CLI,
/// with no way to run byte again short of editing the journal by hand --
/// there is no weaker mode this reduces to. `switch` keeps its own, narrower
/// protection regardless of what happens here: `switch_desktop`'s own
/// precondition check still refuses to start a NEW swap over an unrepaired
/// journal, and that refusal is reported the same "catch and warn" way by
/// `report_desktop_outcome`, without failing the Claude Code switch that
/// already committed.
///
/// `pub` (like `switch_json` and `resolve_add_failure`) specifically so the
/// lock gate is directly testable against a `TestPaths` -- `run` itself
/// resolves `RealPaths`, i.e. the developer's real config directory.
pub fn recover_under_lock<P: HostPaths, D: DesktopPaths>(paths: &P, desktop: Option<&D>) {
    match MutationGuard::try_acquire(paths) {
        Ok(Some(_guard)) => match crate::desktop::swap::recover_if_interrupted(paths, desktop) {
            Ok(Some(repair)) => output::warn(&repair_message(&repair)),
            Ok(None) => {}
            Err(e) => output::warn(&format!(
                "an earlier desktop profile swap could not be repaired automatically, so this \
                 command is continuing without touching it: {e}"
            )),
        },
        Ok(None) => {}
        Err(e) => output::warn(&format!(
            "byte could not take its mutation lock to check for an interrupted desktop profile \
             swap, so this command is continuing without checking: {e}"
        )),
    }
}

/// What the user is told after an interrupted swap was repaired.
///
/// A swap is roughly sixteen directory renames AND a patch of the desktop
/// app's own `config.json`, and the two halves can finish independently, so
/// this must not promise more than actually happened -- "nothing further is
/// needed" is true only when both landed.
///
/// `pub` (like `running_sessions_warning` and `format_size`) specifically so
/// each wording is directly testable without a keychain, a real
/// `%APPDATA%\Claude`, or an interrupted swap to reproduce.
pub fn repair_message(repair: &Repair) -> String {
    let recovery = repair.recovery;
    match &repair.identity {
        IdentityRepair::Restored | IdentityRepair::NotNeeded => {
            format!("repaired an interrupted desktop profile swap ({recovery:?}).")
        }
        // The same unreadable file means opposite things in the two
        // directions, and `restore_identity` treats them that way: a
        // roll-forward CLEARS the app's account keys rather than leave the
        // outgoing account's over the incoming account's cookies, so the app
        // really does open signed out. A reversal writes nothing at all --
        // `config.json` still describes the very session the reversal put
        // back -- so predicting a signed-out app there would send the user
        // to sign in over a session that works.
        IdentityRepair::Unreadable(path) if recovery == Recovery::Reverse => format!(
            "repaired an interrupted desktop profile swap ({recovery:?}). The saved sign-in at \
             {} could not be read, but nothing needed it: Claude Desktop's own config already \
             describes the session that was put back, so it will open as that account. byte \
             will capture a fresh saved sign-in the next time you switch away from it.",
            path.display()
        ),
        IdentityRepair::Unreadable(path) => format!(
            "repaired an interrupted desktop profile swap ({recovery:?}), but the saved \
             sign-in at {} could not be read, so Claude Desktop may open signed out. Sign in \
             there once and byte will capture it again.",
            path.display()
        ),
        IdentityRepair::Deferred => format!(
            "repaired the directory half of an interrupted desktop profile swap \
             ({recovery:?}), but Claude Desktop's own data directory could not be located, so \
             which account it is signed in as was left alone. The swap's journal has been kept \
             so a later byte command can finish it."
        ),
        IdentityRepair::AccountIdentifierInvalid(uuid) => format!(
            "repaired the directory half of an interrupted desktop profile swap \
             ({recovery:?}), but its journal names an account identifier ('{uuid}') byte will \
             not use to look up a stored profile, so which account Claude Desktop is signed in \
             as was left alone. The swap's journal has been kept -- this needs to be inspected \
             by hand rather than repaired automatically."
        ),
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
///
/// Includes `desktop` for the same reason: the desktop half runs under
/// `--json` too, and a script driving it needs a machine-readable answer to
/// "did Claude Desktop follow?" -- the human path's stderr messages are not
/// one, and are not emitted under `--json` at all (except the failure
/// warning, whose detail has nowhere else to go).
pub fn switch_json(
    outcome: &SwitchOutcome,
    desktop: Option<&Result<DesktopOutcome>>,
) -> serde_json::Value {
    serde_json::json!({
        "switched_to": outcome.switched_to.label,
        "uuid": outcome.switched_to.uuid,
        "already_active": outcome.already_active,
        "sync": sync_json(&outcome.sync),
        "desktop": desktop_json(desktop),
    })
}

/// The `desktop` field: one lowercase name per `DesktopOutcome` variant.
///
/// `null` means the desktop half was not attempted at all -- an
/// `already_active` no-op switch, or a platform where no `DesktopPaths`
/// could be discovered. The key is always present, never omitted, for the
/// same reason `listing_json` always carries `desktop_profile`: "not
/// attempted" and "this build has never heard of the field" are different
/// facts and a script must be able to tell them apart.
///
/// A failure is reported as the bare string `"failed"` rather than the
/// error's own text: `Error`'s `Display` is written for humans and its
/// wording is not a contract a script may match on. The detail still
/// reaches the user, on stderr, via `desktop_failure_message`.
fn desktop_json(outcome: Option<&Result<DesktopOutcome>>) -> serde_json::Value {
    let name = match outcome {
        None => return serde_json::Value::Null,
        Some(Ok(DesktopOutcome::Switched)) => "switched",
        Some(Ok(DesktopOutcome::AppRunning)) => "app_running",
        Some(Ok(DesktopOutcome::NoProfileForIncoming)) => "no_profile_for_incoming",
        Some(Ok(DesktopOutcome::IdentityMismatch)) => "identity_mismatch",
        Some(Ok(DesktopOutcome::IncomingIdentifierInvalid)) => "incoming_identifier_invalid",
        Some(Ok(DesktopOutcome::SwitchedWithoutIdentity)) => "switched_without_identity",
        Some(Ok(DesktopOutcome::NothingToDo)) => "nothing_to_do",
        Some(Err(_)) => "failed",
    };
    serde_json::Value::String(name.to_string())
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

    // --json runs the desktop half too. It reports the result through the
    // payload's `desktop` field instead of the human path's stderr messages
    // below; skipping it entirely -- as this did before -- left a script
    // driving `byte switch --json` with Claude Desktop still signed in as
    // the previous account and nothing in its output saying so.
    if json {
        let desktop_outcome = desktop_half(sw, probe, desktop, &outcome);

        // The only prose --json emits, and it goes to stderr like every
        // other message: stdout carries the payload alone. An error's detail
        // has nowhere else to live -- the field can only say "failed".
        if let Some(Err(e)) = &desktop_outcome {
            output::warn(&desktop_failure_message(e));
        }

        output::data(&switch_json(&outcome, desktop_outcome.as_ref()).to_string());
        return Ok(());
    }

    report_sync(&outcome.sync);
    if outcome.already_active {
        output::info(&format!("{} is already active.", outcome.switched_to.label));
    } else {
        output::status(&format!("Switched to {}", outcome.switched_to.label));
        if let Some(msg) = running_sessions_warning(probe.running_claude_sessions()) {
            output::warn(&msg);
        }

        // Deliberately here, after the status line above, rather than
        // hoisted alongside the --json call: the desktop half moves
        // directory trees, so running it first would leave the user staring
        // at a silent terminal until it finished before learning that the
        // Claude Code switch they asked for had already succeeded.
        if let Some(result) = desktop_half(sw, probe, desktop, &outcome) {
            let claude_code_was_logged_out = matches!(outcome.sync, SyncOutcome::LoggedOut);
            report_desktop_outcome(&result, claude_code_was_logged_out);
        }
    }
    Ok(())
}

/// Run the desktop half of a switch, or `None` when it is not attempted.
///
/// Both output modes go through this, so `--json` and a plain `byte switch`
/// can never disagree about whether the desktop app follows -- only about
/// how the answer is reported.
///
/// Not attempted for a no-op switch (`already_active`), and not attempted at
/// all when no desktop paths are available for this platform or environment
/// (see `run`'s `Command::Switch` arm). `switch_desktop` also guards against
/// a self-switch internally, but there is no reason for the CLI to even
/// attempt it for an account that was already active.
fn desktop_half<P: HostPaths + Copy, S: SecretStore, D: DesktopPaths>(
    sw: &Switcher<P, S>,
    probe: &impl ProcessProbe,
    desktop: Option<&D>,
    outcome: &SwitchOutcome,
) -> Option<Result<DesktopOutcome>> {
    if outcome.already_active {
        return None;
    }
    let desktop = desktop?;

    let outgoing = match &outcome.sync {
        SyncOutcome::Updated(meta) | SyncOutcome::Captured(meta) => Some(meta.uuid.as_str()),
        SyncOutcome::LoggedOut => None,
    };
    Some(switch_desktop(
        sw.paths(),
        desktop,
        probe,
        outgoing,
        &outcome.switched_to.uuid,
    ))
}

/// What the user is told when the desktop half fails outright.
///
/// Shared by both output modes: `--json` answers the "did it work" question
/// in its payload, but the reason still has to reach the user somewhere, and
/// stdout is reserved for the payload.
fn desktop_failure_message(e: &Error) -> String {
    format!(
        "the Claude Code switch succeeded, but its desktop app session could not be switched: {e}"
    )
}

/// The advice shown for `DesktopOutcome::IdentityMismatch`, which is reached
/// for several different underlying reasons (see `park_target`'s doc comment
/// and `docs/troubleshooting.md`) that fall into two families: the desktop
/// app disagrees with an account Claude Code already knows about, or Claude
/// Code itself had no account to compare against when this switch started
/// (`outgoing: None` in `ops::desktop::switch_desktop`'s terms).
///
/// `claude_code_was_logged_out` distinguishes them because the commonest way
/// into the second family -- a signed-out desktop app, over a live
/// directory that still holds a session, while Claude Code is *also* logged
/// out -- makes the general wording's first instruction, "sign out of
/// Claude Desktop", nonsensical: there is no session in the app to sign out
/// of. Logging in to Claude Code first is the more direct fix for that
/// family: it gives byte an account to attribute the *live* session to
/// without the user having to separately decide, and act on, which account
/// to sign into inside Claude Desktop itself. That route, before this, was
/// stated only in `docs/troubleshooting.md` and never shown here.
///
/// `pub`, like `repair_message` and `remove_prompt`, specifically so both
/// wordings are directly testable without a keychain or a real desktop app.
pub fn identity_mismatch_message(claude_code_was_logged_out: bool) -> String {
    let fix = if claude_code_was_logged_out {
        "Claude Code was logged out when this switch started, so byte had no account to \
         attribute that session to. Log in to Claude Code first, then switch again -- byte \
         will capture the desktop session once it knows whose it is."
    } else {
        "Otherwise, sign out of Claude Desktop and sign in again there as the account you \
         want; byte will capture that session the next time you switch away from it."
    };
    format!(
        "Claude's desktop app is not signed in as the account byte expected there, so its \
         session was left alone rather than filed under the wrong account -- nothing \
         changed. If it's already showing the account you just switched to, there is \
         nothing more to do. {fix}"
    )
}

/// Report the desktop half's outcome without ever turning it into a command
/// failure. By the time this runs the Claude Code switch has already
/// committed, and undoing that is explicitly out of scope (see
/// `ops::desktop`'s module doc comment) -- this follows the same "never
/// fail an operation that already succeeded" rule `notify::send` and
/// `atomic::prune` already apply elsewhere in this codebase.
///
/// `claude_code_was_logged_out` is threaded through only for
/// `identity_mismatch_message` -- see its own doc comment for why that one
/// case branches on it.
fn report_desktop_outcome(outcome: &Result<DesktopOutcome>, claude_code_was_logged_out: bool) {
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
        // Deliberately conditional, not a blanket "sign out" instruction:
        // the most common way to reach this is a tray click that switched
        // Claude Code without touching the desktop app (see the tray's
        // `Action::SwitchTo`), followed by a CLI `byte switch` back to the
        // account the desktop app was on the whole time. In that case the
        // app is ALREADY correct, and the old unconditional wording told
        // the user to destroy a working session for no benefit. Telling
        // them to retry the same switch is no better: by the time this
        // message prints, the Claude Code half has already committed, so a
        // repeat of the same `byte switch` is a self-switch and never
        // re-attempts the desktop half at all (see `desktop_half`'s
        // `already_active` guard) -- so the fix, when one is actually
        // needed, has to happen by hand, directly in the app.
        Ok(DesktopOutcome::IdentityMismatch) => {
            output::warn(&identity_mismatch_message(claude_code_was_logged_out));
        }
        // `incoming` (the account Claude Code just switched TO) has an
        // identifier byte cannot use to locate a stored profile at all --
        // see `ops::desktop::switch_desktop`'s own check. This is not the
        // ordinary "nothing captured yet" state `NoProfileForIncoming`
        // reports: it means the account's own identity data is malformed,
        // which no amount of switching or signing in again will fix on its
        // own.
        Ok(DesktopOutcome::IncomingIdentifierInvalid) => output::warn(
            "The account just switched to has an identifier byte cannot use to locate a \
             stored Claude Desktop profile, so its desktop session was left untouched -- \
             nothing changed there. This usually means Claude Code's own record of the \
             account is malformed; removing and re-adding it with `byte remove` and `byte \
             add` will give it a fresh one.",
        ),
        // The profile moved, but the account's saved sign-in could not be
        // read, so the app will open signed out. `switch_desktop` has
        // already warned, naming the file and the parse failure -- facts
        // only it holds. This adds what to do about it, and deliberately
        // does not repeat them.
        Ok(DesktopOutcome::SwitchedWithoutIdentity) => output::warn(
            "Claude's desktop app has this account's session back, but will open signed out. \
             Sign in there once and byte will remember it again.",
        ),
        Ok(DesktopOutcome::NothingToDo) => {}
        Err(e) => output::warn(&desktop_failure_message(e)),
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

/// What `byte remove` asks before it does anything.
///
/// The desktop session is named explicitly when there is one, rather than
/// left inside "its stored credentials": that directory is a live claude.ai
/// session and a saved sign-in as plain files, it is the one credential byte
/// keeps outside the OS credential store (SECURITY.md, location 5), and it
/// is far and away the most consequential thing this command destroys. A
/// prompt that only implies it is asking for consent the user has not
/// knowingly given.
///
/// `pub` (like `running_sessions_warning` and `repair_message`) specifically
/// so both wordings are directly testable without a keychain or a terminal.
pub fn remove_prompt(label: &str, has_desktop_session: bool) -> String {
    if has_desktop_session {
        format!(
            "Remove '{label}'? Its stored credentials AND its saved Claude Desktop session -- \
             that account's signed-in claude.ai session on this machine -- are both deleted, \
             and cannot be recovered afterward."
        )
    } else {
        format!("Remove '{label}'? Its stored credentials cannot be recovered afterward.")
    }
}

/// `byte remove` is unlike every other write byte performs: the OS keychain
/// entry it deletes has no backup, and the parked desktop session it deletes
/// has none either, so a removal is genuinely unrecoverable except by
/// re-authenticating with `byte add` and signing in to Claude Desktop again.
/// It must not proceed without explicit confirmation.
fn cmd_remove<P: HostPaths + Copy, S: SecretStore>(
    sw: &Switcher<P, S>,
    name: &str,
    yes: bool,
    json: bool,
) -> Result<()> {
    if !yes {
        // Resolve first, so an unknown name still reports NoSuchAccount
        // rather than demanding confirmation for an account that was never
        // going to be removed anyway. The uuid comes along because the
        // prompt below has to say whether there is a desktop session to
        // delete, and that is keyed by uuid, not by the name typed.
        let accounts = sw.load_accounts()?;
        let account = accounts.resolve(name)?;
        let (label, uuid) = (account.label.clone(), account.uuid.clone());

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

        let has_desktop = manage::has_desktop_profile(sw.paths(), &uuid);
        if !output::confirm(&remove_prompt(&label, has_desktop)) {
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
