# podrick

A terminal task tracker with a perfect memory, built for agents first.

```
  ▏ ○ fix the flaky test         in 2d      qdb
      ○ pin the seed                        zu7
      ○ check CI on the runner              nn3

  ▏ ○ write the release notes    in 18h     ffu
    ○ ship the migration         3d ago     hwx
    ○ rewrite the intro                     e4y

  6 open · 1 done
```

Every change appends to an append-only ledger, so nothing is ever lost and any task's
full history can be replayed — including the notes you left when you closed it.

```console
$ pd log a7f
   1 created        fix the flaky test    · you · 3d ago
  11 completed      fix the flaky test    · you · 2d ago
       "finally green"
  12 uncompleted    fix the flaky test    · agent · 1d ago
```

## Install

**Homebrew**, on macOS or Linux:

```sh
brew install aram-p/tap/podrick
```

**A prebuilt binary**, no toolchain required — macOS and Linux, arm64 and x86-64:

```sh
curl -sSL https://github.com/aram-p/podrick/releases/latest/download/pd-aarch64-apple-darwin.tar.gz | tar xz
sudo install -m 755 pd /usr/local/bin/pd
```

Swap the filename for `pd-x86_64-apple-darwin`, `pd-x86_64-unknown-linux-gnu`, or
`pd-aarch64-unknown-linux-gnu`. The binaries are unsigned, so macOS will quarantine one
fetched by a browser — `xattr -d com.apple.quarantine pd` clears it. Checksums are on the
[release page](https://github.com/aram-p/podrick/releases/latest).

**From source**, needing Rust 1.85 or newer ([rustup](https://rustup.rs)):

```sh
cargo install --git https://github.com/aram-p/podrick
```

That puts `pd` in `~/.cargo/bin`, which has to be on your `PATH` — rustup normally adds it,
but not if it was installed with `--no-modify-path`:

```sh
command -v pd || export PATH="$HOME/.cargo/bin:$PATH"   # and add it to your shell rc
pd --version
```

Not on crates.io or in homebrew-core yet — core wants a project with rather more stars
than this one has.

## Use

```sh
pd add fix the flaky test -p1 -d fri   # quotes optional; dates in plain language
pd                                     # the list
pd done 2 -m "pinned the seed"         # notes are optional, and permanent
pd undo                                # revert the last action, whatever it was
pd flaky                               # substring search
```

Tasks live in a `.podrick` file at the root of whatever git repo you are in, so each
project has its own list. `-g` uses a global one. `pd all` shows every list at once.

| | |
| --- | --- |
| `pd add <text>` | `-p p1..p4`, `-d <when>`, `--under <path\|id>` |
| `pd done` / `reopen` / `drop` | `-m <note>` on any of them |
| `pd undo` | reverts the last action, cascades included |
| `pd edit` / `pri` / `due` / `mv` / `note` | change one field |
| `pd list` | `-a/--all`, `--sort priority\|due\|created\|alpha`, `-p` |
| `pd batch` | many changes, one undo — ops as JSONL on stdin |
| `pd all` / `pd files` | across every known list |
| `pd log [id]` | the ledger |
| `pd compact` | archive the log, keep the state |
| `pd config sort due [--project]` | global, or this project only |

Dates are ordinary English: `fri`, `tomorrow 9am`, `next tue`, `in 3 days`, `dec 25`,
`2026-12-25 15:00`. A weekday means *today* if today is that weekday, and a bare time
that has already passed means tomorrow. There is no recurrence, on purpose.

## For agents

`pd` is designed to be driven by a script or an agent, so it behaves itself:

- **`--json` on every command**, including writes — `pd add --json` returns the new
  task's id, so there is no need to re-list to find it.
- **`pd --help --json`** emits a machine-readable schema of the entire command surface in
  one call. Start there.
- **Data on stdout, everything else on stderr.** `pd list --json | jq` is never polluted.
- **Exit codes**: `0` ok · `1` not found · `2` usage · `3` conflict · `4` I/O.
- **No prompt ever fires without a TTY.** Where a human would be asked to confirm, a
  script gets an error naming the flag that would have said yes.
- **`pd batch` for sweeps.** Restating forty tasks should leave one entry in the history,
  not forty, so that one `pd undo` puts them all back:

  ```sh
  pd list --json | jq -c '.tasks[] | {op:"edit", id, text:(.text|ascii_upcase[0:1]+.[1:])}' | pd batch
  ```

  Nothing is appended unless every operation validates. See `pd batch --help`.
- **`id` is permanent, `path` is positional.** `2.1` is convenient to type but shifts
  whenever the tree changes, so a path-addressed write from a non-interactive caller is
  refused unless it passes `--expect-seq <n>` matching the seq it last read. Address
  tasks by `id` and this never comes up.

## Storage

One file, `.podrick`, holding one JSON event per line:

```json
{"v":1,"seq":11,"ts":"2026-08-12T14:03:11+04:00","actor":"cli","ev":"completed","id":"a7f","note":"finally green","batch":11}
```

State is replayed from the log on every command, which takes well under a millisecond for
any realistic list. Nothing is ever rewritten or deleted — `pd compact` is the only
command that shortens the file, it never runs on its own, and it moves the old log to
`.podrick.archive` rather than dropping it.

The file is not meant to be hand-edited; use the commands, and the ledger stays honest.

Full design rationale, including what was deliberately left out, is in [SPEC.md](SPEC.md).

## License

MIT
