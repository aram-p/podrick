# Changelog

[Semantic versioning](https://semver.org). While the version is `0.x`, breaking changes
bump the **minor** position and everything else bumps the **patch** position — see
[SPEC.md §13](SPEC.md#13-versioning).

Changes to the three versioned contracts — JSON payloads, exit codes, and the log format —
are always listed under their own heading, because those are the ones that can break a
caller. A log written by any older `pd` must always still replay.

## 0.1.2

### Fixed

- **A value-taking global flag swallowed the command after it.** `pd --file x add hi`
  searched for `"add hi"` and added nothing. Argv normalisation asks clap which flags
  consume the argument after them so its scan can skip their values — but it asked an
  unbuilt `Command`, and clap fills `num_args` in from each argument's action during the
  build, leaving it `None` before that. The answer came back "nothing takes a value", so
  `--file`'s value looked like the subcommand position and an implied `list` went in ahead
  of the real command. Affects `--file`/`-f`, `--now` and `--expect-seq`, and turned a
  write into a silent no-op read rather than an error.
- **The list ignored the width of the terminal.** The layout was a fixed 81 columns —
  a 58-wide text column plus 23 of chrome — so anything narrower wrapped the ids onto
  their own lines, and a title longer than 58 pushed its id rightwards until the ids
  stopped forming a column at all. The text column is now whatever is left after the
  gutter, the dates and the ids, and a title too long for it wraps at a word boundary and
  hangs under the text, with the date and id staying on the task's first line.
- **A list with no dates in it reserved eleven columns for a date column.** That was most
  of what pushed a narrow terminal over the edge. The column is drawn only when some task
  in the list actually has a date, and is only as wide as the longest one.
- The text column's cap rose from 58 to 72, so ordinary long titles stop wrapping on a
  wide terminal. `PODRICK_COLUMNS` overrides the detected width, as `PODRICK_NOW`
  overrides the clock.

## 0.1.1

### Added

- **`pd batch`** — many changes as one undoable action. Reads `{"op": …}` objects from
  stdin, one per line or as a single JSON array, and appends every resulting event under
  one batch id, so `pd undo` reverts the whole sweep rather than its last line. Covers
  every single-task verb except `add`, which is excluded because `created` has no
  compensating event. Operations address tasks by `id` only, nothing is written unless all
  of them validate, and each is validated against the state its predecessors leave behind
  — so closing a parent and then reopening a subtask it cascaded to does what it reads
  like. An operation that would change nothing is skipped rather than refused.
- **JSON contract**: each command in `pd --help --json` gained a `details` field carrying
  its long help, where that says more than the one-line `about`. The schema claims to be
  the whole API, and a command like `batch` whose contract is an input format was not
  describable in one line. Additive; existing fields are unchanged.

### Changed

- `pd undo` says "and 3 change(s)" for a batch of unrelated edits and keeps "subtask(s)"
  for a cascade, rather than calling every grouped event a subtask.

## 0.1.0

First release. Append-only JSONL ledger, priorities `p1`–`p4`, subtasks to four levels,
natural-language dates with times, per-directory task files with registry-backed discovery,
`--json` on every command, and `pd --help --json` for the full command schema.

The rest of this section is development history — work done between the initial commit and
the tag, so no released version ever had the old behaviour. It is kept because the
reasoning is worth having.

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
- **The registry could still lose an entry**, less often, after the locking fix above.
  `read` prunes entries whose task file is missing, and the file was registered *before*
  it was created — so a concurrent `pd` reading the registry in that window pruned the
  new entry and wrote the registry back without it. A file created by `--here` is now
  created on disk at the moment it is announced, before it is registered. The test for
  this failed roughly one run in five; it now passes twenty in twenty.
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
- Priority filtering happens inside the tree walk, alongside the text filter, so a match
  keeps its real parent as context instead of being re-indented under whatever row sorted
  above it. The two filters now behave identically and compose: `pd list -p1 parser`
  requires both. A parent kept as context does not drag its other children in.
- An empty result names what was actually asked for — `nothing at p2`, `nothing matching
  "parser" at p1` — rather than reporting `nothing open` when there is plenty open.
- The task file's `.gitignore` entry is the glob `.podrick*`, so the lock and the archive
  are covered too rather than sitting in `git status` forever.
- A task file found through discovery is announced only when the command is about to
  **write** to it. Reads no longer print `using <path> (same directory tree)` on every
  invocation — the notice existed to stop a write landing somewhere surprising, and a read
  cannot do that.
- Notices are styled for terminals: an indented `↳` in the accent colour, with the project
  directory shortened (`~/Dev/podrick` rather than the full path to the dotfile). Piped and
  `NO_COLOR` output keeps the greppable `pd: ` prefix unchanged.
