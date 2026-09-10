# Agent guidance for byte

This file is the canonical source of truth for AI coding agents working in this
repo. `CLAUDE.md`, `.cursorrules`, `.windsurfrules`, `GEMINI.md`,
`.aider.conf.md`, and `.github/copilot-instructions.md` are symlinks to this
file.

## OSS Spec conformance

This repository adheres to [`OSS_SPEC.md`](OSS_SPEC.md), a prescriptive
specification for open source project layout, documentation, automation, and
governance. A copy of the spec lives at the repository root so contributors and
AI agents can consult it without leaving the repo; its version is recorded in
the YAML front matter at the top of the file.

Run `oss-spec validate .` to verify conformance. When in doubt about a layout,
naming, or workflow decision, consult the relevant section of `OSS_SPEC.md` —
it is the source of truth for the conventions this repo follows.

## Build and test commands

```sh
make build         # developer build
make test          # full test suite
make lint          # zero-warning linter
make fmt           # format in place
make fmt-check     # verify formatting (CI)
```

## Commit and PR conventions

- All commits follow [Conventional Commits](https://www.conventionalcommits.org/).
- PRs are squash-merged; the **PR title** becomes the single commit on `main`,
  so it must follow conventional-commit format.
- Breaking changes use `<type>!:` or a `BREAKING CHANGE:` footer.

## Architecture summary

byte moves a small `AccountSnapshot` (one account's OAuth credentials plus
its identity) between Claude Code's live config files and a per-account
store, touching only the specific keys it owns and leaving everything else
in those files byte-for-byte untouched.

Four layers, each depending only on the ones below it. `cli/` (argument
parsing, `--json`, rendering — no file I/O or business logic of its own) and
`tray/` (the Windows/macOS tray app) sit together at the top as byte's two
front ends →
`ops/` (switch, add, manage, desktop — the operations, generic over the
`HostPaths` and `SecretStore` traits rather than their concrete
implementations) →
`claude/` (reads and patches Claude Code's two files), `store/` (account
metadata and secrets), and `desktop/` (Windows only: switches the Claude
*desktop* app's own session — a separate app with its own OAuth cache and
web session — alongside Claude Code's, via `ops/desktop.rs`) →
`error.rs` / `output.rs` / `paths.rs` / `atomic.rs` /
`lock.rs` / `autostart.rs` (primitives used from every layer above). Nothing in a lower layer imports
from a higher one; production code instantiates
`Switcher<&RealPaths, KeyringStore>`, tests instantiate
`Switcher<&TestPaths, MemoryStore>` — the same generic types, no keychain or
real Claude Code installation required.

See [`docs/architecture.md`](docs/architecture.md) for the full module map,
major data types, and an end-to-end command trace.

## Where new code goes

| Change type | Goes in |
|---|---|
| New feature | `src/...` |
| Tests       | `tests/...` |
| Docs update | `docs/...` |
| Examples    | `examples/...` |
| LLM prompt  | `prompts/<name>/<major>_<minor>_<patch>.md` (see `prompts/README.md`) |

## Test conventions

- **All tests live in separate files** — never inline in source files (no `#[cfg(test)]` blocks, no `if __name__ == "__main__"` test harnesses). This keeps source files free of test scaffolding and lets agents, hooks, and linters treat source and test code differently.
- Test files are named with a `_test` or `_tests` suffix (e.g. `check_test.rs`, `utils_test.py`). The stem must match the pattern `_?[Tt]ests?$` per §20 of `OSS_SPEC.md`.
- Tests live in `tests/`. Use `tempfile` or equivalent for any test that writes to the filesystem.
- **Assert identity, not cardinality.** `assert_eq!(remaining.len(), 3)`
  passes whichever three items happen to survive — including the three a
  reversed sort would keep. When a test is about *which* items survive (or
  which one changed), assert that directly, not just how many there are.
- **Pin the error variant, not `.is_err()`.** `assert!(result.is_err())`
  passes for any failure, including the wrong one for the wrong reason. Use
  `assert!(matches!(err, Error::TheSpecificVariant { .. }))` so the test
  fails if the code starts erroring for a different cause than the one it's
  named for.

## Source file size

- Non-test source files must stay under **1000 physical lines** (§20.5 of `OSS_SPEC.md`). When a file grows past the limit, prefer splitting by concern (extracting submodules, helpers, or sibling files) over relaxing the cap.
- A file may opt out by placing `oss-spec:allow-large-file: <reason>` in any comment within its first 20 lines. The reason must be non-empty and motivate why the file genuinely cannot be split (generated code, cohesive state machine, third-party snapshot, inherently dense rule catalogue).

## Documentation sync points

When you change… | Update…
--- | ---
public API | `docs/`, `README.md` Quick start
CLI flags  | `man/<cmd>.md`, `README.md`
config keys| `docs/configuration.md`

## Parity / cross-cutting rules

- Every write to a Claude Code file goes through `atomic.rs` (temp file in
  the same directory, fsync, atomic rename), preceded by a timestamped
  backup and followed by a read-back verification, regardless of which op
  triggers it. Housekeeping (backup pruning) must never turn an
  already-verified, already-committed write into an `Err` — a safety
  mechanism guarding one commit point while another can fail after
  committing has recurred four times in this codebase: see
  `atomic::prune`'s doc comment, and `ops::desktop::record_parked_profile`,
  which reports a failure to record the `desktop_profile` bookkeeping entry
  through `output::warn` rather than erroring a desktop switch whose
  directory renames and `config.json` patch have already committed.
  `desktop::journal::Journal` is the same discipline applied one level up,
  for a multi-step swap rather than a single write: the whole planned
  sequence is recorded before the first step runs, so a crash partway
  through is repaired by whatever byte command runs next.
- `JsonDocument` never deserializes a Claude Code file into a typed struct —
  every field byte does not explicitly model must survive a capture/apply
  cycle untouched.
- All user-facing text goes through `output.rs`'s helpers (`status`, `warn`,
  `info`, `header`, `error`, `confirm` on stderr; `data` on stdout). No other
  module calls `println!`/`eprintln!` directly — this is what keeps `--json`
  output pipeable.
- A single `Error` enum (`error.rs`) covers every fallible path in the
  crate. When you add a variant that can reach a user, add its row to
  [`docs/troubleshooting.md`](docs/troubleshooting.md) in the same change —
  that table claims to list every error byte can report, and has drifted
  from that claim more than once.
- `lock::MutationGuard::acquire`/`try_acquire` must be bound to a named
  variable (`let _guard = ...`), never to `_`. `let _ = MutationGuard::acquire(..)`
  drops the guard — and releases the lock — immediately, before the command
  it was meant to protect ever runs, silently defeating the whole mechanism.
- Only mutating commands (`switch`, `capture`, `add`, `remove`, `rename`)
  take `MutationGuard`. `list`, `current`, and `autostart` must not: every
  file byte writes is replaced atomically, so a concurrent read is always
  safe, and locking a read would make a running tray's momentary write
  block something harmless.
- The tray is Windows/macOS only, gated entirely inside `tray/mod.rs`
  (`pub use app::run` there vs. an `unsupported::run` with the same
  signature elsewhere). Call `tray::run(paths)` unconditionally from
  anywhere else in the crate — no `cfg` at the call site — and add any new
  platform-specific tray behavior inside that module, not around it.
- `tray/app.rs`'s `Wake::TrayClick` arm must stay inert (no `rebuild()`, no
  menu mutation of any kind): `TrayIconEvent::send` fires *before* the OS
  shows byte's popup menu, so anything reacting there would `DestroyMenu` a
  popup the OS is actively displaying, out from under the user's own click.

## Maintenance skills

Per §21 of `OSS_SPEC.md`, this repo ships agent skills for keeping drift-prone artifacts in sync with their sources of truth. Skills live under `.agent/skills/<name>/` and are also accessible via the `.claude/skills` symlink.

| Skill | When to run |
|---|---|
| `maintenance`    | When several artifacts have likely drifted at once — umbrella skill that runs every `update-*` skill in the correct order. |
| `update-docs`    | After any change to the public API, configuration keys, or error messages. |
| `update-readme`  | After any change that alters user-visible behavior, commands, or install instructions. |
| `update-prompts` | After any change to an LLM prompt's source of truth (embedded docs, rendering-context keys, JSON-schema enums, validation rules). |

Each skill has a `SKILL.md` (the playbook) and a `.last-updated` file (the baseline commit hash). Run a skill by loading its `SKILL.md` and following the discovery process and update checklist. The skill rewrites `.last-updated` at the end of a successful run, and improves itself in place when it discovers new mapping entries. The `maintenance` skill owns a **Registry** table listing every `update-*` skill — add a row whenever you create a new sync skill.