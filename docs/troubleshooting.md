# Troubleshooting

Every error byte can report is listed below, along with what actually
happened and how to recover. All of these come from a single `Error` enum
(`src/error.rs`); the message shown on your terminal is close to verbatim
what's quoted here.

| Symptom | Cause | Fix |
|---|---|---|
| `Claude Code file not found: <path>` | Claude Code isn't installed, or has never logged in, so `.claude.json` or `.credentials.json` doesn't exist yet. Nothing is created or changed. | Install Claude Code and log in at least once, then retry. If you're using `CLAUDE_CONFIG_DIR`, confirm it points at the right directory. |
| `failed to parse <path>: ...` | The file exists but isn't valid JSON — most likely something else wrote to it mid-edit. byte aborts before writing anything rather than overwriting a file it can't fully understand. | Fix or restore the file by hand (see "Recovering from a backup" below), then retry. |
| `io error on <path>: ...` | A filesystem operation failed — permission denied, disk full, a read-only volume, or similar — or, when `<path>` names a registry key such as `Software\Microsoft\Windows\CurrentVersion\Run`, the registry operation behind `byte autostart` failed. This is the most likely real-world error and can surface at almost any step: reading a live file, creating byte's config directory (including the desktop profile store, below), or writing a backup. | Check disk space and permissions on the reported path, then retry; for the registry case, check that your user can write its own `Run` key. |
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
| `byte remove needs confirmation; re-run with --yes to proceed without prompting` | `byte remove` deletes a keychain entry with no backup, and the account's parked Claude Desktop session with it, so it refuses to run unattended: standard input isn't a terminal (a script, cron, or CI), or `--json` is set (a prompt would corrupt machine-readable output). | Re-run with `--yes` if you're sure, or run it interactively without `--json` to be prompted instead. |
| `another byte process is currently changing accounts...` | Another byte process — often the tray — already holds the mutation lock (`mutation.lock` in byte's config directory) because it's mid-switch, mid-capture, or mid-add/remove/rename. byte refused to start a second write sequence rather than risk two processes interleaving writes to the same files. Nothing was read or changed. | Wait for the other process to finish, then retry. Usually that is instant — but a `byte add` waiting for its new login holds the lock for as long as it waits (up to `--timeout`, 300 seconds by default), and the tray reports Busy for that whole window. The lock is an OS-level advisory lock tied to that process's open file handle, so it is always released automatically if that process exits or crashes — there is no lock file to delete by hand. |
| `a desktop profile swap was interrupted and could not be repaired automatically` | byte was interrupted while moving the Claude desktop app's session between accounts. The journal recording that swap is either unreadable, was written by a newer version of byte, or — if this appears right as a switch starts — is simply still sitting on disk from that earlier interruption; byte refuses to start a new swap on top of one that was never repaired, since that would destroy the only record of it while its files may already be half-moved. That refusal is checked before byte writes anything at all, so a switch refused this way changed nothing. Your session data is still on disk — nothing is deleted by a swap, only moved. This never exits non-zero by itself: automatic recovery at the start of every command catches this exact failure and prints it as a warning (`an earlier desktop profile swap could not be repaired automatically, so this command is continuing without touching it: ...`) rather than blocking the command you actually ran, and `byte switch` catches it the same way (see the desktop-switch bullet below) rather than undoing a Claude Code switch that already committed. | Do not delete the journal. Upgrade byte if the message says the format is newer. If it says an earlier swap was never repaired, run any byte command (recovery runs automatically at the start of every command, e.g. `byte list`) and then retry. Otherwise the journal lists every `from`/`to` pair byte intended, so the move can be completed or reversed by hand. |

A few behaviors worth calling out even though they aren't errors:

- **Switching to the already-active account** is a no-op — byte still syncs
  the live credentials back to the store first (in case Claude Code rotated
  the token), then reports that the account was already active.
- **`the desktop session was switched, but <accounts.json path> could not be
  read, so the stored desktop profile for '<account>' will not be listed`**,
  or **`the desktop session was switched, but the stored desktop profile for
  '<account>' could not be recorded in <accounts.json path>: ...`** — two
  wordings of the same warning, not a failure, depending on whether
  `accounts.json` could be read at all or was read but failed to save. The
  desktop session itself has already been switched — the profile directories
  have moved and the app's `config.json` has been patched — and the only
  thing missing is the bookkeeping line that lets `byte list` show that
  account's stored profile and its size. byte reports it rather than turning
  a switch that fully happened into an error you would have nothing to
  retry. The record is rewritten by the next switch that parks this account.
- **`byte switch` also switches the Claude desktop app's session**, right
  after the Claude Code switch itself completes. What it reports on stderr:
  - **`Claude desktop app switched too.`** — the desktop half moved as well.
  - **`Claude is running, so its desktop session was left on the previous
    account. Quit Claude and run this switch again to move it too.`** —
    Chromium corrupts profile state if its directories move underneath a
    running process, so byte refuses to touch them while the app is open.
    Nothing was changed; quit Claude Desktop and re-run the same switch.
  - **`No desktop session stored for this account yet, so Claude will open
    signed out. Sign in there once and byte will remember it.`** — this
    account has no captured desktop profile yet (it has never been the
    outgoing side of a switch while signed in to Claude Desktop). Sign in
    once and the next switch away from it captures one.
  - **`Claude's desktop app is not signed in as the account byte expected
    there, so its session was left alone rather than filed under the wrong
    account -- nothing changed. If it's already showing the account you just
    switched to, there is nothing more to do. Otherwise, sign out of Claude
    Desktop and sign in again there as the account you want; byte will
    capture that session the next time you switch away from it.`** — in the
    commonest case, `config.json`'s `lastKnownAccountUuid` names a different
    account than the one byte was about to park the live session under (the
    list further down covers the other ways to reach the same refusal, where
    the app names no account byte can use at all). The two halves have
    drifted apart: byte's tray switches Claude Code without touching the
    desktop app at all, and you can sign into Claude Desktop by hand at any
    time.

    The most common way to reach this message is exactly that drift: a tray
    click switches Claude Code to a new account while the desktop app stays
    signed in as the old one; a later CLI `byte switch` back to that old
    account then finds the desktop app *already* correct, but byte's own
    bookkeeping still names the account Claude Code is switching *away from*
    as the one to file the live session under. That is why the message no
    longer says unconditionally to sign out — an earlier wording did, and
    following it in this exact case would destroy a working, correctly
    signed-in session for no benefit. Re-running the same `byte switch` is
    not the fix either: by the time this message prints, the Claude Code
    half has already committed, so a repeat of the identical switch is a
    self-switch and never re-attempts the desktop half at all — the only way
    to actually change what the desktop app is signed in as is by hand,
    directly in Claude.

    In the genuine-drift case (the app is signed in as neither the account
    you switched from nor the one you switched to), parking anyway would
    write this account's cookies and OAuth keys into the *other* account's
    stored profile, so a later switch into that account would restore the
    wrong session and apply the wrong identity — which is what the refusal
    itself still guards against regardless of which of the two situations
    produced it. Nothing on disk was changed — the Claude Code switch itself
    still happened.

    Five further states reach the same refusal, all of them "byte cannot
    tell whose session this is":

    - **A signed-out app (no `lastKnownAccountUuid` at all) over a store
      that already holds that account's session.** A signed-out app on its
      own is *not* a mismatch and is parked normally — there is no session
      there to misfile, and that is the ordinary state of a machine you have
      never signed into. But byte itself produces a signed-out app every
      time it parks a profile for an account with nothing stored, and after
      that the store is full. Reading an absent uuid as "nothing to misfile"
      there would overwrite that account's real saved sign-in with an empty
      one, and nothing recovers it. Sign in to Claude Desktop, or switch to
      the account whose session is actually stored.
    - **A signed-out app whose stored profile holds saved keys but no
      directories.** Same protection, one level down: the refusal is on the
      write that would replace those keys, not just on the directories.
    - **A signed-out app while Claude Code is *also* logged out, over a
      desktop directory that still holds a session.** Neither half names an
      account, so there is nothing to file the live session under — and
      leaving it in place while the incoming account's profile is installed
      beside it is exactly the blend the refusal exists to prevent. This is
      not an exotic state: byte itself clears `lastKnownAccountUuid`
      whenever it restores a profile that has no saved sign-in stored with
      it, and a tray click switches Claude Code without touching the desktop
      app at all. A desktop directory with nothing session-bearing in it is
      *not* refused — that is a machine that has never signed in, and its
      first switch still works. Sign in to Claude Desktop as the account you
      want, or log in to Claude Code first so byte knows which account it is
      switching away from.
    - **A `lastKnownAccountUuid` that is not a string.** byte will not guess
      whose session it is looking at.
    - **A `lastKnownAccountUuid` that could not be a directory name** —
      empty, `.`, `..`, or containing `/`, `\`, `:` or a NUL. byte files
      parked profiles in one directory per account, named by that value, and
      a value like `../elsewhere` would put the parked session outside the
      profile store entirely. Nothing on your machine writes such a value;
      byte checks because `config.json` belongs to another application and
      byte does not own what goes into it.
  - **`the desktop session for this account was restored, but its saved
    sign-in at <path> could not be read (...), so Claude Desktop will open
    signed out. Sign in there once and byte will capture it again.`**,
    followed by **`Claude's desktop app has this account's session back, but
    will open signed out. Sign in there once and byte will remember it
    again.`** — the account's profile directories moved back into place, but
    the `oauth.json` byte parked beside them is present and unreadable
    (truncated by a full disk, or damaged). byte will not guess at its
    contents, and it will not silently fall back to an empty one either:
    that would sign the app out while reporting a clean switch. It clears
    the app's account keys instead — leaving the previous account's keys
    over this account's cookies is the one outcome that must not happen —
    and says so. Your session data is intact; only the saved sign-in is
    gone. Sign in to Claude Desktop once and the next switch away captures a
    fresh one. The swap is finished, so nothing is left on disk to retry and
    no journal remains.
  - **`the Claude Code switch succeeded, but its desktop app session could
    not be switched: ...`** — the desktop half hit an error (for example, an
    unrepaired journal from an earlier interruption — see
    `DesktopSwapInterrupted` above). This is a warning, not a failure: the
    Claude Code switch you asked for already happened and is not undone by a
    problem in this second, best-effort half. Re-run `byte switch` (or any
    byte command, to trigger recovery first) once the underlying problem is
    resolved.

    If the error came from patching `config.json` itself — a permissions
    problem on `%APPDATA%\Claude`, a full disk — the swap's journal is
    deliberately **left on disk**, because the directories have already
    moved and the app's account keys have not yet been updated to match.
    Recovery at the start of the next byte command finishes exactly that
    step. Do not delete the journal; fix the underlying problem and run any
    byte command.

    Until that happens, the two halves disagree, and it is worth being
    explicit about what that means if you open Claude Desktop in the
    meantime: the profile directories are the **incoming** account's, while
    `config.json` still holds the **outgoing** account's `oauth:` keys, so
    the app authenticates as the account you switched *away from* over the
    account you switched *to*'s cookie jar. What it actually shows in that
    state is not something byte can predict. Fix the underlying problem and
    run any byte command before using the app; the recovery is what puts the
    two back in agreement.

  On a machine with no Claude Desktop data directory at all — every
  non-Windows platform (unless `CLAUDE_DESKTOP_DIR` is set), and any Windows
  machine where the desktop app is not installed or has never been run —
  none of the above appears; the desktop half is silently skipped rather
  than warning on every single switch about a directory that will never
  exist there. byte checks for the directory itself, not just for
  `%APPDATA%`, which is set for every Windows user whether or not Claude
  Desktop was ever installed: nothing is created under `%APPDATA%\Claude`,
  no profile is parked, and `byte list` reports no desktop session.

  **`byte switch --json` switches the desktop half too**, and answers in the
  payload instead of on stderr: a `desktop` field carrying one lowercase
  string — `"switched"`, `"app_running"`, `"no_profile_for_incoming"`,
  `"identity_mismatch"`, `"switched_without_identity"` or `"nothing_to_do"`
  — one per case above, including the self-switch case that prints nothing
  at all — plus `"failed"` when the
  desktop half errored. The field is `null` when the
  desktop half was not attempted at all: switching to the already-active
  account, or a machine with no Claude Desktop data directory. The key is
  always present, so `null` is distinguishable from an older byte that never
  emitted the field. The *advice* messages above — the ones telling you what
  to do about each outcome — are not printed under `--json`. Warnings that
  name a specific file or failure still are, on stderr, because the payload
  has nowhere to put them: a desktop failure's own error text (the field can
  only say `"failed"`), the `switched_without_identity` warning naming the
  unreadable `oauth.json` and why it could not be read, and the
  `accounts.json` bookkeeping warnings above. Everything on **stdout** is
  still the payload alone, which is what `--json` actually guarantees. A
  desktop failure never fails the command — `byte switch --json` still exits
  zero with a valid JSON object on stdout.
- **`repaired an interrupted desktop profile swap (RollForward)` /
  `(Reverse)`** on `byte list` or any other command is a confirmation, not an
  error: it means an earlier `byte switch` was interrupted mid-swap, and this
  command's automatic recovery (see `DesktopSwapInterrupted` above) just
  finished either completing the install (`RollForward`) or undoing the
  parks (`Reverse`). A swap is two things — the profile directories, and the
  account keys inside the desktop app's own `config.json` — and *this*
  wording means both landed: nothing further is needed. The two variants
  below are the same repair reporting that one half did not.

  That recovery only runs when
  the command can take byte's mutation lock: a journal is on disk for the
  whole of every *healthy* swap too, so if another byte process is
  mid-switch right now, that journal is its swap in flight and is left
  strictly alone — silently, because a concurrent command is ordinary, not a
  fault. Run the command again once the other one finishes if you were
  expecting a repair.
- **`repaired an interrupted desktop profile swap (RollForward), but the
  saved sign-in at <path> could not be read, so Claude Desktop may open
  signed out. Sign in there once and byte will capture it again.`** — the
  same repair, with its second half done as well as it can be: the
  directories are where they belong, but the `oauth.json` the swap needed is
  present and unreadable. byte cleared the app's account keys rather than
  leave the other account's in place. Sign in to Claude Desktop once.
  Nothing is left on disk to retry — the journal is cleared, because
  re-reading the same damaged file would not go differently.
- **`repaired an interrupted desktop profile swap (Reverse). The saved
  sign-in at <path> could not be read, but nothing needed it: Claude
  Desktop's own config already describes the session that was put back, so
  it will open as that account. byte will capture a fresh saved sign-in the
  next time you switch away from it.`** — the same unreadable file, in the
  other direction, where it costs nothing. A reversal restores the session
  that was *already* live, and the app's `config.json` was never patched
  away from it, so there is nothing to sign in for and no action to take.
  This is deliberately not the wording above: an earlier version predicted a
  signed-out app here too, which would have sent you to sign out of and back
  into a session that works.
- **`repaired the directory half of an interrupted desktop profile swap
  (...), but Claude Desktop's own data directory could not be located, so
  which account it is signed in as was left alone. The swap's journal has
  been kept so a later byte command can finish it.`** — the profile
  directories were put back where they belong (that needs nothing but the
  journal), but `%APPDATA%\Claude` could not be resolved this run, so
  `config.json` was not reached. This is the one repair that deliberately
  does **not** clear the journal: the swap is genuinely unfinished, and only
  a command that can find the app can complete it. Run byte again on the
  machine and account where Claude Desktop is installed, or set
  `CLAUDE_DESKTOP_DIR` (see [Configuration](configuration.md)). Do not
  delete the journal.
- **`byte could not take its mutation lock to check for an interrupted
  desktop profile swap, so this command is continuing without checking:
  ...`** is the third possibility for that same check: the lock file itself
  (`mutation.lock` in byte's config directory) could not be opened or
  locked — a permissions problem on that directory, most likely. It is a
  warning, never a failure: a lock problem must not break an unrelated
  command. Fix the permissions on byte's config directory (see
  [Configuration](configuration.md)) and any pending repair happens on the
  next command.
- **`an earlier desktop profile swap could not be repaired automatically, so
  this command is continuing without touching it: ...`** is what every byte
  command prints instead of the confirmation above when automatic recovery
  itself fails (an unreadable journal, or one written by an incompatible
  format version — see `DesktopSwapInterrupted` above). The command you
  actually ran (`byte list`, `byte current`, `byte switch`, ...) still
  completes normally; only the stale journal is left exactly as it was. This
  is deliberate: letting a broken journal fail every single byte command
  would turn one corrupt file into total unavailability of the whole CLI.
  Follow `DesktopSwapInterrupted`'s own fix above to actually resolve it.
- **`byte list` marks accounts with a stored desktop session** and its size
  on disk (`[desktop session: 12.3 MB]`), and `--json` reports the same
  record under a `desktop_profile` key — `null` for an account that has
  never had one captured, or `{"captured_at": ..., "bytes": ...}` once it
  has.
- **`byte remove` deletes the account's parked desktop session too**, along
  with its keychain entry and its `accounts.json` row — the whole of
  `<byte-config-dir>/desktop/<account-uuid>/`, which holds that account's
  signed-in claude.ai session and its saved sign-in as plain files. The
  confirmation prompt names it when there is one to delete (**`Remove
  '<label>'? Its stored credentials AND its saved Claude Desktop session —
  that account's signed-in claude.ai session on this machine — are both
  deleted, and cannot be recovered afterward.`**) and does not mention it
  otherwise. Deleting byte's copy is not the same as revoking the session:
  see [Security](../SECURITY.md) for what to do if you need it actually
  revoked.
- **`the account was removed, but its stored Claude Desktop session at
  <path> could not be deleted (...). That directory still holds a live
  claude.ai session and a saved sign-in for it; delete it by hand.`** — a
  warning, not a failure. The removal itself has already happened: the
  keychain entry is gone and `accounts.json` no longer lists the account, so
  there is nothing to retry and no byte command left that can reach the
  directory. The usual cause on Windows is a file inside the profile still
  held open by another process (Claude Desktop itself, an indexer, an
  antivirus scanner). Quit Claude Desktop and delete the named directory
  yourself.
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

## Desktop app switching is unverified

Every automated test for the Windows desktop-switching feature above runs
against a synthetic profile tree in a temporary directory (see
[Architecture](architecture.md)) — none of them launch the real Claude
desktop app. That leaves the one question that actually matters unanswered:
whether swapping this particular set of directories and `config.json` keys
actually changes what a real, running Claude shows as signed in.

It cannot be answered by an automated test, and, worse, it cannot currently
be checked from inside a session running in the Claude desktop app either:
the decisive test requires quitting the app entirely, which ends the very
session that would be doing the checking. It needs a person, a second Claude
account, and a willingness to be signed out mid-session:

1. `byte capture` the current desktop session by switching away and back
   once.
2. Quit Claude entirely. Confirm no `claude.exe` under
   `AppData\Local\AnthropicClaude` remains.
3. `byte switch <other account>`.
4. Open Claude. Confirm it is signed in as the other account — the chat side
   *and* the Code tab.
5. Switch back. Confirm the original session returns, including
   conversation history.

If step 4 shows a signed-out app rather than the other account, the
OAuth-cache half (`config.json`'s `oauth:` keys and `lastKnownAccountUuid`)
is not sufficient on its
own to switch the app, and that finding belongs back in the design — not
papered over here — before anything built on top of it should be trusted.

Until someone has actually run this checklist against a real signed-in
installation, treat desktop switching as **unproven, not tested**: every
other claim in this document and elsewhere about the desktop half describes
what the code is written to do, not what has been confirmed to happen in the
real app.

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
