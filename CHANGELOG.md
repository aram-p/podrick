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

### Fixed

- **The registry lost entries under concurrency.** Registering a task file was a
  load-modify-save with no lock, so simultaneous first writes in different projects
  overwrote each other's registrations. Eight concurrent `pd --here add` in eight repos
  left **one** registration of eight; it is now eight of eight. `save` re-reads inside the
  lock, keeps entries it did not know about, and renames a temp file into place.
- **A parent link could close a loop at replay**, hanging or blowing the stack on a log
  that a well-behaved `pd` would never write but a corrupted or hand-forged one could.
  Both `moved` and two `created` events naming each other are now refused at replay, and
  the tree walks carry visited sets instead of relying on a depth cutoff.
- **Sorting could reparent a child.** It ran over the flattened rows after the tree walk,
  so a subtree could be split from its root. Sorting now happens inside the walk, where
  siblings are siblings by construction.
- **`new_id` could return a colliding id** when it widened on collision without
  re-checking the wider candidate.

### Changed

- **`pd config <key> <value> --here` is now `--project`.** `--here` is a global flag
  meaning "skip discovery and use this directory's own file", and one token cannot carry
  both meanings — spelling them the same made `pd config sort … --here` impossible in any
  directory whose file came from discovery. **Breaking**, but pre-release: no tagged
  version ever shipped the old spelling.
- An unknown `--sort` key is now a usage error (exit 2) naming the valid keys, rather than
  being silently ignored. The same applies to an invalid `sort` in `config.toml`; an
  invalid one in the registry still degrades to unset, since being strict there would
  discard a whole entry over one bad field.
- `pd all` renders each file in that file's own configured sort, so it can no longer
  disagree with `pd list` about the same project.
- Priority-filtered output is flat rather than nested — a parent that does not match the
  filter is no longer shown as scaffolding. The `path` column still says where each task
  lives.
- The task file's `.gitignore` entry is the glob `.podrick*`, so the lock and the archive
  are covered too rather than sitting in `git status` forever.
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
