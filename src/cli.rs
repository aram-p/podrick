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
        /// Include done and dropped tasks
        #[arg(short = 'a', long)]
        all: bool,
        /// One-shot sort: priority, due, created, alpha
        #[arg(long, value_name = "KEY")]
        sort: Option<String>,
        /// Show only this priority
        #[arg(short = 'p', long, value_name = "P1-P4")]
        priority: Option<String>,
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

    /// List every known task file
    #[command(after_help = "EXAMPLE\n  pd files --json")]
    Files,

    /// Generate a shell completion script
    #[command(after_help = "EXAMPLE\n  pd completions zsh > ~/.zfunc/_pd")]
    Completions { shell: Shell },
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
    fn priorities_parse_every_documented_spelling() {
        assert_eq!(parse_priority("p1").unwrap(), Some(1));
        assert_eq!(parse_priority("P4").unwrap(), Some(4));
        assert_eq!(parse_priority("2").unwrap(), Some(2));
        assert_eq!(parse_priority("none").unwrap(), None);
        assert!(parse_priority("p5").is_err());
        assert!(parse_priority("banana").is_err());
    }
}
