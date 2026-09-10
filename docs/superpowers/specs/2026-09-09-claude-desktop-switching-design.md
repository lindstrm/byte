# Claude Desktop switching — design

Status: approved design, not yet implemented
Date: 2026-09-09
Supersedes: §14 of `2026-08-17-byte-account-switcher-design.md` (see §2)

## 1. Purpose

byte switches Claude Code's account by moving a small OAuth snapshot between
`~/.claude/.credentials.json`, `~/.claude.json`, and a per-account store. The
Claude desktop app on Windows keeps its own, entirely separate session state,
so a byte switch today leaves the app signed in as the previous account.

This design extends byte to switch the desktop app's session alongside Claude
Code's, so one `byte switch` moves both.

Windows only. macOS is deliberately out of scope (§13).

## 2. What was found, and how it corrects §14

§14 of the original design recorded Claude Desktop as holding "a Chromium web
session" and concluded that it and Claude Code use "entirely separate
authentication systems". Both halves need correcting. Verified by inspection
on Windows on 2026-09-09.

**The desktop app also keeps OAuth tokens in a plain JSON file.**
`%APPDATA%\Claude\config.json` contains `oauth:tokenCache`,
`oauth:tokenCacheV2`, and `lastKnownAccountUuid`. §14 did not mention this
file at all. It matters because it is exactly the shape byte already handles:
a JSON document where byte owns a few keys and must leave every other byte
untouched.

**The app hosts Claude Code.** The desktop app's Code tab spawns real Claude
Code processes from `%APPDATA%\Claude\claude-code\<version>\claude.exe`, and
those read `~/.claude/.credentials.json` like any other Claude Code session.
So byte *already* switches the Code tab — what it does not switch is the
app's own identity and the chat side's web session.

**Three live credential stores, not two.** All three are independently
written; observed write times on a single day:

| Store | Written |
|---|---|
| `%APPDATA%\Claude\Network\Cookies` | 20:53 |
| `%APPDATA%\Claude\config.json` | 20:17 |
| `~\.claude\.credentials.json` | 19:04 |

**The two account identifiers are the same identifier space.** The desktop
app's `lastKnownAccountUuid` and Claude Code's `oauthAccount.accountUuid`
hold the same value for the same account -- measured directly on a real
signed-in machine on 2026-09-09, both reading
`4d32c114-7f8d-431a-bffb-b2448a5e8ebf`. This is load-bearing: `switch_desktop`
refuses to park the live desktop session when `lastKnownAccountUuid` names an
account other than the one byte is filing it under, which is what stops a
tray switch (which does not move the desktop half) from causing the next CLI
switch to file one account's session under another's uuid. If the two were
*different* identifier spaces, that guard would refuse the desktop half on
every switch rather than only on a genuine mismatch. Re-check this first if
desktop switching ever appears to refuse universally.

**Confidence note.** That the cookie jar is rewritten continuously while
signed in is strong evidence the web view maintains its own authenticated
session, and therefore that patching `config.json` alone would not switch the
app. It is inference from write behaviour, not a controlled experiment: the
decisive test requires quitting the app, and the environment this was
designed in runs inside it (§12). The design is safe under either outcome —
if the OAuth cache alone turns out to be sufficient, the profile swap is
redundant rather than wrong, and can be removed as an optimisation.

## 3. Decisions

1. **Full app session, chat side included.** Not just the OAuth cache.
2. **One switch, degrading honestly.** `byte switch` always switches Claude
   Code, then switches the desktop app only if it is closed; if it is
   running, byte says plainly that the app was left on the previous account.
3. **Denylist, not allowlist.** Carry everything except known junk, so state
   byte has not identified — including whatever a future app version adds —
   travels with the account instead of being silently dropped.
4. **Journalled swap** with resume or rollback.
5. **Park the old profile, leave a fresh one** when switching to an account
   with no captured profile: the app opens signed out, the user signs in, and
   byte captures. Mirrors `byte add`.
6. **Windows only.**

## 4. Three kinds of state, three treatments

The 1.28 GB of `%APPDATA%\Claude` is not one thing, and treating it as one is
the mistake §14's "rename the directory" approach would have made.

### 4.1 Directories that move

| Entry | Size | Why |
|---|---|---|
| `Network` | ~130 KB | Cookies, the web session itself |
| `Local Storage` | 15.3 MB | Web session state |
| `claude-code-sessions` | 28.6 MB | Code-tab conversation history |
| `local-agent-mode-sessions` | 3.6 MB | Agent session state |
| `IndexedDB` | 2.5 MB | Web session state |
| `Session Storage` | 0.2 MB | Web session state |
| `WebStorage`, `Shared Dictionary` | small | Web session state |

**Roughly 52 MB per stored account.**

`claude-code-sessions` is over half of that and is the one item with a
visible consequence: switching accounts hides that account's Code-tab history
until you switch back. It moves because it is account state, and decision 3
says account state travels. Called out here so the behaviour is a decision on
the record rather than a surprise.

### 4.2 Files that are patched, never moved

`config.json` mixes account state with app preferences — `locale`,
`userThemeMode`, window sizing, updater state, and MCP allowlist caches sit
in the same file as `oauth:tokenCache`, `oauth:tokenCacheV2`, and
`lastKnownAccountUuid`. Moving it would drag the user's theme and window
layout between accounts.

byte therefore owns exactly three keys in it and patches them in place,
through the existing `JsonDocument` — preserving key order, number formatting,
and every unmodelled byte, with the same backup, atomic write, and read-back
verification every other Claude Code file write already gets.

### 4.3 Entries that stay put

- **`Local State`** — holds the DPAPI-protected key that encrypts cookies.
  Shared and left in place, which is what keeps *every* parked profile's
  cookies decryptable. Moving it would break all stored profiles at once.
- **`claude_desktop_config.json`** — the user's MCP server configuration.
- **`Partitions`** (65 MB) — the account-scoped partition is already named
  `cowork-artifact-<accountUuid>-<workspace>`, so it self-segregates: distinct
  accounts get distinct directories and switching back finds the previous one
  intact. Everything else under it is `launch-preview-*` sandboxes that are
  ~90% Cache and Code Cache. Leaving the whole tree alone is correct *and*
  saves 65 MB.
- **`buddy-tokens.json`** — 68 bytes, one key `tokens-today`. An LLM usage
  counter, not credentials, despite the name.
- **`ant-device-registry.json`, `ant-did`, `Preferences`,
  `window-state.json`, `bridge-state.json`** — device and app preferences.

### 4.4 Junk, excluded

`Cache`, `Code Cache`, `GPUCache`, `DawnGraphiteCache`, `DawnWebGPUCache`,
`blob_storage`, `logs`, `Crashpad`, `sentry`, `lockfile`, and `claude-code`.

`claude-code` is 416 MB of Claude Code *binaries* — program files, not
account state. §14's whole-directory rename would have duplicated them per
account.

## 5. Storage layout

```
<byte_config_dir>/desktop/
  journal.json              # present only mid-swap (§7)
  <account-uuid>/           # one parked profile per captured account
    Network/
    Local Storage/
    claude-code-sessions/
    ...
    oauth.json              # the three config.json keys byte owns
```

`accounts.json` gains a per-account `desktop_profile` record: whether one is
captured, when, and its size on disk. Absent means never captured, which is
what drives the decision-5 behaviour.

## 6. Security: credentials on disk

§6.3 of the original design routes secrets to the OS keychain and metadata to
`accounts.json`. **This feature cannot honour that rule**, and the departure
is deliberate rather than an oversight.

Cookies are credentials. They are also ~100 KB of SQLite plus tens of
megabytes of LevelDB, and no keychain will hold them — Windows Credential
Manager's per-entry budget is roughly 1280 characters, which byte has already
hit once. The parked profile store therefore holds credential material as
plain files.

Consequently:

- `<byte_config_dir>/desktop/` is created with an ACL granting the current
  user only, and byte verifies that before writing into it.
- `oauth.json` lives in the profile directory rather than the keychain, for
  the same reason and so that one account's secrets are not split across two
  stores with different lifetimes.
- The README and `docs/configuration.md` state plainly that parked desktop
  profiles contain live session credentials on disk.

This is the one place in byte where a secret is stored outside the keychain,
and it needs to stay conspicuous.

## 7. The switch algorithm

`byte switch <name>` becomes:

1. Switch Claude Code exactly as today — unchanged, and it must not become
   slower or conditional on anything below.
2. If the desktop app is running, stop here and report that the app still
   holds the previous account. Detection reuses `is_claude_desktop_app`
   (`src/claude/detect.rs`), inverted: it currently exists to *exclude* the
   app from Claude Code session counts.
3. Capture the outgoing account's live profile into its store — always,
   before anything is moved, so a session is never lost by switching away
   from it. This is the desktop counterpart of the CLI's sync-back.
4. If the incoming account has a parked profile, move it into place. If it
   does not, leave the live location without the moved entries, so the app
   opens signed out (decision 5).
5. Patch `config.json`'s three owned keys to the incoming account's values,
   or remove them when there is no parked profile.

Every move in steps 3–5 goes through the journal.

## 8. Journal

A swap is roughly sixteen directory renames, not one atomic write, so it
cannot borrow `atomic.rs`'s guarantees. The journal supplies the equivalent.

`<byte_config_dir>/desktop/journal.json` records the whole intended sequence
of `(from, to)` moves **before** the first one runs, marks each as it
completes, and is deleted on success. Renames within a volume are effectively
instantaneous, so the exposure window is small — but the failure it guards is
severe: the app appears signed out while the user's real session sits filed
under another account's name, with nothing on screen to explain it.

**Every byte command checks for a journal at startup**, not just `switch`. A
half-completed swap must be repaired by whatever runs next, not only by a
retry of the command that failed.

Recovery is decided by stage, not by a count. A swap has two stages — park
the outgoing profile, then install the incoming one — and the journal records
which stage each move belongs to. If any *install* move completed, roll
forward and finish the install: the incoming profile is already partly in
place, and reversing would have to unpick it. If none did, the failure
happened during the park, so reverse it and leave the outgoing account live.
Either way byte reports what it repaired rather than doing it silently.

This is the same discipline as `rollback_credentials` and `atomic::write`,
and for the same reason — a safety mechanism that guards one commit point
while another can fail after committing has now recurred four times in this
codebase.

## 9. Module structure

```
src/desktop/
  paths.rs      DesktopPaths trait + RealDesktopPaths + TestDesktopPaths
  profile.rs    entry classification and the denylist (pure)
  journal.rs    journal encode/decode, plan, replay, reverse (pure)
  config.rs     the config.json patch, over JsonDocument
  swap.rs       the move itself, the only part that touches the real FS
```

Layered under `ops/`, depending only on `claude/document.rs`, `atomic.rs`,
`error.rs`, `output.rs` — matching the existing dependency direction.

`DesktopPaths` mirrors `HostPaths`: production uses the real
`%APPDATA%\Claude`, tests use a temp directory. A `CLAUDE_DESKTOP_DIR`
environment override joins the existing `CLAUDE_CONFIG_DIR` and
`BYTE_CONFIG_DIR`.

## 10. Errors

| Condition | Behaviour |
|---|---|
| Desktop app running | Claude Code switch succeeds; desktop half skipped; reported plainly, not as a failure |
| No parked profile for incoming account | Profile parked, app left signed out, reported before it happens |
| Move fails mid-swap | Journal reverses completed moves; original cause reported, not a rollback failure |
| Journal found at startup | Repaired, and what was repaired is reported |
| Profile store ACL wrong | Refuse to write; report the path |
| `config.json` unparseable | Desktop half fails; Claude Code switch already committed and is reported as succeeded |

Every new variant that can reach a user gets a row in
`docs/troubleshooting.md` in the same change — that table claims to list
every error byte can report and has drifted from the claim more than once.

## 11. Testing

The `DesktopPaths` seam makes nearly all of this testable against a synthetic
profile tree in a temp directory, including the real move logic:

- Denylist classification, including entries no rule mentions.
- `config.json` patching: the three owned keys change, every other byte
  survives a capture/apply cycle — the existing `JsonDocument` property.
- Journal encode/decode, plan generation, replay, reverse.
- **Crash mid-swap**, by interrupting between journalled steps and asserting
  the next run repairs it. This is the highest-value test in the feature.
- Round trip: park account A, switch to B, switch back, assert A's tree is
  byte-identical to what was parked.

No test may touch the real `%APPDATA%\Claude`, the real keychain, or a real
home directory.

## 12. The verification gap

Automated tests cannot answer the one question that matters most: whether
swapping *this particular set* actually switches the real app.

Worse, it cannot be answered from the environment this was designed in, which
runs inside the Claude desktop app — quitting the app to test ends the
session doing the testing. Verification requires a human, a second Claude
account, and a willingness to be signed out mid-session.

Stated here so that it is a known gap rather than a discovery. §14's original
findings were recorded confidently and turned out to be wrong on two points;
this design should not repeat that by assuming its own untested half works.

## 13. Out of scope

- **macOS.** The equivalent lives at `~/Library/Application Support/Claude`
  and the classification logic is largely shared, but macOS already carries
  an unverified tray, an unverified launcher, and two known open defects
  (#18, #19). Adding a blind 52 MB profile swap stacks unverifiable risk on
  unverifiable risk. The platform seam is built so macOS drops in once
  someone can run it.
- **Simultaneous accounts** via `--user-data-dir` (§14.2 approach B). Still
  unverified, and orthogonal to switching.
- **Reconciling a desktop/Claude Code mismatch** that a user creates by hand.

## 14. Phasing

| Phase | Deliverable |
|---|---|
| A | `DesktopPaths`, `CLAUDE_DESKTOP_DIR`, profile classification and denylist — all pure, all tested |
| B | Journal: plan, replay, reverse, startup check |
| C | `config.json` patching over `JsonDocument` |
| D | The swap, wired into `byte switch`, with process gating |
| E | Docs, troubleshooting rows, README; human verification (§12) |
