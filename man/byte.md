% BYTE(1) | User Commands

# NAME

byte - switch between Claude accounts

# SYNOPSIS

**byte** \[**--json**] \[*COMMAND*]

# DESCRIPTION

**byte** switches which Claude account Claude Code is authenticated as. It
stores each account's OAuth credentials in the operating system's credential
store and swaps them into Claude Code's configuration on demand.

Only the authentication identity is swapped. Settings, project history,
plugins, and MCP server tokens are shared across all accounts.

On Windows, **switch** also switches the Claude *desktop* app's own session
immediately after the Claude Code half completes — see **switch** below and
**ENVIRONMENT**'s **CLAUDE_DESKTOP_DIR**.

Run with no *COMMAND* to start byte's tray icon, which shows the stored
accounts, marks the active one, and switches on a click. The tray is
available on **Windows and macOS only**; on other platforms, running **byte**
with no arguments prints a message directing you to the commands below
instead of starting anything.

# COMMANDS

**list**
: List stored accounts. The active account is marked with an asterisk.

**current**
: Print the active account's label.

**switch** *NAME*
: Switch to a stored account. *NAME* matches a label, an email address, or an
  account UUID prefix.

  On Windows, this also switches the Claude desktop app's session right
  after the Claude Code half completes: the outgoing account's live
  desktop session is parked into its own store, the incoming account's
  stored session is installed if one has been captured, and `config.json`'s
  account-identifying keys are patched to match. An account with no stored
  desktop session is left signed out in the app rather than left on the
  previous account; signing in there once causes the next switch away from
  it to capture a session for it. The desktop half is skipped entirely,
  and reported rather than treated as a failure, whenever Claude is
  currently running (moving its files under a live process would corrupt
  them) or when no desktop app directory can be found for this platform or
  environment — which is every non-Windows platform today. A problem in the
  desktop half never fails the command: the Claude Code half above has
  already committed by the time the desktop half runs. See
  **CLAUDE_DESKTOP_DIR** below and
  [Troubleshooting](../docs/troubleshooting.md) for the exact messages this
  produces, and
  [Troubleshooting's desktop-switching section](../docs/troubleshooting.md#desktop-app-switching-is-unverified)
  for what has and has not been verified about it.

**capture**
: Save the currently logged-in account.

**add** \[**--timeout** *SECONDS*] \[**--yes**]
: Log Claude Code out, then wait for you to log in as a different account and
  save it automatically. Defaults to 300 seconds. Prompts for confirmation
  *before* logging out when standard input is a terminal and **--json** is not
  set; otherwise **--yes** is required. The tray's **Add account...** item
  opens a terminal running this command, and relies on that prompt.

**remove** *NAME* \[**--yes**]
: Forget a stored account, deleting both its metadata and its stored
  credentials. Unlike every other write byte performs, this has no backup and
  cannot be undone. Prompts for confirmation when standard input is a
  terminal and **--json** is not set; otherwise **--yes** is required.

**rename** *NAME* *LABEL*
: Change an account's display label.

**autostart** *ACTION*
: Manage whether byte's tray starts automatically at login. *ACTION* is one
  of **enable**, **disable**, or **status**. Opt-in: byte never registers
  itself at login unless you run **autostart enable**.

# OPTIONS

**--json**
: Emit machine-readable JSON on standard output.

# ENVIRONMENT

**CLAUDE_CONFIG_DIR**
: Directory holding `.claude.json` and `.credentials.json`.

**BYTE_CONFIG_DIR**
: byte's own configuration directory.

**CLAUDE_DESKTOP_DIR**
: Directory holding the Claude desktop app's own data (normally
  `%APPDATA%\Claude` on Windows). Read only by the desktop half of
  **switch**; setting it on a non-Windows platform does not make that half
  supported there.

# FILES

*accounts.json*
: Account metadata, in byte's configuration directory.

*backups/*
: Timestamped copies made before every write. The ten most recent per file are
  kept.

*desktop/*
: Parked Claude desktop app sessions, one subdirectory per account named by
  its UUID, plus a journal file while a swap is in progress (see NOTES).
  Windows only. **Contains live session credentials — cookies and an OAuth
  cache — as ordinary files, not entries in the OS credential store**: this
  is the one place byte keeps a secret outside it, because those
  credentials do not fit in one. Treat it with at least the care given to
  *backups/* above.

*mutation.lock*
: Advisory lock held for the duration of a **switch**, **capture**, **add**,
  **remove**, or **rename**. See NOTES.

*tray.lock*
: Advisory lock held for a running tray's entire lifetime. See NOTES.

# EXIT STATUS

**0**
: Success.

**1**
: An operation failed — for example, an unknown account name, a locked or
  unavailable keychain, or a Claude Code file that could not be parsed. The
  cause is printed to standard error.

**2**
: A usage error: an unrecognized command or flag, or a missing required
  argument.

# NOTES

Claude Code reads its credentials at startup, so sessions that are already
running keep the previous account until they are restarted. When a switch
completes, byte reports this only if it actually detects a running Claude
Code session; with none running, it says nothing.

**switch**, **capture**, **add**, **remove**, and **rename** each take an
advisory lock (`mutation.lock` in byte's configuration directory) for the
duration of the command, so a CLI invocation and a tray-driven switch can
never interleave their writes to the same files. It is short-lived for every
one of them except **add**, which takes it only *after* its confirmation
prompt is answered and then holds it while waiting for the new login — up to
**--timeout** seconds. **list**, **current**,
and **autostart** do not take this lock; every file byte writes is replaced
atomically, so a concurrent read is always safe. If another byte process
already holds the lock, the command fails immediately with "another byte
process is currently changing accounts" rather than waiting or retrying;
simply run it again once the other process finishes.

The tray holds a second, longer-lived lock (`tray.lock`) for as long as it
runs, so a second **byte** started with no arguments while one is already
running fails with "byte is already running" instead of opening a duplicate
icon. On a platform other than Windows or macOS, **byte** with no arguments
fails with "the tray is only available on Windows and macOS" and suggests
the CLI commands above instead.

A desktop-app **switch** moves several directories, not one file, so it
cannot rely on a single atomic write the way every other byte write does.
Instead the whole planned sequence is written to a journal in `desktop/`
before the first move runs; every byte command checks for that journal at
startup and repairs whatever it finds left behind by an interrupted swap,
not only a retry of **switch** itself. This repair, and the desktop half of
**switch** in general, is Windows only and has been exercised solely
against a synthetic test directory, never a real, signed-in installation of
Claude — see
[Troubleshooting](../docs/troubleshooting.md#desktop-app-switching-is-unverified)
for exactly what has not yet been verified and what would verify it.
