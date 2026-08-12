# Changelog

[Semantic versioning](https://semver.org). While the version is `0.x`, breaking changes
bump the **minor** position and everything else bumps the **patch** position — see
[SPEC.md §13](SPEC.md#13-versioning).

Changes to the three versioned contracts — JSON payloads, exit codes, and the log format —
are always listed under their own heading, because those are the ones that can break a
caller. A log written by any older `pd` must always still replay.

## Unreleased

### Added

- `pd list --all` gained the short form **`-a`**. Because `list` is the implied command,
  argv normalisation now inserts it at the front rather than in front of the first
  positional, so `pd -a` and `pd -a <filter>` work as well as `pd list -a` — `list`'s own
  flags are not global, and clap will not accept them ahead of a subcommand. `pd --help`
  and `pd --version` are left alone.

### Changed

- A task file found through discovery is announced only when the command is about to
  **write** to it. Reads no longer print `using <path> (same directory tree)` on every
  invocation — the notice existed to stop a write landing somewhere surprising, and a read
  cannot do that.
- Notices are styled for terminals: an indented `↳` in the accent colour, with the project
  directory shortened (`~/Dev/podrick` rather than the full path to the dotfile). Piped and
  `NO_COLOR` output keeps the greppable `pd: ` prefix unchanged.

## 0.1.0

First release. Append-only JSONL ledger, priorities `p1`–`p4`, subtasks to four levels,
natural-language dates with times, per-directory task files with registry-backed discovery,
`--json` on every command, and `pd --help --json` for the full command schema.
