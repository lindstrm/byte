//! Answering "is Claude Code running right now?"
//!
//! Claude Code reads its credentials once at startup, so a switch does not
//! affect a session that is already running. byte warns about that -- but a
//! warning shown when nothing is running is noise, so the count has to be
//! real.
//!
//! This is deliberately a heuristic and is never allowed to fail an
//! operation: a probe that errors reports zero.

use std::path::Path;

/// Something that can count running Claude Code sessions and check app state.
pub trait ProcessProbe: Send + Sync {
    fn running_claude_sessions(&self) -> usize;

    /// Whether the Claude desktop app is running.
    ///
    /// The inverse reading of `is_claude_desktop_app`, which exists to
    /// EXCLUDE the app from session counts. A profile swap needs the app
    /// closed -- Chromium corrupts profile state otherwise -- so this gates
    /// the desktop half of a switch.
    fn desktop_app_running(&self) -> bool;
}

/// Decide whether one process is a Claude Code session.
///
/// Two install shapes exist: a native binary named `claude`/`claude.exe`, and
/// an npm install that runs as `node` with the CLI entry point in its
/// arguments. Exposed separately from the probe so it can be tested without a
/// live process table.
///
/// On Windows, the unrelated Anthropic Claude desktop app also ships an
/// executable literally named `claude.exe`, and its Chromium/Electron helper
/// processes (renderer, gpu-process, utility, crashpad-handler) share that
/// name too -- a real process-table check found 11 of them alongside genuine
/// CLI sessions. Those helpers are always tagged with a `--type=` flag, which
/// a real Claude Code invocation never carries, so that flag is used to tell
/// them apart here.
///
/// That leaves one shape this function cannot resolve on its own: the
/// desktop app's own bare top-level process, which carries no `--type=` (or
/// any other) argv and so looks identical to a bare CLI invocation from
/// `process_name` and `argv` alone. That is *not* an unavoidable false
/// positive overall -- it only needs information this pure function doesn't
/// have. `SysinfoProbe::running_claude_sessions` closes it by also checking
/// the process's executable path (`is_claude_desktop_app`), since
/// distinguishing by install location needs the full path, which is
/// deliberately kept out of this function's signature -- a later task
/// depends on `(&str, &[String]) -> bool` staying as it is.
pub fn looks_like_claude_code(process_name: &str, argv: &[String]) -> bool {
    let name = process_name.to_ascii_lowercase();
    let stem = name.strip_suffix(".exe").unwrap_or(&name);

    if stem == "claude" {
        return !argv
            .iter()
            .any(|a| a.to_ascii_lowercase().starts_with("--type="));
    }

    if stem == "node" || stem == "node.js" {
        return argv.iter().any(|a| {
            let a = a.replace('\\', "/").to_ascii_lowercase();
            a.contains("@anthropic-ai/claude-code") || a.ends_with("/claude-code/cli.js")
        });
    }

    false
}

/// True when `exe` is the Anthropic Claude desktop app's executable rather
/// than a genuine Claude Code CLI binary.
///
/// On Windows the desktop app installs under
/// `%LOCALAPPDATA%\AnthropicClaude\...\claude.exe` (both the Squirrel shim at
/// the top of that directory and the versioned `app-<version>\claude.exe` it
/// launches) -- a distinct product that happens to ship a binary literally
/// named `claude.exe` (see `looks_like_claude_code`'s doc comment). Its
/// Electron helper subprocesses are already excluded there by their
/// `--type=` flag; this catches the one shape that check can't reach: the
/// app's own bare top-level process, which carries no `--type=` or any other
/// distinguishing argv.
///
/// `exe()` is not always available: it can be `None` (permission or
/// namespace restrictions can hide another process's path) and on Linux a
/// read failure yields an empty path rather than `None`. Both are treated as
/// "unknown," which deliberately resolves to *not* the desktop app --
/// excluding a process just because its path could not be read would
/// silently suppress a real warning, which is the worse failure mode here
/// (precision matters, but not by turning missing data into a false "this is
/// fine").
///
/// Exposed separately from the probe, like `looks_like_claude_code`, so it
/// can be tested without a live process table.
pub fn is_claude_desktop_app(exe: Option<&Path>) -> bool {
    let Some(path) = exe else {
        return false;
    };

    let path = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();

    // Windows lays the app out under `.../AnthropicClaude/`. macOS ships
    // it as `Claude.app/Contents/MacOS/Claude`, whose executable stem is a
    // plain `claude` and whose argv carries no `--type=` marker (the
    // Electron helpers are separate `Claude Helper*` bundles, already
    // excluded by name). Without the second arm, every macOS user with
    // Claude Desktop open sees a phantom "1 running Claude Code session
    // still uses the previous account" after every single switch -- and
    // macOS is a first-class tray platform here, so a Windows-only
    // exclusion closes this gap on only one of the two supported systems.
    path.contains("/anthropicclaude/") || path.contains("/claude.app/contents/macos/")
}

/// Whether one entry in the process table is a Claude Code session.
///
/// The composition of the two predicates above, extracted so it can be
/// tested: `SysinfoProbe` needs a live process table and therefore never
/// is. Dropping the `!is_claude_desktop_app(..)` half re-introduces
/// exactly the bug commit `fde66d6` fixed, and every test of the two
/// halves individually still passes with it gone -- so the joining `&&`
/// needs a test of its own.
pub fn is_claude_code_process(name: &str, argv: &[String], exe: Option<&Path>) -> bool {
    looks_like_claude_code(name, argv) && !is_claude_desktop_app(exe)
}

/// Counts sessions from the real process table.
#[derive(Debug, Default)]
pub struct SysinfoProbe;

impl SysinfoProbe {
    pub fn new() -> Self {
        Self
    }
}

impl ProcessProbe for SysinfoProbe {
    fn running_claude_sessions(&self) -> usize {
        use sysinfo::{ProcessRefreshKind, RefreshKind, System, UpdateKind};

        // Only the two fields the classifier actually reads. `everything()`
        // additionally collects memory, disk I/O, users and environment for
        // every process on the machine -- and this runs on the tray's winit
        // event-loop thread after each switch, so the tray stops responding
        // for however long that takes.
        let system = System::new_with_specifics(
            RefreshKind::nothing().with_processes(
                ProcessRefreshKind::nothing()
                    .with_cmd(UpdateKind::Always)
                    .with_exe(UpdateKind::Always),
            ),
        );

        system
            .processes()
            .values()
            .filter(|p| {
                let name = p.name().to_string_lossy();
                let argv: Vec<String> = p
                    .cmd()
                    .iter()
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect();
                is_claude_code_process(&name, &argv, p.exe())
            })
            .count()
    }

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
}

/// A probe that reports a fixed count and desktop app state, for tests.
#[derive(Debug, Clone, Copy)]
pub struct FakeProbe {
    count: usize,
    desktop: bool,
}

impl FakeProbe {
    pub fn with_count(count: usize) -> Self {
        Self {
            count,
            desktop: false,
        }
    }

    pub fn with_desktop(count: usize, desktop: bool) -> Self {
        Self { count, desktop }
    }
}

impl ProcessProbe for FakeProbe {
    fn running_claude_sessions(&self) -> usize {
        self.count
    }

    fn desktop_app_running(&self) -> bool {
        self.desktop
    }
}
