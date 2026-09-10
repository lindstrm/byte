use byte::claude::detect::{
    FakeProbe, ProcessProbe, is_claude_code_process, is_claude_desktop_app, looks_like_claude_code,
};
use std::path::Path;

#[test]
fn a_fake_probe_reports_what_it_was_given() {
    assert_eq!(FakeProbe::with_count(0).running_claude_sessions(), 0);
    assert_eq!(FakeProbe::with_count(3).running_claude_sessions(), 3);
}

#[test]
fn a_native_claude_binary_is_recognised() {
    assert!(looks_like_claude_code("claude.exe", &["claude.exe".into()]));
    assert!(looks_like_claude_code("claude", &["claude".into()]));
}

#[test]
fn a_node_process_running_the_claude_cli_is_recognised() {
    // npm installs appear as node with the CLI entry point in argv.
    assert!(looks_like_claude_code(
        "node.exe",
        &[
            "node.exe".into(),
            "C:\\Users\\x\\AppData\\Roaming\\npm\\node_modules\\@anthropic-ai\\claude-code\\cli.js"
                .into(),
        ]
    ));
}

#[test]
fn an_unrelated_node_process_is_not_recognised() {
    assert!(!looks_like_claude_code(
        "node.exe",
        &["node.exe".into(), "server.js".into()]
    ));
}

#[test]
fn an_unrelated_binary_whose_name_merely_contains_claude_is_not_recognised() {
    // This name is hypothetical -- the real Anthropic Claude desktop app is
    // not called "claude-desktop". It ships as `claude.exe` on Windows,
    // sharing the exact CLI binary name, and is excluded by the probe's
    // executable-path check (`is_claude_desktop_app`) rather than by this
    // classifier. This case still earns its place: it guards against a
    // classifier that matched by loose substring (e.g. `name.contains("claude")`)
    // rather than the exact stem this function actually compares.
    assert!(!looks_like_claude_code(
        "claude-desktop.exe",
        &["claude-desktop.exe".into()]
    ));
}

#[test]
fn byte_itself_is_never_counted() {
    // The tray is a long-running process; counting it would make every switch
    // warn about a session that is byte's own.
    assert!(!looks_like_claude_code("byte.exe", &["byte.exe".into()]));
}

#[test]
fn an_electron_helper_process_sharing_the_claude_binary_name_is_not_recognised() {
    // On Windows the Anthropic Claude desktop app -- a different product --
    // also ships an executable literally named `claude.exe`. Its internal
    // Chromium/Electron helper processes (renderer, gpu-process, utility,
    // crashpad-handler) are tagged with a `--type=` flag; a real Claude Code
    // CLI invocation never carries one. Observed on a live process table
    // where this shape accounted for 11 of 22 raw matches.
    assert!(!looks_like_claude_code(
        "claude.exe",
        &["claude.exe".into(), "--type=renderer".into()]
    ));
    assert!(!looks_like_claude_code(
        "claude.exe",
        &["claude.exe".into(), "--type=crashpad-handler".into()]
    ));
    assert!(!looks_like_claude_code(
        "claude.exe",
        &["claude.exe".into(), "--type=gpu-process".into()]
    ));
}

#[test]
fn a_claude_binary_with_ordinary_cli_flags_is_still_recognised() {
    // The exclusion above is keyed on the `--type=` Electron/Chromium marker
    // specifically, not on "carries more than one argv element". A real
    // Claude Code invocation routinely carries several ordinary flags and
    // must still classify as positive -- a classifier written as
    // `argv.len() == 1` would wrongly exclude it while still passing every
    // other case in this file.
    assert!(looks_like_claude_code(
        "claude.exe",
        &[
            "claude.exe".into(),
            "--print".into(),
            "--output-format=json".into()
        ]
    ));
}

#[test]
fn the_desktop_apps_executable_path_is_recognised() {
    // Both the Squirrel shim (directly under `AnthropicClaude\`) and the
    // versioned app directory it launches (`AnthropicClaude\app-<version>\`)
    // identify the desktop app -- see `is_claude_desktop_app`'s doc comment.
    assert!(is_claude_desktop_app(Some(Path::new(
        "C:\\Users\\x\\AppData\\Local\\AnthropicClaude\\app-1.46388.4\\claude.exe"
    ))));
    assert!(is_claude_desktop_app(Some(Path::new(
        "C:\\Users\\x\\AppData\\Local\\AnthropicClaude\\claude.exe"
    ))));
}

#[test]
fn a_genuine_cli_installs_executable_path_is_not_the_desktop_app() {
    assert!(!is_claude_desktop_app(Some(Path::new(
        "C:\\Users\\x\\AppData\\Roaming\\Claude\\claude-code\\2.1.260\\claude.exe"
    ))));
    assert!(!is_claude_desktop_app(Some(Path::new(
        "C:\\Users\\x\\.local\\bin\\claude.exe"
    ))));
}

#[test]
fn an_unavailable_executable_path_is_not_treated_as_the_desktop_app() {
    // exe() returns None on some platforms/processes (permission or
    // namespace restrictions can hide another process's path). Treating
    // "unknown" as "exclude it" would silently suppress a real warning
    // instead of a false one -- the worse failure mode -- so unknown must
    // resolve to "not the desktop app" and stay counted.
    assert!(!is_claude_desktop_app(None));
}

#[test]
fn an_empty_executable_path_is_not_treated_as_the_desktop_app() {
    // On Linux, exe() can return an empty path rather than None when
    // /proc/<pid>/exe could not be read. The same "unknown stays counted"
    // handling applies.
    assert!(!is_claude_desktop_app(Some(Path::new(""))));
}

#[test]
fn the_macos_desktop_app_executable_path_is_recognised() {
    // macOS ships the desktop app as a .app bundle whose main executable is
    // a plain `Claude` carrying no `--type=` argv marker, so the name/argv
    // predicate cannot tell it apart from the CLI -- only the path can.
    assert!(is_claude_desktop_app(Some(Path::new(
        "/Applications/Claude.app/Contents/MacOS/Claude"
    ))));
    // Case-insensitively, since the check lowercases first.
    assert!(is_claude_desktop_app(Some(Path::new(
        "/Users/x/Applications/claude.app/contents/macos/claude"
    ))));
}

#[test]
fn the_composed_classifier_counts_the_cli_and_excludes_the_desktop_app() {
    // The joining `&&` itself, which `SysinfoProbe` can never be tested
    // through. All three processes below are named some case of `claude`
    // with a single-element argv -- indistinguishable to
    // `looks_like_claude_code`, which answers *yes* to all of them. Only
    // the executable path separates them, so an implementation that drops
    // the `!is_claude_desktop_app(..)` half (the bug fde66d6 fixed) passes
    // every other test in this file and fails this one.
    let argv = vec!["claude".to_string()];

    assert!(
        is_claude_code_process("claude", &argv, Some(Path::new("/usr/local/bin/claude"))),
        "a real CLI session must count"
    );
    assert!(
        !is_claude_code_process(
            "Claude",
            &argv,
            Some(Path::new("/Applications/Claude.app/Contents/MacOS/Claude"))
        ),
        "the macOS desktop app must not count as a Claude Code session"
    );
    assert!(
        !is_claude_code_process(
            "claude.exe",
            &["claude.exe".to_string()],
            Some(Path::new(
                r"C:\Users\x\AppData\Local\AnthropicClaude\claude.exe"
            ))
        ),
        "the Windows desktop app must not count as a Claude Code session"
    );
}

#[test]
fn a_fake_probe_reports_the_desktop_app_state_it_was_given() {
    assert!(!FakeProbe::with_count(3).desktop_app_running());
    assert!(FakeProbe::with_desktop(0, true).desktop_app_running());
    assert!(!FakeProbe::with_desktop(2, false).desktop_app_running());
    // The two readings are independent: sessions running does not imply the
    // app is, and vice versa.
    assert_eq!(FakeProbe::with_desktop(2, true).running_claude_sessions(), 2);
}
