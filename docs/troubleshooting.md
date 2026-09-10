# Troubleshooting

Every error byte can report is listed below, along with what actually
happened and how to recover. All of these come from a single `Error` enum
(`src/error.rs`); the message shown on your terminal is close to verbatim
what's quoted here.

| Symptom | Cause | Fix |
|---|---|---|
| `Claude Code file not found: <path>` | Claude Code isn't installed, or has never logged in, so `.claude.json` or `.credentials.json` doesn't exist yet. Nothing is created or changed. | Install Claude Code and log in at least once, then retry. If you're using `CLAUDE_CONFIG_DIR`, confirm it points at the right directory. |
| `failed to parse <path>: ...` | The file exists but isn't valid JSON — most likely something else wrote to it mid-edit. byte aborts before writing anything rather than overwriting a file it can't fully understand. | Fix or restore the file by hand (see "Recovering from a backup" below), then retry. |
| `io error on <path>: ...` | A filesystem operation failed — permission denied, disk full, a read-only volume, or similar — or, when `<path>` names a registry key such as `Software\Microsoft\Windows\CurrentVersion\Run`, the registry operation behind `byte autostart` failed. This is the most likely real-world error and can surface at almost any step: reading a live file, creating byte's config directory, or writing a backup. | Check disk space and permissions on the reported path, then retry; for the registry case, check that your user can write its own `Run` key. |
| `no account matching '<name>'` | `<name>` didn't match any stored account's label, email, or UUID prefix. | Run `byte list` to see exact labels, or `byte capture` / `byte add` first if the account isn't stored yet. |
| `'<name>' is ambiguous; it matches N accounts` | `<name>` matched more than one account as a prefix. | Use a longer prefix, or the account's exact label or email. |
| `no Claude account is currently logged in` | `byte capture` (or the first half of `byte add`) ran while Claude Code had no live credentials. | Log in with `claude` first, then retry. |
| the live Claude Code login has credentials but `<path>` has no identifiable account... | `.credentials.json` has real tokens, but `.claude.json` has no `oauthAccount` — typically a login that didn't fully complete. byte refuses to guess and changes nothing. | Relaunch Claude Code and complete the login again, then retry. |
| `unsupported snapshot schema version N; this build expects M` | A stored account was saved by a different, incompatible version of byte. | Update byte, or re-authenticate the account with `byte add` to save it under the current schema. |
| `unsupported accounts.json schema version N; this build expects M` | `accounts.json` itself (its document layout, not an individual account's credentials) was written by a different, incompatible version of byte. | Update byte. If you don't need the stored accounts, move `accounts.json` aside (see "Recovering from a backup" below) so byte starts a fresh store, then re-add each account with `byte add`. Moving it aside orphans those accounts' OS keychain entries (service name `byte-claude-account-switcher`) — no byte command can reach them afterward, since `byte remove` itself needs to resolve a name from `accounts.json` first — so clear them by hand if you want them gone: Windows Credential Manager, the macOS login Keychain (Keychain Access.app), or your Linux Secret Service provider's own tool (e.g. Seahorse, KWalletManager, or `secret-tool`). |
| `stored credentials for '<name>' are unusable: ...` | The stored snapshot has no refresh token, no identity to key it by, or (since the keychain-size fix split a snapshot's secret and non-secret halves across two stores) no OS keychain entry at all for an account `accounts.json` still lists — it can't be applied safely. | Re-authenticate with `byte add`. |
| `secret store unavailable: ...` | The OS credential store is locked, unreachable, or (on Linux) no Secret Service provider is running. | Unlock your keychain / login session, or start a Secret Service provider (e.g. `gnome-keyring-daemon`), then retry. |
| `tray error: byte is already running — check your notification area.` | A second `byte` tried to start the tray while one was already running; it holds `tray.lock` in byte's config directory for as long as it runs. This is a different, longer-lived lock than the mutation one above — nothing needs to "finish" for it to clear. | Look for byte's icon in your notification area. If you actually want a fresh tray, quit the running one first. |
| `tray error: the tray is only available on Windows and macOS. ...` | This build is running on Linux (or another platform that is neither). The tray depends on a GTK event loop there that this crate never starts, so it is unconditionally unsupported — not a failure that retrying, unlocking anything, or opening a desktop session will fix. | Use the CLI instead: `byte list`, `byte switch <account>`, `byte add`. None of them need the tray. |
| `tray error: ...` (any other message) | On Windows or macOS: the tray icon, its menu, or the underlying windowing event loop failed to initialize or run — or the tray could not open a terminal for **Add account…** (`could not open a terminal`, `could not locate byte's own executable`); on macOS that is usually a denied Automation permission, re-enabled under System Settings > Privacy & Security > Automation — for example the OS refused to create a tray icon or the event loop could not start. Rare, and most likely a headless session or an unusual remote desktop. | Confirm a desktop session with a system tray is available, then retry. The CLI (`byte list`, `byte switch`, ...) needs no tray and keeps working regardless. |
| `failed to render JSON output: ...` | `--json` output failed to serialize. This should not happen in practice. | Retry without `--json` to confirm the underlying command works, then report the exact command as a bug. |
| `write verification failed for <path>; the original was restored from backup` | byte wrote a file, then read it back and it didn't match what was written. byte already restored the pre-write backup automatically — this is reported so you know a write was rejected, not so you have to fix it. | Retry the command. If it fails repeatedly, check disk space and file permissions on the config directory. |
| `write verification failed for <path>, and restoring the pre-write backup afterwards also failed` | The rare double failure: a write didn't verify, and byte's own attempt to restore the pre-write backup over it *also* failed. The file may now hold neither the old nor the new content. | Follow "Recovering from a backup" below for the named file, then confirm with `claude` and `byte current` that it looks right. |
| applying the account snapshot failed, and rolling back `<path>` afterwards also failed | The rarest failure: writing the credentials file or the config-file half of a switch failed, and restoring the credentials file to its pre-switch state *also* failed. The two files may now disagree about which account is active. | Follow "Recovering from a backup" below for both `.claude.json` and `.credentials.json`, then confirm with `claude` and `byte current` that they agree. |
| `timed out after N seconds waiting for a new login` (`byte add`) | Nobody logged in as a different account within the timeout. | Retry `byte add`, optionally with a longer `--timeout`, and log in via `claude` promptly. |
| `byte add needs confirmation; re-run with --yes to proceed without prompting` | `byte add` logs Claude Code out before it starts waiting for a new login, so it refuses to do that unattended: standard input isn't a terminal (a script, cron, or CI), or `--json` is set (a prompt would corrupt machine-readable output). Nothing was logged out — the check runs before the logout, not after it. | Re-run with `--yes` if you're sure, or run it interactively without `--json` to be prompted instead. |
| `byte remove needs confirmation; re-run with --yes to proceed without prompting` | `byte remove` deletes a keychain entry with no backup, so it refuses to run unattended: standard input isn't a terminal (a script, cron, or CI), or `--json` is set (a prompt would corrupt machine-readable output). | Re-run with `--yes` if you're sure, or run it interactively without `--json` to be prompted instead. |
| `another byte process is currently changing accounts...` | Another byte process — often the tray — already holds the mutation lock (`mutation.lock` in byte's config directory) because it's mid-switch, mid-capture, or mid-add/remove/rename. byte refused to start a second write sequence rather than risk two processes interleaving writes to the same files. Nothing was read or changed. | Wait for the other process to finish, then retry. Usually that is instant — but a `byte add` waiting for its new login holds the lock for as long as it waits (up to `--timeout`, 300 seconds by default), and the tray reports Busy for that whole window. The lock is an OS-level advisory lock tied to that process's open file handle, so it is always released automatically if that process exits or crashes — there is no lock file to delete by hand. |
| `a desktop profile swap was interrupted and could not be repaired automatically` | byte was interrupted while moving the Claude desktop app's session between accounts. The journal recording that swap is either unreadable, was written by a newer version of byte, or — if this appears right as a switch starts — is simply still sitting on disk from that earlier interruption; byte refuses to start a new swap on top of one that was never repaired, since that would destroy the only record of it while its files may already be half-moved. That refusal is checked before byte writes anything at all, so a switch refused this way changed nothing. Your session data is still on disk — nothing is deleted by a swap, only moved. | Do not delete the journal. Upgrade byte if the message says the format is newer. If it says an earlier swap was never repaired, run any byte command (recovery runs automatically at the start of every command, e.g. `byte list`) and then retry. Otherwise the journal lists every `from`/`to` pair byte intended, so the move can be completed or reversed by hand. |

A few behaviors worth calling out even though they aren't errors:

- **Switching to the already-active account** is a no-op — byte still syncs
  the live credentials back to the store first (in case Claude Code rotated
  the token), then reports that the account was already active.
- **`the desktop session was switched, but ... the stored desktop profile
  for '<account>' could not be recorded`** is a warning, not a failure. The
  desktop session itself has already been switched — the profile directories
  have moved and the app's `config.json` has been patched — and the only
  thing missing is the bookkeeping line that lets `byte list` show that
  account's stored profile and its size. byte reports it rather than turning
  a switch that fully happened into an error you would have nothing to
  retry. The record is rewritten by the next switch that parks this account.
- **Removing the active account** is allowed, once confirmed (see
  `byte remove` above). `byte` stops tracking it as active, but Claude Code
  itself is left logged in as that account's credentials until you run
  `byte switch` to something else.
- **Sessions already running.** Claude Code only reads its credentials at
  startup. After every switch, byte checks for already-running `claude`
  sessions and, only if it finds at least one, prints a reminder that they
  keep using the previous account until restarted; with none running, it
  says nothing.
- **The tray's "Add account…" menu item opens a terminal.** It doesn't add
  the account in place: that means logging Claude Code out and waiting for an
  interactive login, which needs a console to prompt in and a human to answer
  — neither of which a menu click supplies. The terminal stops at `byte
  add`'s confirmation prompt, so a mis-aimed click costs nothing: answer `n`,
  or close the window, and you stay logged in. If no terminal opens, the
  notification says so and you can run `byte add` in one yourself. That message is printed to the terminal the tray
  was started from as well, so the click is never silent even when the
  notification isn't drawn — see the next entry.
- **Tray notifications never appear.** byte's notifications are best-effort:
  it asks the OS to show one and has no way to learn whether anything was
  drawn. On Windows 11 this has been observed to fail completely and
  silently — the toasts were accepted, written to the notification database,
  and never displayed, with the API reporting success at every step and
  nothing to log. The tray is not broken when this happens and the action
  the notification described did take place. Every notification is also
  printed to the terminal the tray was started from, and the tooltip always
  names the active account; use those to confirm what happened. To get the
  toasts themselves back, check Windows' Do Not Disturb setting and the
  per-app notification settings for **Windows PowerShell** — that is the app
  identity byte's toasts are sent under.
- **Any failure while `byte add` is waiting for a login** — a timeout, or
  anything else, such as a parse error from catching Claude Code mid-write
  to one of its files — always triggers a restore attempt before the error
  is reported, because `byte add` has already logged you out by the time it
  starts waiting. If the restore succeeds, you see the original error and
  nothing more: your previous account is back if you had one, or a "nothing
  to restore" status if you started `byte add` already logged out. If the
  restore *also* fails, you see both errors printed one after the other —
  the original cause, then the restore failure — since losing track of
  either could leave you unsure whether you're logged in as anything. Two
  errors from one `byte add` is the signal to check `byte current` and, if
  it looks wrong, follow "Recovering from a backup" below.

## Recovering from a backup

byte backs up every file before it writes to it, in
`<byte-config-dir>/backups/` (see [Configuration](configuration.md) for
where that is on your platform). Filenames are `<original-name>.<timestamp>.bak`
— for example `.claude.json.1755400000000.bak` — so sorting the directory
also sorts them chronologically. The ten most recent backups per file are
kept.

Most failures (parse errors, verification failures) are handled
automatically and don't need a manual restore. If you do need one — for
example after an `ApplyRollbackFailed` error, or if a switch just looks
wrong — copy the most recent relevant backup back over the live file. The
examples below assume `BYTE_CONFIG_DIR` is set; if you rely on the platform
default instead, substitute the path from [Configuration](configuration.md).

```sh
# Example: restore .claude.json on Linux/macOS
cp "$BYTE_CONFIG_DIR/backups/.claude.json.<timestamp>.bak" ~/.claude.json
```

```powershell
# Example: restore .claude.json on Windows
Copy-Item "$env:BYTE_CONFIG_DIR\backups\.claude.json.<timestamp>.bak" "$HOME\.claude.json"
```

After restoring, confirm the file still parses (`claude` should start
normally) and that `byte current` reports the account you expect.
