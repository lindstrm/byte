# byte

claude account switcher

[![CI](https://github.com/lindstrm/byte/actions/workflows/ci.yml/badge.svg)](https://github.com/lindstrm/byte/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

## Why?

- Switch between a personal and a work Claude account without logging out
  through the browser and back in again every time.
- Credentials live in your OS's credential store (Windows Credential Manager,
  macOS Keychain, or a Linux Secret Service provider). Only the OAuth tokens
  go there — each account's profile (email, organization, billing type) sits
  in byte's own `accounts.json`, which never holds a token. Pre-write backups
  are plaintext, though — see [Security](SECURITY.md) for exactly where and
  for how long.
- Only the account identity is swapped. Settings, project history, plugins,
  and MCP server tokens are shared across accounts and never touched.
- Every write is backed up first, replaced atomically, and verified
  afterward, so a crash mid-switch can't corrupt `~/.claude.json`.
- Scriptable: every command accepts `--json` for machine-readable output.
- On Windows, `byte switch` also switches the Claude *desktop* app's own
  session, so one command moves both — see
  [Desktop app (Windows)](#desktop-app-windows). Its per-account session
  store holds live credentials as ordinary files, the one place byte keeps a
  secret outside the OS credential store, because those credentials don't
  fit in one.

## Prerequisites

- Rust 1.88 or later (edition 2024) to build from source — see
  [`rust-toolchain.toml`](rust-toolchain.toml).
- [Claude Code](https://claude.com/claude-code) installed and already logged
  in as at least one account. `byte` reads Claude Code's own credential
  files; it does not perform the OAuth login itself.
- An OS credential store: Windows Credential Manager, the macOS login
  Keychain, or a Secret Service provider on Linux (e.g. GNOME Keyring or
  KWallet).

## Install

```sh
cargo install --path .
```

## Quick start

```sh
# Save the account you're currently logged in as
byte capture

# Log out, log in as a second account, and save it too
# (asks for confirmation first — it logs Claude Code out)
byte add

# Optional: accounts are labeled by email by default — give them names you'll
# actually type
byte rename you@personal.example.com personal
byte rename you@work.example.com work

# Switch back and forth by label, email, or UUID prefix
byte switch work
byte switch personal
```

## Usage

| Command | Description |
|---|---|
| `byte` | Start the tray icon (Windows and macOS only — see [Tray](#tray); fails on other platforms) |
| `byte list` | List stored accounts; the active one is marked with `*` |
| `byte current` | Print the active account's label |
| `byte switch <name>` | Switch to a stored account (on Windows, also switches the Claude desktop app's session — see [Desktop app (Windows)](#desktop-app-windows)) |
| `byte capture` | Save the currently logged-in account |
| `byte add [--timeout <secs>] [--yes]` | Log out, then save the next account you log in as (default 300s; prompts before logging out unless `--yes` is given) |
| `byte remove <name> [--yes]` | Forget a stored account (irreversible; prompts for confirmation unless `--yes` is given) |
| `byte rename <name> <label>` | Change an account's display label |
| `byte autostart enable\|disable\|status` | Opt in (or out) of starting the tray at login |

`<name>` matches a label, an email address, or an account UUID prefix. Every
command accepts `--json` for machine-readable output on stdout; status
messages always go to stderr, so `--json` output can be piped safely.
`byte add` and `byte remove` both require `--yes` under `--json` or when
standard input isn't a terminal, since neither can prompt in either case —
for `byte add` the check runs *before* it logs Claude Code out.

See [`man/byte.md`](man/byte.md) for the full reference, or run `byte --help`.

## Tray

Running `byte` with no arguments — **on Windows and macOS only** — opens a
tray icon instead of the CLI. Its menu lists every stored account, marking
the same active one `byte list` does, plus **Add account…** and **Quit**.
Clicking an account switches to it. Clicking **Add account…** opens a
terminal running `byte add`, rather than adding the account in place: doing
that means logging Claude Code out and waiting for an interactive login,
which needs a console to prompt in and a human to answer. The terminal stops
at a confirmation prompt, so a mis-aimed click in the notification area costs
nothing — answer `n`, or close the window, and you stay logged in. The new
account appears in the menu on its own once it's saved.

Every notification is also written to the terminal the tray was started
from, and the tooltip always names the active account (under autostart there is no console, so
the tooltip is the one that remains). Both matter, because
a notification the OS accepts is not one you necessarily see: on Windows 11
byte's toasts were accepted and recorded in the notification database while
none were ever drawn on screen, with success reported at every step. **If a
menu click looks like it did nothing, read the tray's terminal** — the
action almost certainly happened.

Only one tray runs at a time; starting a second `byte` while one is already
running reports that instead of opening a duplicate icon. The tray and the
CLI cooperate rather than compete: a CLI `byte switch` updates the running
tray's menu automatically (it watches `accounts.json` for changes), and a
lock keeps a CLI mutation and a tray-driven one from interleaving their
writes to the same files — held only for the write itself, except during
`byte add`, which holds it while it waits for the new login — see [Configuration](docs/configuration.md)
and [Troubleshooting](docs/troubleshooting.md) for both locks.

On Linux (or any platform besides Windows and macOS), running `byte` with no
arguments does not start a tray — its dependencies can't function there. It
exits non-zero with an error pointing at the CLI commands above instead.

The tray never starts itself at login. `byte autostart enable` opts in
explicitly; `byte autostart status` reports whether it's registered, and
`byte autostart disable` removes it. See
[Configuration](docs/configuration.md) for exactly where each platform
registers it.

## Desktop app (Windows)

On Windows, `byte switch` also switches the Claude **desktop** app's own
session, right after the Claude Code switch completes — one command moves
both. This only happens for a CLI `byte switch`; a tray click switches
Claude Code alone (see [Tray](#tray) above), so if you mostly use the tray,
the desktop app can sit on a different account until you run `byte switch`
from a terminal once.

What moves with the account: its chat session (cookies, local/session
storage) and the Code tab's conversation history — so switching away from an
account hides its Code-tab history in the app until you switch back to it.
`config.json` is patched in place rather than moved, since it also holds
your theme, window size, and other preferences; only the handful of keys
that identify the signed-in account are touched. MCP server configuration,
cached artifact partitions, and other app preferences are left alone
entirely.

Switching to an account that has never had a desktop session captured
leaves the app **signed out** — there is nothing stored for it yet. Sign in
there once and byte captures that session automatically the next time you
switch away from it, the same way `byte add` captures a fresh Claude Code
login.

If Claude is running, the desktop half is skipped and reported rather than
attempted, rather than risk corrupting a session by moving its files out
from underneath the running app. Quit Claude and run the same switch again
to move it.

**Credentials on disk.** A desktop session's cookies don't fit in the OS
credential store, so each account's parked session is stored as ordinary
files under `<byte config dir>/desktop/` instead — the one place byte keeps
a secret outside the OS credential store. See
[Configuration](docs/configuration.md#the-desktop-profile-store) for exactly
what that means for its permissions (in short: it inherits the config
directory's own permissions, and is not additionally ACL-restricted on
Windows).

**Unverified.** No automated test exercises the real Claude desktop app —
they all run against a synthetic directory tree instead, which is the only
way to test this without risking the tester's own session. Whether this
actually changes what a real, signed-in Claude shows is therefore unproven,
not merely untested-by-CI. See
[Troubleshooting](docs/troubleshooting.md#desktop-app-switching-is-unverified)
for exactly what would verify it.

Windows only. On macOS and Linux, `byte switch` behaves exactly as it did
before this feature existed, with no desktop half attempted.

## Configuration

byte reads three environment variables — the third only on Windows, and only
for the desktop half of `switch` described above:

| Variable | Effect |
|---|---|
| `CLAUDE_CONFIG_DIR` | Overrides where Claude Code's `.claude.json` and `.credentials.json` are read from |
| `BYTE_CONFIG_DIR` | Overrides byte's own config directory (`accounts.json`, `backups/`, `desktop/`) |
| `CLAUDE_DESKTOP_DIR` | Overrides where the Claude desktop app's own data is read from (normally `%APPDATA%\Claude`) |

See [Configuration](docs/configuration.md) for default paths per platform and
how a stored account is split between `accounts.json` and the OS keychain.

## Examples

See [`examples/`](examples/) for runnable demos, including
`roundtrip_check`, which round-trips a real `.claude.json` through byte's
JSON writer and diffs the result — useful for confirming byte preserves a
file it hasn't seen before.

## Troubleshooting

byte never writes over a file it could not parse, and every write is backed
up first — if a switch leaves things looking wrong, your previous file is in
`<config-dir>/backups/`. See [Troubleshooting](docs/troubleshooting.md) for
the full error reference.

## Documentation

- [Getting started](docs/getting-started.md)
- [Configuration](docs/configuration.md)
- [Architecture](docs/architecture.md)
- [Troubleshooting](docs/troubleshooting.md)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Licensed under [MIT](LICENSE).
