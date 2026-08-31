# Changelog

[Semantic versioning](https://semver.org). While the version is `0.x`, breaking changes
bump the **minor** position and everything else bumps the **patch** position — see
[SPEC.md §13](SPEC.md#13-versioning).

Changes to the three versioned contracts — JSON payloads, exit codes, and the log format —
are always listed under their own heading, because those are the ones that can break a
caller. A log written by any older `pd` must always still replay.

## 0.2.1

### Added

- **`pd who <task> <person>...`** links people to a task, and `--remove` unlinks them.
  Repeatable, and idempotent in both directions — linking someone twice appends nothing
  and is not an error — so an agent can restate a task's people without first reading
  them. A link is untyped: it means only that the person is involved.
- **`pd people`** prints the roster, each person with how many of their tasks are open and
  how many are done, busiest first and ties alphabetical. It is derived from the links
  rather than maintained, so a person exists exactly as long as some task still names
  them, and there is no separate list to go stale. Dropped tasks count towards neither
  total.
- **`pd people rename <old> <new>`** renames a person everywhere at once, and
  **`pd people remove <person>`** unlinks them from every task as one undoable action.
  Renaming onto a name that already exists merges the two.
- **`pd list --person <who>`** filters the tree to that person's tasks, keeping a
  non-matching parent as context exactly as the text and priority filters do.
- **`{"op":"who", …}` in `pd batch`**, with `"remove": true` to unlink — restating who is
  on forty tasks is the sweep a batch exists for.
- People render as a dim `@name` suffix beside the task's title, and `pd log` names them
  on the `linked` / `unlinked` lines and both sides of a rename.

A name is matched on a folded key — trimmed, inner runs of whitespace collapsed,
lower-cased — so `Meri`, `meri` and `MERI` are one person and any spelling finds them. Two
spellings of one person would split the counts, and a rename would then only fix half of
them.

It is **shown** in the capitalisation it was given, which is stored rather than derived:
title-casing at render time mangles `McDonald`, `de Vries` and `O'Brien`. The first link
to a person pins the spelling — a later `pd who a7f mcdonald` finds the same person and
leaves `McDonald` alone — and `pd people rename` is how a spelling is changed, including
when only the case moves (`pd people rename meri Meri` is a real change, not a no-op). A
name nobody ever capitalised is capitalised word by word at the edge, so `meri` reads back
as `Meri`.

### Log format

- Three new event types: **`linked`** and **`unlinked`** (`data.person`, the folded key;
  `data.person_display`, the spelling as typed; and a reserved `data.role` that nothing
  writes yet — a typed link can arrive later without a format break, and every reader
  already tolerates it being present), and **`renamed`**, which carries `data.from` /
  `data.to` and no task id at all. Replay resolves a rename against the links already in
  the log, so history is never rewritten to make them follow.
- A `renamed` event's two names are **display forms**, folded by replay. That is what lets
  a rename that changes only the capitalisation mean something, and what lets `pd undo`
  restore the old spelling by the same from/to swap it uses for an `edited`.
- `compacted` snapshots carry each task's `people` (keys) and a `data.people` map of key
  to display form, holding only the people some task still names. Both default to empty,
  so a snapshot written by an older `pd` still replays — its names simply fall back to the
  capitalisation default.
- A log written before any of this replays unchanged: every name in it is folded, absent
  display forms leave the map empty, and each name is capitalised at the edge.
- All three are undoable: `linked` and `unlinked` invert each other, and `renamed`
  inverts by swapping its two names.

### JSON payloads

- Every task payload gains **`people`**, an array of display forms (not the folded keys —
  a caller echoing a name to a human should not have to un-fold it; any spelling is still
  accepted back as input) — `pd list --json`,
  `pd add --json`, and every write that returns a task.
- `pd who --json` returns `task`, `changed` (the people it actually acted on, empty when
  the call was a no-op) and `action`.
- `pd people --json` returns `people`, each with `name` (the display form), `open` and
  `done`. Ties in the ordering break on the folded key, so a capital letter never moves
  anybody.
- `pd people rename --json` returns `from` / `to` as display forms, and
  `pd people remove --json` returns `removed` as one.
- Piped (non-TTY) list output gains a sixth tab-separated column, the comma-separated
  display forms. It is appended, so the five before it are unmoved.

## 0.2.0

### Changed

- **`pd list` shows done tasks by default.** They render as a flat block under the open
  tree, the same place `--all` has always put them. A tracker that hides its own evidence
  of progress is dispiriting to look at, and "what did I finish today" gets asked about as
  often as "what is left". Dropped tasks are a different claim — *decided against this* —
  and still wait behind `-a/--all`.
- **`-a/--all` now means "dropped too".** Its old effect — adding the done tasks — is the
  default, so in practice it only changes what you see if something was dropped.

### Added

- **`-o/--open` on `pd list`**, for the old open-only view. Conflicts with `--all`.

### JSON payloads

- `pd list --json` returns the done tasks too, unless `--open` is passed. A caller that
  wants only open tasks should pass `--open` or filter on `state`.

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
