# Configuration

byte has no config file of its own to edit. It reads two files that belong to
Claude Code, writes one small metadata file of its own, and stores secrets in
the OS credential store. Every path below can be overridden with an
environment variable for testing or non-standard setups.

## Environment variables

| Variable | Type | Default | Effect |
|---|---|---|---|
| `CLAUDE_CONFIG_DIR` | path | unset | Directory containing Claude Code's `.claude.json` and `.credentials.json`. When set, both files are read directly from this directory (`<dir>/.claude.json`, `<dir>/.credentials.json`) instead of the default locations below. |
| `BYTE_CONFIG_DIR` | path | unset | byte's own configuration directory, holding `accounts.json`, `backups/`, and `desktop/`. When unset, falls back to the platform default below. |
| `CLAUDE_DESKTOP_DIR` | path | unset | Directory containing the Claude **desktop** app's own data (normally `%APPDATA%\Claude` on Windows). Read only by the desktop half of `byte switch` — see [The desktop profile store](#the-desktop-profile-store) below. Setting it on a non-Windows platform does not make the feature supported there; it exists mainly so the swap can be tested against a synthetic directory instead of a real installation. |

## Claude Code's files (read and patched, never fully rewritten)

When `CLAUDE_CONFIG_DIR` is not set, byte looks in the same places Claude
Code itself uses:

| File | Default location | What byte reads/writes |
|---|---|---|
| `.claude.json` | `~/.claude.json` | The `oauthAccount` object and the `userID` field. Every other key (settings, project history, plugin usage, onboarding flags — roughly 80 top-level keys on a typical install) is preserved byte-for-byte. |
| `.credentials.json` | `~/.claude/.credentials.json` | The `claudeAiOauth` object. `mcpOAuth` (per-MCP-server tokens) is preserved untouched. |

byte never deserializes either file into a typed struct — it patches specific
keys in a parsed JSON document and re-serializes the rest unchanged,
preserving key order and the original pretty-printed or minified formatting.

## byte's own configuration directory

`accounts.json` and `backups/` (see below) live in `BYTE_CONFIG_DIR` if set,
otherwise in the platform default:

| Platform | Default `BYTE_CONFIG_DIR` |
|---|---|
| Windows | `%APPDATA%\byte` |
| macOS | `~/Library/Application Support/byte` |
| Linux / other Unix | `$XDG_CONFIG_HOME/byte`, or `~/.config/byte` if `XDG_CONFIG_HOME` is unset |

Within that directory:

| Path | Contents |
|---|---|
| `accounts.json` | Every stored account's metadata and which one is active: label, email, organization, UUID, billing type, organization role, subscription tier, the associated Claude Code `userID`, and added/last-used timestamps — in effect, the full `oauthAccount` profile Claude Code stores per account, plus display fields byte derives from it. Once an account's Claude desktop session has been parked at least once, its entry also carries a `desktop_profile` record (when it was captured, and its size on disk); absent until then. That record is metadata about the parked session, not the session itself — the session's own files live under `desktop/`, below. No secrets: `accessToken`/`refreshToken` are never written here (see below). Safe to read directly. |
| `backups/` | Timestamped copies of `.claude.json`, `.credentials.json`, and `accounts.json`, made before every write. The ten most recent per file are kept. See [Troubleshooting](troubleshooting.md) for how to restore one. |
| `desktop/` | One subdirectory per account with a parked Claude desktop app session, named by the account's UUID, holding that session's files (cookies, local/session storage, Code-tab history) plus `oauth.json` (the desktop app's own captured OAuth cache for that account). `journal.json` sits directly under `desktop/` and exists only while a swap is in progress — normally absent. **Contains live session credentials as ordinary files, not OS-keychain entries** — see [The desktop profile store](#the-desktop-profile-store) below. Windows only: absent entirely on every other platform unless `CLAUDE_DESKTOP_DIR` is set. |
| `mutation.lock` | An empty file used only as an advisory OS lock, held for the duration of a single `switch`/`capture`/`add`/`remove`/`rename` — whichever process (CLI or tray) is doing it. Its content, if any, is not meaningful; only holding the lock is. Released automatically when that process exits, even on a crash. |
| `tray.lock` | The same kind of advisory lock as `mutation.lock`, held for as long as the tray runs, so a second `byte` started with no arguments refuses to start a duplicate tray. Independent of `mutation.lock` — holding one never blocks the other. See [Troubleshooting](troubleshooting.md) for what each lock's contention error means. |

`accounts.json` carries its own `schema` field, versioning the document's
layout independently of the per-account credential schema below. byte
refuses to load a file whose `schema` it does not recognize rather than risk
misparsing it — see the troubleshooting entry for
`unsupported accounts.json schema version`.

## The tray and autostart

Running `byte` with no arguments starts a tray icon on **Windows and macOS**
(see [README](../README.md) for what it shows and how it behaves); on other
platforms it prints a message and exits rather than starting anything, since
the tray's dependencies cannot function there.

The tray never starts at login on its own. `byte autostart enable` opts in by
registering the current executable's path in one of these platform-specific
locations — the exact place `byte autostart status` and `describe_location`
report:

| Platform | Registered in |
|---|---|
| Windows | The registry Run key, `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, under the value name `byte`. |
| macOS | A LaunchAgent plist at `~/Library/LaunchAgents/fyi.jocke.byte.plist`. |
| Linux | A `.desktop` entry at `~/.config/autostart/byte.desktop`. Since the tray itself is Windows/macOS only (see above), a `byte` launched this way exits immediately with the tray's own "unavailable" message — `byte autostart enable` does not currently check for this before registering. |

`byte autostart disable` removes the entry, treating an already-absent one as
success — so running either `enable` or `disable` twice in a row is harmless.

## Where credentials are stored

Only the `claudeAiOauth` object -- `accessToken`, `refreshToken`,
`expiresAt`, and the other fields Claude Code writes into that block, but
**not** the `oauthAccount` profile above -- is stored as one entry per
account in the operating system's credential store, under the service name
`byte-claude-account-switcher`:

| Platform | Backend |
|---|---|
| Windows | Windows Credential Manager — look for a generic credential named `byte-claude-account-switcher`. |
| macOS | The login Keychain — search for the service name `byte-claude-account-switcher`. |
| Linux | A Secret Service provider (e.g. GNOME Keyring or KWallet) via `zbus`. |

Splitting the two this way (rather than storing one combined entry per
account) is what keeps each keychain entry under the OS credential store's
size limit — Windows Credential Manager's is the tightest, at 1280
characters once its UTF-16 encoding is accounted for, which a combined
entry could exceed on an account with a large profile. `byte switch`
reassembles the two halves in memory when it needs a complete snapshot to
apply; neither half is ever written to disk combined.

`accounts.json` never contains a refresh token or access token. The
credential store does — and so, in plaintext, do the `.credentials.json`
backups in `backups/` (above): once a refresh token has been live on this
machine, it persists in one or more backup generations for a while
afterward, independent of the credential store entry. Removing an account
with `byte remove` deletes its metadata entry and its credential store
entry, but does **not** delete its past backups. See
[Security](../SECURITY.md#threat-model) for the full picture of where
credentials rest and what removal does and doesn't clear.

## The desktop profile store

Everything described above fits in the OS credential store. A Claude
**desktop** app session does not: its cookies and local/session storage run
to tens of megabytes per account, and Windows Credential Manager's
per-entry budget — the same roughly-1280-character limit that motivated
splitting `oauth` from `account` above — has no room for it at any split.

So, on Windows, each parked desktop session lives at
`<byte config dir>/desktop/<account-uuid>/` as ordinary files: cookies,
local/session storage, Code-tab history, and `oauth.json` (the desktop
app's own OAuth cache for that account, captured from its `config.json`).
This is the **one place byte stores a secret outside the OS credential
store**. See the [README](../README.md#desktop-app-windows) for what moves
between accounts versus what gets patched in place, and
[Architecture](architecture.md) for how a swap that takes many separate
directory renames, rather than one atomic write, still survives being
interrupted partway through.

Its permissions are more modest than "the one place a secret lives outside
the keychain" might suggest, and this project would rather say so plainly
than overstate them. The directory is created under byte's own config
directory and takes on that location's permissions — on Windows, whatever
ACL `%APPDATA%` carries for the current user, the same protection
`accounts.json` itself already relies on — rather than being given a
separate, narrower ACL of its own. Setting a real per-directory ACL on
Windows needs a Win32 dependency this project deliberately does not add;
until that changes, **do not read this directory as "ACL-restricted"** —
it inherits the config directory's permissions, nothing more. On
Unix-like systems, relevant today only to development and tests since the
feature itself only runs on Windows, the directory additionally gets an
explicit owner-only `0700` mode. Treat `desktop/` with at least the same
care as `backups/` above: anyone who can read your user profile can read
what is in it.
