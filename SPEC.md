# podrick — v1 spec

A task tracker for the terminal, designed so that agents are the primary caller and
humans are the comfortable secondary. Binary: `pd`. Crate: `podrick`. Rust, MIT,
public repo `aram-p/podrick`.

Status: awaiting approval. No code written yet.

---

## 1. Principles

1. **Simple when used simply, powerful when pushed.** `pd add fix the thing` must work
   with zero ceremony. Everything else is opt-in.
2. **Agents first.** 90% of calls are non-interactive. That means: never block without a
   TTY, data on stdout / chatter on stderr, meaningful exit codes, `--json` everywhere.
3. **Nothing is ever destroyed.** The file is an append-only ledger. `done`, `drop`,
   `edit`, and `undo` all append; none rewrite. Only `pd compact` ever removes lines, and
   only when asked.
4. **The file is not hand-edited.** Editing happens through commands. This is what buys
   us the log-only storage model.
5. **Restraint in output.** Default view of a 10-item list fits in under 15 lines.

---

## 2. Storage

### 2.1 Format

Each task file is **JSONL — one event object per line, append-only.** State is derived by
replaying the log on every command. There is no separate state file; the log is the only
truth.

Rationale: it is the ledger and the state at once (no dual source of truth), it stays
greppable and agent-readable, it diffs cleanly, and it costs no C dependency. Replaying
tens of thousands of events is sub-millisecond, so SQLite buys nothing at this scale.

### 2.2 Locations

| What | Where |
| --- | --- |
| Per-project file | `.podrick` at the git repo root, else cwd |
| Global file | `~/.local/share/podrick/global.podrick` |
| Registry | `~/.local/share/podrick/registry.jsonl` |
| Config | `~/.config/podrick/config.toml` |
| Compaction archive | `.podrick.archive` next to the file |

### 2.3 File resolution

On every command, in order:

1. `-f <path>` — explicit file, wins over everything.
2. `-g` — the global file.
3. **Walk up** from cwd to the git root, then to `$HOME`, then stop. First `.podrick`
   found wins.
4. **Smart match** against the registry, best signal first:
   - **S1** cwd is inside a registered file's tree, or contains one.
   - **S2** cwd's git repo has the same `remote.origin.url` as a registered file.
     (Correctly unifies worktrees and second clones; correctly separates `list.am`
     from `list.am-mobile`.)
   - **S3** a registered file exists in a sibling package of the same monorepo.
   - **S4** repo basename matches a registered file's repo basename.
   - **S5** most-recently-used file.

   **S1–S2 auto-adopt**: the matched file is used, and a loud line goes to stderr naming
   it. **S3–S5 mention only**: never acted on, surfaced as a suggestion.
5. **Nothing found** → depends on the caller:
   - **TTY**: prompt `No task file for <dir>. Create one here? [Y/n]`. On yes, create at
     the git root (else cwd), print `created .podrick in <dir>`, and prompt once to add
     `.podrick` to `.gitignore` if in a git repo.
   - **Not a TTY**: **refuse.** Exit 1 with an actionable error naming `--here`,
     `-f <path>`, and `-g`. Agents must opt into file creation deliberately.

`--here` forces creation/use of a file in cwd's project, skipping smart matching.
The resolved file path is always reported as `file` in `--json` output.

Budget for resolution: up to ~500ms is acceptable (git calls, stats).

### 2.4 Concurrency

Every write takes an advisory `flock` on `.podrick.lock`, with a stale-lock timeout of
5 seconds (after which the lock is broken and a warning goes to stderr). Reads are
lock-free. This exists because concurrent agents will otherwise eventually tear a line
and a corrupt ledger is unrecoverable.

### 2.5 Compaction

`pd compact` moves the existing log to `.podrick.archive` (appending, never truncating)
and writes a fresh file beginning with a single `compacted` event carrying a full state
snapshot. **Never automatic.**

**Slowness nudge**: every command times its own replay. If replay exceeds **50 ms**, a
one-line hint goes to stderr suggesting `pd compact` (suppressed when not a TTY, and
rate-limited to once per day per file via the registry entry). The tool notices it has
gotten slow and says so; it never acts on its own.

---

## 3. Data model

A task:

| Field | Type | Notes |
| --- | --- | --- |
| `id` | string | 3-char base32, permanent, unique per file. The real identity. |
| `path` | string | Positional, e.g. `2.1.3`. Derived at render time, never stored. |
| `text` | string | |
| `state` | `open` \| `done` \| `dropped` | |
| `priority` | 1–4 \| null | p1 highest, p4 lowest, null = unset (renders as p4) |
| `due` | string \| null | `YYYY-MM-DD` or `YYYY-MM-DDTHH:MM` |
| `parent` | id \| null | |
| `created_at` / `completed_at` | timestamp | derived from the log |

**Nesting: max 4 levels.** Exceeding it is a usage error (exit 2).

**Parent semantics**: completing a parent auto-completes its open descendants, each
logged as its own event. Completing all children does *not* auto-complete the parent.

**Identity vs. addressing**: `id` is identity — it is what the ledger records and what
`--json` returns. `path` is a typing convenience accepted as input. Agents are told in
`--help` to use ids.

**Staleness is enforced by caller type, not uniformly.** Callers may pass
`--expect-seq <n>` — the log sequence they last read, emitted in every `--json` payload.
When stdin is **not a TTY**, a path-addressed write without a matching `--expect-seq`
fails with exit 3; a silently-shifted write is unrecoverable for an agent. When stdin
**is a TTY**, the path resolves against current state and the write proceeds. Id-addressed
writes are never subject to this — ids don't shift.

---

## 4. Event ledger

One JSON object per line:

```json
{"v":1,"seq":42,"ts":"2026-08-12T14:03:11+04:00","actor":"cli","ev":"completed","id":"a7f","note":"shipped in v2.1","data":{}}
```

`actor` is `"cli"` when stdin is a TTY, `"agent"` otherwise.
`note` is always optional, on every event type, never prompted for.

| Event | `data` |
| --- | --- |
| `created` | `text`, `parent`, `priority`, `due` |
| `completed` | — |
| `uncompleted` | — (from `pd reopen`) |
| `dropped` | — (decided not to do it; deliberately distinct from `completed`) |
| `edited` | `from`, `to` |
| `reprioritized` | `from`, `to` |
| `rescheduled` | `from`, `to` |
| `moved` | `from_parent`, `to_parent` |
| `noted` | — (note with no state change) |
| `compacted` | `snapshot` |

`pd undo` appends a **compensating event** of the appropriate type carrying
`"undo_of": <seq>`. History is never rewritten.

---

## 5. Command surface

```
pd                              list — default view of the resolved file
pd <word>                       substring search

pd add <text> [-p N] [-d <date>] [--under <path|id>]
pd done   <path|id> [-m <note>]
pd reopen <path|id> [-m]        mark a done task open again
pd drop   <path|id> [-m]        decided not to do it
pd undo                         revert the last event, whatever it was
pd edit   <path|id> <text>
pd pri    <path|id> p1..p4
pd due    <path|id> <date|none>
pd mv     <path|id> --under <path|id>
pd note   <path|id> -m <note>

pd list [--all] [--sort K] [-p N]
pd all                          across every registered file
pd log [<id>]                   the ledger
pd compact
pd config [sort K] [--here]     no args = print resolved config chain
pd files                        registry contents
```

16 verbs. Global flags: `-g`, `-f <path>`, `--here`, `--json`, `--no-color`,
and the hidden `--now <iso>` (see §9).

Quoting is optional — trailing positionals are joined. `--` ends flag parsing.
`-m` notes take a quoted string.

**Cut from v1, deliberately**: the interruption stack (`push`/`pop`) — it serves only the
human 10% and carries its own global state; revisit with the TUI. Recurrence — 40% of the
date complexity for 5% of tasks, and it's the thing Todoist does better because it has a
server. Tags and projects — the substring search covers them at zero data-model cost.
`pd init` — first `pd add` does everything it would have. The TUI itself — v2.

---

## 6. Sorting

Resolution order, highest wins:

```
--sort flag  >  per-project setting  >  global setting  >  insertion order
```

Global setting lives in `config.toml`; per-project lives in the **registry entry**, not in
the event log — config is not history. Keys: `priority`, `due`, `created`, `alpha`.

Sorting reorders **siblings within the tree** and never flattens it. `pd config` with no
arguments prints the resolved chain, so the ordering is never a mystery.

---

## 7. JSON contract

`--json` is available on **every** command, including writes — `pd add --json` returns the
created task's id, so an agent never has to re-list to find what it just made.

**The JSON shape is a stable API from v1.** Every payload carries `file` (the resolved
path) and, on error, `error` and `hint`.

`pd --help --json` emits a machine-readable schema of the entire command surface —
every command, flag, argument, and return shape in one call. This is the bet that a
well-behaved CLI beats an MCP server: one call and the agent has the whole API.

All human-facing help text carries a concrete example per command and states what
`--json` returns.

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | success |
| 1 | not found (no task file, no such task) |
| 2 | usage error (bad flag, nesting too deep) |
| 3 | conflict (stale path, lock contention) |
| 4 | I/O error |

Data goes to **stdout**, all chatter to **stderr** — `pd list --json \| jq` is never
polluted by a "created .podrick" note.

---

## 8. Dates

**Date + time.** No recurrence, no deadline-vs-due distinction.

Accepted forms: `fri`, `friday`, `friday 3pm`, `tomorrow`, `tomorrow 09:00`, `next tue`,
`in 3 days`, `dec 25`, `25/12`, `2026-12-25`, `2026-12-25 15:00`, `none`.

Two rules, both explicitly tested:
- **`fri` means today** if today is Friday.
- **A bare time already past means tomorrow** (`3pm` at 16:00 → tomorrow 15:00).

Nothing notifies you — there's no daemon. Overdue is surfaced only when you run `pd`.

---

## 9. Testing

Date handling is where the bugs will live, so:

- **Injectable clock**: hidden `--now <iso>` flag and `PODRICK_NOW` env var. Non-negotiable
  — without it, tests fail at midnight and on the last day of the month.
- **Table-driven parser tests** over every accepted form above, plus rejections.
- **DST and month/year boundaries pinned explicitly.**
- **Property tests** for parse → render → parse round-trip stability.
- **Golden tests** for rendered output, both TTY and piped.
- **Ledger replay tests**: a log of N events produces exactly the expected state; `undo`
  of each event type restores the prior state.
- **Concurrency test**: N processes appending simultaneously produce N well-formed lines.

---

## 10. Rendering

Near-monochrome, one accent color. What Linear would ship:

- No boxes, no table borders, no connector lines.
- `○` open, `●` done, `⊘` dropped.
- Priority as a colored bar `▏` in the left gutter — never the literal text `p1`.
- Ids dimmed at the **right** edge.
- Dates relative and dim (`in 2d`, `3d ago`), red only when overdue.
- Subtasks indented two spaces per level.
- Generous left padding, a blank line between groups, nothing bold except the accent.
- **Everything above vanishes** when stdout is not a TTY or `NO_COLOR` is set — plain,
  parseable lines.

---

## 11. Build & distribution

Public repo, so all three install paths are live:

```
cargo install podrick                                   # crates.io
brew install aram-p/tap/podrick                         # Homebrew tap
curl -fsSL https://podrick.sh/install | sh              # prebuilt binary
```

Release CI (GitHub Actions, tag-triggered) cross-compiles for
`aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, and
`aarch64-unknown-linux-gnu`, attaches stripped binaries to the release, and publishes to
crates.io. The tap formula and the install script both read from those release assets.

Binary size target: under 2 MB stripped, with `opt-level="z"`, LTO, `codegen-units=1`, and
`panic="abort"`.

Shell completions (`pd completions <shell>`) for zsh, bash, and fish, generated by
`clap_complete`.

---

## 12. v2 backlog

TUI (full editing, since the CLI is the only editor in v1) · the interruption stack ·
recurrence, if it's ever genuinely missed · optional MCP wrapper over the stable JSON
contract.
