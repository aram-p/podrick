//! The command surface. Help text is written for agents: every command carries a
//! concrete example and states what `--json` returns.

use clap::{Parser, Subcommand};
use clap_complete::Shell;

#[derive(Parser, Debug)]
#[command(
    name = "pd",
    version,
    about = "A terminal task tracker with a perfect memory",
    long_about = "podrick — a terminal task tracker with a perfect memory.\n\n\
        Every change appends to an append-only ledger, so nothing is ever lost and the \
        full history of a task can be replayed. Designed to be driven by agents: pass \
        --json to any command for machine-readable output, and see `pd --help --json` \
        for a schema of the entire command surface.\n\n\
        EXIT CODES\n  \
        0  success\n  \
        1  not found (no task file, or no such task)\n  \
        2  usage error (bad flag, nesting deeper than 4)\n  \
        3  conflict (stale path, see --expect-seq)\n  \
        4  I/O error\n\n\
        Data goes to stdout; all other output goes to stderr, so `pd list --json | jq` \
        is never polluted.",
    after_help = "EXAMPLES\n  \
        pd add fix the flaky test -p1 -d fri\n  \
        pd done 2 -m \"fixed by pinning the seed\"\n  \
        pd list --json\n  \
        pd log --json",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,

    /// Use the global task file instead of this directory's
    #[arg(short = 'g', long, global = true)]
    pub global: bool,

    /// Operate on an explicit task file
    #[arg(short = 'f', long, value_name = "PATH", global = true)]
    pub file: Option<String>,

    /// Use (or create) a task file for this project, skipping discovery
    #[arg(long, global = true)]
    pub here: bool,

    /// Machine-readable output. Available on every command, including writes.
    #[arg(long, global = true)]
    pub json: bool,

    /// Never emit colour or symbols
    #[arg(long, global = true)]
    pub no_color: bool,

    /// The log sequence you last read. Required for path-addressed writes when stdin is
    /// not a terminal; prevents acting on a task whose position has since shifted.
    #[arg(long, value_name = "N", global = true)]
    pub expect_seq: Option<u64>,

    /// Override the clock (RFC 3339). Exists so date handling is testable.
    #[arg(long, value_name = "ISO", global = true, hide = true)]
    pub now: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Add a task
    ///
    /// Quotes are optional; trailing words are joined.
    /// --json returns the created task, including its permanent id.
    #[command(after_help = "EXAMPLE\n  pd add ship the migration -p2 -d \"friday 3pm\"")]
    Add {
        /// The task text
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// Priority, p1 (highest) to p4
        #[arg(short = 'p', long, value_name = "P1-P4")]
        priority: Option<String>,
        /// Due date, e.g. "fri", "tomorrow 9am", "dec 25"
        #[arg(short = 'd', long, value_name = "WHEN")]
        due: Option<String>,
        /// Nest under this task (path or id). Maximum depth is 4.
        #[arg(long, value_name = "PATH|ID")]
        under: Option<String>,
    },

    /// Mark a task done. Open subtasks are completed with it, each logged separately.
    #[command(after_help = "EXAMPLE\n  pd done 2.1 -m \"shipped in v2.1\"")]
    Done {
        target: String,
        /// A note recorded alongside the event
        #[arg(short = 'm', long)]
        message: Option<String>,
    },

    /// Mark a done task open again
    #[command(after_help = "EXAMPLE\n  pd reopen a7f")]
    Reopen {
        target: String,
        #[arg(short = 'm', long)]
        message: Option<String>,
    },

    /// Abandon a task — deliberately distinct from completing it
    #[command(after_help = "EXAMPLE\n  pd drop 3 -m \"decided against it\"")]
    Drop {
        target: String,
        #[arg(short = 'm', long)]
        message: Option<String>,
    },

    /// Revert the last event, whatever it was, by appending a compensating event
    ///
    /// History is never rewritten. Repeated undos walk back through the log.
    #[command(after_help = "EXAMPLE\n  pd undo")]
    Undo,

    /// Change a task's text
    #[command(after_help = "EXAMPLE\n  pd edit a7f fix the *other* flaky test")]
    Edit {
        target: String,
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
    },

    /// Set a task's priority
    #[command(after_help = "EXAMPLE\n  pd pri a7f p1     # or: pd pri a7f none")]
    Pri { target: String, priority: String },

    /// Set or clear a task's due date
    #[command(after_help = "EXAMPLE\n  pd due a7f tomorrow 9am     # or: pd due a7f none")]
    Due {
        target: String,
        #[arg(required = true, num_args = 1..)]
        when: Vec<String>,
    },

    /// Re-parent a task
    #[command(after_help = "EXAMPLE\n  pd mv a7f --under 2     # or: pd mv a7f --top")]
    Mv {
        target: String,
        /// New parent (path or id)
        #[arg(long, value_name = "PATH|ID", conflicts_with = "top")]
        under: Option<String>,
        /// Move to the top level
        #[arg(long)]
        top: bool,
    },

    /// Link people to a task, or unlink them with --remove
    ///
    /// Repeatable and idempotent in both directions: linking someone twice is not an
    /// error and does not duplicate them, and unlinking someone who is not there does
    /// nothing. A link means only that the person is involved — there are no roles.
    ///
    /// Names are matched loosely: trimmed, inner runs of spaces collapsed, and
    /// lower-cased, so `Meri` and `  meri ` are the same person. `pd people` derives the
    /// full list from the links themselves.
    ///
    /// --json returns the task, including its `people`.
    #[command(after_help = "EXAMPLE\n  \
        pd who a7f meri gegham\n  \
        pd who a7f --remove gegham")]
    Who {
        target: String,
        /// One or more people
        #[arg(required = true, num_args = 1..)]
        people: Vec<String>,
        /// Unlink them instead of linking them
        #[arg(short = 'r', long)]
        remove: bool,
    },

    /// The people this file knows about, with how much is open on each
    ///
    /// Derived from the links, never maintained: a person exists exactly as long as some
    /// task still names them. Sorted by open count, descending, then by name. Dropped
    /// tasks count towards neither total.
    ///
    /// --json returns `people`, each with `name`, `open` and `done`.
    #[command(after_help = "EXAMPLE\n  \
        pd people\n  \
        pd people rename meri \"meri sargsyan\"\n  \
        pd people remove gegham")]
    People {
        #[command(subcommand)]
        cmd: Option<PeopleCmd>,
    },

    /// Attach a note to a task without changing its state
    #[command(after_help = "EXAMPLE\n  pd note a7f -m \"blocked on the API review\"")]
    Note {
        target: String,
        #[arg(short = 'm', long, required = true)]
        message: String,
    },

    /// List tasks (the default when no command is given)
    #[command(after_help = "EXAMPLE\n  pd list --all --sort due")]
    List {
        /// Include dropped tasks too
        #[arg(short = 'a', long)]
        all: bool,
        /// Open tasks only — hide the done ones shown by default
        #[arg(short = 'o', long, conflicts_with = "all")]
        open: bool,
        /// One-shot sort: priority, due, created, alpha
        #[arg(long, value_name = "KEY")]
        sort: Option<String>,
        /// Show only this priority
        #[arg(short = 'p', long, value_name = "P1-P4")]
        priority: Option<String>,
        /// Show only tasks linked to this person, keeping their parents as context
        ///
        /// Matched case-insensitively, so any spelling of the name finds them.
        #[arg(long, value_name = "PERSON")]
        person: Option<String>,
        /// Substring filter
        #[arg(num_args = 0..)]
        filter: Vec<String>,
    },

    /// List open tasks across every registered task file
    #[command(after_help = "EXAMPLE\n  pd all --json")]
    All,

    /// Show the ledger, optionally for a single task
    #[command(after_help = "EXAMPLE\n  pd log a7f --json")]
    Log {
        /// Limit to one task (path or id)
        target: Option<String>,
        /// Show at most this many events, most recent last
        #[arg(short = 'n', long, default_value = "40")]
        limit: usize,
    },

    /// Archive the current log and start fresh from a snapshot
    ///
    /// Never runs automatically. The old log is appended to .podrick.archive.
    #[command(after_help = "EXAMPLE\n  pd compact")]
    Compact,

    /// Show or set configuration
    ///
    /// With no arguments, prints the resolved chain so the ordering is never a mystery.
    #[command(after_help = "EXAMPLE\n  pd config sort due --project")]
    Config {
        key: Option<String>,
        value: Option<String>,
        /// Apply to this project only, rather than globally
        ///
        /// Named `--project` rather than `--here` because the global `--here` means
        /// something else — "skip discovery and use cwd's own file" — and one token
        /// cannot mean both. Spelling them the same made `pd config sort … --here`
        /// impossible in any directory whose file came from discovery.
        #[arg(long)]
        project: bool,
    },

    /// Apply many changes as a single, single-undo action
    ///
    /// Reads operations from stdin, one JSON object per line (a JSON array of the same
    /// objects also works). Every resulting event shares one batch id, so `pd undo`
    /// reverts the whole set and never half of it.
    ///
    /// Nothing is written unless every operation validates, so a typo on line 40 does not
    /// leave lines 1-39 applied.
    ///
    /// OPERATIONS
    ///   {"op":"edit",   "id":"a7f", "text":"New text"}
    ///   {"op":"pri",    "id":"a7f", "priority":"p1"}       p1-p4, or none
    ///   {"op":"due",    "id":"a7f", "when":"friday 3pm"}   or "none" to clear
    ///   {"op":"mv",     "id":"a7f", "under":"b2c"}         or "top":true
    ///   {"op":"done",   "id":"a7f", "note":"optional"}     cascades to open subtasks
    ///   {"op":"reopen", "id":"a7f"}
    ///   {"op":"drop",   "id":"a7f", "note":"optional"}     cascades to open subtasks
    ///   {"op":"note",   "id":"a7f", "message":"..."}
    ///
    /// `add` is deliberately not available here: `created` has no compensating event, so
    /// a batch containing one could only ever be half-undone.
    #[command(after_help = "EXAMPLE\n  \
            # capitalise every task, as one undoable action\n  \
            pd list --json | jq -c '.tasks[] | {op:\"edit\", id, \
            text:(.text|ascii_upcase[0:1]+.[1:])}' | pd batch")]
    Batch,

    /// List every known task file
    #[command(after_help = "EXAMPLE\n  pd files --json")]
    Files,

    /// Generate a shell completion script
    #[command(after_help = "EXAMPLE\n  pd completions zsh > ~/.zfunc/_pd")]
    Completions { shell: Shell },
}

#[derive(Subcommand, Debug)]
pub enum PeopleCmd {
    /// Rename a person everywhere at once
    ///
    /// Appended as a ledger event rather than applied by rewriting history, so every
    /// link that named the old name resolves to the new one on replay — including the
    /// links in a log written before the rename. Renaming onto a name that already
    /// exists merges the two.
    #[command(after_help = "EXAMPLE\n  pd people rename meri \"meri sargsyan\"")]
    Rename { old: String, new: String },

    /// Unlink a person from every task
    ///
    /// One undoable action, so `pd undo` puts every link back.
    #[command(after_help = "EXAMPLE\n  pd people remove gegham")]
    Remove { person: String },
}

/// A person's name as the two things it has to be at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonName {
    /// What everything is compared against: trimmed, inner runs of whitespace collapsed
    /// to one space, lower-cased.
    pub key: String,
    /// The same name with the caller's capitalisation intact. `McDonald` and `de Vries`
    /// are not spelling mistakes, and no rule that rewrites them is going to be right
    /// often enough to be worth being wrong about.
    pub display: String,
}

/// Split a name into the form that is matched and the form that is shown.
///
/// Matching is case-insensitive because nobody types a colleague's name the same way
/// twice, and it folds to one key rather than keeping every spelling because two
/// spellings of one person is exactly the failure a derived list cannot recover from —
/// the counts would split, and `pd people rename` would only fix half of them.
pub fn parse_person(s: &str) -> Result<PersonName, String> {
    let display = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if display.is_empty() {
        return Err("a person needs a name".to_string());
    }
    Ok(PersonName {
        key: display.to_lowercase(),
        display,
    })
}

/// Accepts `p1`..`p4`, `1`..`4`, and `none`.
pub fn parse_priority(s: &str) -> Result<Option<u8>, String> {
    let s = s.trim().to_lowercase();
    if s == "none" || s == "clear" || s == "p0" {
        return Ok(None);
    }
    let digits = s.strip_prefix('p').unwrap_or(&s);
    match digits.parse::<u8>() {
        Ok(n) if (1..=4).contains(&n) => Ok(Some(n)),
        _ => Err(format!(
            "{s:?} is not a priority; use p1, p2, p3, p4, or none"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_command_surface_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn people_names_fold_to_one_key_and_keep_their_spelling() {
        let p = parse_person("  Meri ").unwrap();
        assert_eq!((p.key.as_str(), p.display.as_str()), ("meri", "Meri"));

        let p = parse_person("MERI\tSARGSYAN").unwrap();
        assert_eq!(p.key, "meri sargsyan");
        assert_eq!(
            p.display, "MERI SARGSYAN",
            "the caller's caps are kept verbatim"
        );

        assert_eq!(
            parse_person("meri   sargsyan").unwrap().display,
            "meri sargsyan"
        );
        assert_eq!(parse_person("McDonald").unwrap().key, "mcdonald");
        assert!(parse_person("   ").is_err());
    }

    #[test]
    fn priorities_parse_every_documented_spelling() {
        assert_eq!(parse_priority("p1").unwrap(), Some(1));
        assert_eq!(parse_priority("P4").unwrap(), Some(4));
        assert_eq!(parse_priority("2").unwrap(), Some(2));
        assert_eq!(parse_priority("none").unwrap(), None);
        assert!(parse_priority("p5").is_err());
        assert!(parse_priority("banana").is_err());
    }
}
