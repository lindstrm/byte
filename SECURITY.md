# Security policy for byte

## Supported versions

The latest release line on `main` receives security fixes. Older lines are
considered end-of-life.

## Reporting a vulnerability

**Do not open public GitHub issues for security problems.**

Instead, please report privately via [GitHub Security Advisories](https://github.com/lindstrm/byte/security/advisories/new),
or by email to `jocke@indicio.com`.

## Response

We aim to acknowledge reports within 72 hours and provide a triage update
within 7 days.

## Disclosure

We follow coordinated disclosure: we will agree on a release window with the
reporter and credit them in the release notes (unless they request otherwise).

## Scope

In scope: any vulnerability in the published release of byte.
Out of scope: vulnerabilities in third-party dependencies (please report those
upstream).

## Threat model

byte stores each account's OAuth refresh token in the OS credential store
(Windows Credential Manager, the macOS login Keychain, or a Linux Secret
Service provider) so it can switch accounts without repeating a browser
login. These are live credentials, and the honest framing is this: **anyone
who can run code as the logged-in user can read them.**

That is not a new exposure byte introduces for the credential store copy
specifically: it is equally true today of Claude Code's own credential file,
`~/.claude/.credentials.json`, which is the source byte copies those tokens
from in the first place. But byte holds live credentials in five places, not
one, and only the third has the OS credential store's protection:

1. `~/.claude/.credentials.json` — Claude Code's own file.
2. `%APPDATA%\Claude\config.json` — the Claude desktop app's own file,
   holding its `oauth:tokenCache` and `oauth:tokenCacheV2`. Listed for the
   same reason as location 1: it is not byte's doing, it exists whether or
   not byte is installed, and it is the source byte copies from to produce
   location 5. A complete inventory of where a live desktop-session token
   rests on disk has to name it.
3. The OS credential store — one entry per account byte has stored, under
   the service name `byte-claude-account-switcher`.
4. `<byte-config-dir>/backups/` — a **plaintext file**, refresh token
   included, written before every capture, switch, and add (see
   [Configuration](docs/configuration.md)). The ten most recent generations
   per file are kept, so more than one past refresh token can be recovered
   from here even after it has been rotated or the account removed.
5. `<byte-config-dir>/desktop/<account-uuid>/` — a parked Claude desktop
   session, present only if you have used desktop switching. It holds that
   account's cookie jar, local storage, and an `oauth.json` capture of the
   app's own OAuth token cache. **These are live session credentials as
   ordinary files.** They are not in the credential store, and they cannot
   be: a cookie jar is tens of megabytes of SQLite and LevelDB, far past
   what any OS credential store will hold. byte creates the directory
   owner-only on Unix (mode `0700`); on Windows it inherits the config
   directory's permissions rather than getting an explicit owner-only ACL,
   because setting one needs a Win32 dependency byte does not carry.

Locations 1, 2 and 3 are on equal footing: they rely on the same OS-level
protections, and none adds encryption beyond what the platform already
provides for a logged-in user's own data — this is the "byte does not
worsen that posture" claim, and it is true of those three. Locations 4 and
5 are not on that footing: they are ordinary files with ordinary filesystem
permissions and none of the OS credential store's access control. Location
4 additionally holds up to ten generations of history rather than one live
copy; location 5 holds one live copy per account, but of a *session* rather
than a refresh token, so revoking it means signing that account out of
Claude rather than rotating a token.

A local attacker able to run code as you can read live credentials from any
of the five. Locations 1 and 2 exist with or without byte; 3, 4 and 5 are
byte's. The backups directory is the easiest to overlook; the desktop
profile store is the largest, and the only one whose contents byte cannot
put in a keychain even in principle.

If your threat model includes a local attacker able to run arbitrary code as
you, the correct response to a suspected compromise is the same regardless
of which copy was read: revoke the affected account's session from your
claude.ai account settings. `byte remove <name>` deletes byte's copy of the
credential from the OS credential store and its metadata entry, but it does
**not** clear that account's parked desktop profile in `desktop/`, nor its
past backups in `backups/` — those age out only
through the normal ten-generation pruning — and neither `byte remove` nor
deleting `~/.claude/.credentials.json` revokes the token itself — only
Anthropic's auth servers can do that.

`accounts.json`, byte's own metadata file, holds each stored account's
profile: email, organization name, billing type, organization role,
subscription tier, the associated Claude Code `userID`, and the
added/last-used timestamps — in effect, the non-secret `oauthAccount` object
Claude Code stores per account, which byte copies in verbatim and opaque: it
does not parse, type, or filter that object's fields, the same way it does
not parse `claudeAiOauth` (see [Architecture](docs/architecture.md)). So the
claim below is conditional, not something byte enforces by inspecting
content: **byte itself never *writes* a credential into `accounts.json`**,
because `accessToken`/`refreshToken` live only in `claudeAiOauth` and byte's
own code never copies that object's fields into `oauthAccount`'s. It is not
a claim that byte would notice or filter one out if Anthropic ever put a
credential-shaped value inside `oauthAccount` itself — byte has no way to
tell a credential apart from any other string in an object it treats as
opaque. As things stand today, `oauthAccount` is profile and identity data,
`accounts.json` never contains a token, and the credential store and the
`backups/` directory do — so reading `accounts.json` alone (e.g. its
contents ending up in a support bundle or backup) does not expose account
credentials. That distinction matters more now than when `accounts.json`
held only a handful of display fields, precisely because its inventory has
grown to the full profile; the boundary that keeps it secret-free has not
moved, but there is more non-secret data on the wrong side of a misreading
of it than there used to be.
