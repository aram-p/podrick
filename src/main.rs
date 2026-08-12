//! podrick — a terminal task tracker with a perfect memory.
//!
//! The log is the truth (see `event`), state is replayed from it (`state`), and every
//! command is safe to drive from a script (`--json`, exit codes, no prompts without a
//! terminal).

mod cli;
mod config;
mod dates;
mod error;
mod event;
mod render;
mod state;
mod store;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chrono::{DateTime, Local};
use clap::{CommandFactory, Parser};
use serde_json::{json, Value};

use cli::{Cli, Cmd};
use config::Sort;
use error::{AppError, Result};
use event::{Actor, Data, Event, Kind};
use render::Style;
use state::{State, Task, TaskState, MAX_DEPTH};
use store::{How, Registry, ResolveOpts, Resolved, Store};

const NUDGE_MICROS: u128 = 50_000;

/// Flags that consume the argument after them, so argv normalisation can skip their
/// values rather than mistaking one for a subcommand.
const VALUE_FLAGS: [&str; 4] = ["-f", "--file", "--expect-seq", "--now"];

/// `pd fix the thing` has to mean "search for it", and `pd -a` has to mean "list
/// everything", but clap needs a subcommand to attach either to. When no known command is
/// present, insert an explicit `list` immediately after argv[0].
///
/// It goes at the front rather than in front of the first positional because `list`'s own
/// flags — `-a`, `--sort` — are not global, so clap will not accept them ahead of the
/// subcommand. `pd -a decide` has to become `pd list -a decide`, not `pd -a list decide`.
fn normalize_args(raw: Vec<String>) -> Vec<String> {
    if raw.len() < 2 {
        return raw;
    }
    let known: Vec<String> = Cli::command()
        .get_subcommands()
        .map(|s| s.get_name().to_string())
        .collect();

    let mut i = 1;
    while i < raw.len() {
        let a = &raw[i];
        if a == "--" {
            break;
        }
        if a.starts_with('-') {
            // `pd --help` and `pd --version` describe the tool itself; rewriting them to
            // `pd list --help` would answer a question nobody asked.
            if matches!(a.as_str(), "-h" | "--help" | "-V" | "--version") {
                return raw;
            }
            if VALUE_FLAGS.contains(&a.as_str()) {
                i += 1;
            }
            i += 1;
            continue;
        }
        // A real subcommand: leave the line exactly as the caller wrote it.
        if known.contains(a) {
            return raw;
        }
        break;
    }

    let mut out: Vec<String> = raw.iter().take(1).cloned().collect();
    out.push("list".to_string());
    out.extend_from_slice(&raw[1..]);
    out
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().collect();

    // clap handles --help itself and exits, so the schema has to be served before it.
    if raw.iter().any(|a| a == "--json") && raw.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", command_schema());
        return ExitCode::SUCCESS;
    }

    let cli = Cli::parse_from(normalize_args(raw));
    let json = cli.json;

    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if json {
                let payload = json!({
                    "error": e.msg,
                    "hint": e.hint,
                    "code": e.code as i32,
                });
                println!("{payload}");
            } else {
                eprintln!("pd: {}", e.msg);
                if let Some(h) = &e.hint {
                    eprintln!("    {h}");
                }
            }
            ExitCode::from(e.code as u8)
        }
    }
}

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

struct Ctx {
    json: bool,
    style: Style,
    now: DateTime<Local>,
    actor: Actor,
    tty: bool,
    expect_seq: Option<u64>,
}

impl Ctx {
    fn build(cli: &Cli) -> Result<Ctx> {
        let tty = std::io::stdin().is_terminal();
        let color = std::io::stdout().is_terminal()
            && !cli.no_color
            && std::env::var_os("NO_COLOR").is_none()
            && !cli.json;

        let now = match cli
            .now
            .clone()
            .or_else(|| std::env::var("PODRICK_NOW").ok())
        {
            Some(s) => DateTime::parse_from_rfc3339(&s)
                .map_err(|_| {
                    AppError::usage(format!("--now must be RFC 3339, got {s:?}"))
                        .with_hint("example: --now 2026-08-12T14:30:00+04:00")
                })?
                .with_timezone(&Local),
            None => Local::now(),
        };

        Ok(Ctx {
            json: cli.json,
            style: if color {
                Style { color: true }
            } else {
                Style::plain()
            },
            now,
            actor: if tty { Actor::Cli } else { Actor::Agent },
            tty,
            expect_seq: cli.expect_seq,
        })
    }

    fn ts(&self) -> String {
        self.now.to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
    }

    /// Chatter. Always stderr, so stdout stays a clean data channel.
    ///
    /// A terminal gets a quiet marker that sits under the tree without competing with it;
    /// everything else keeps the greppable `pd:` prefix that scripts may already match on.
    fn note(&self, msg: &str) {
        if self.style.color {
            eprintln!("  {} {}", self.style.accent("↳"), self.style.dim(msg));
        } else {
            eprintln!("pd: {msg}");
        }
    }

    fn emit(&self, value: Value) {
        println!("{value}");
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

impl Cmd {
    /// Whether this command appends to the ledger. Adoption of a file found through
    /// discovery is announced only for these — knowing *where* a write landed matters,
    /// whereas repeating it on every bare `pd` is just noise.
    fn mutates(&self) -> bool {
        matches!(
            self,
            Cmd::Add { .. }
                | Cmd::Done { .. }
                | Cmd::Reopen { .. }
                | Cmd::Drop { .. }
                | Cmd::Undo
                | Cmd::Edit { .. }
                | Cmd::Pri { .. }
                | Cmd::Due { .. }
                | Cmd::Mv { .. }
                | Cmd::Note { .. }
                | Cmd::Compact
        )
    }
}

fn run(cli: &Cli) -> Result<()> {
    let ctx = Ctx::build(cli)?;

    match &cli.cmd {
        None => cmd_list(cli, &ctx, false, None, None, &[]),
        Some(Cmd::List {
            all,
            sort,
            priority,
            filter,
        }) => cmd_list(
            cli,
            &ctx,
            *all,
            sort.as_deref(),
            priority.as_deref(),
            filter,
        ),
        Some(Cmd::Add {
            text,
            priority,
            due,
            under,
        }) => cmd_add(
            cli,
            &ctx,
            text,
            priority.as_deref(),
            due.as_deref(),
            under.as_deref(),
        ),
        Some(Cmd::Done { target, message }) => {
            cmd_close(cli, &ctx, target, message, Kind::Completed)
        }
        Some(Cmd::Reopen { target, message }) => {
            cmd_close(cli, &ctx, target, message, Kind::Uncompleted)
        }
        Some(Cmd::Drop { target, message }) => cmd_close(cli, &ctx, target, message, Kind::Dropped),
        Some(Cmd::Undo) => cmd_undo(cli, &ctx),
        Some(Cmd::Edit { target, text }) => cmd_edit(cli, &ctx, target, &text.join(" ")),
        Some(Cmd::Pri { target, priority }) => cmd_pri(cli, &ctx, target, priority),
        Some(Cmd::Due { target, when }) => cmd_due(cli, &ctx, target, &when.join(" ")),
        Some(Cmd::Mv { target, under, top }) => cmd_mv(cli, &ctx, target, under.as_deref(), *top),
        Some(Cmd::Note { target, message }) => cmd_note(cli, &ctx, target, message),
        Some(Cmd::All) => cmd_all(&ctx),
        Some(Cmd::Log { target, limit }) => cmd_log(cli, &ctx, target.as_deref(), *limit),
        Some(Cmd::Compact) => cmd_compact(cli, &ctx),
        Some(Cmd::Config { key, value, here }) => {
            cmd_config(cli, &ctx, key.as_deref(), value.as_deref(), *here)
        }
        Some(Cmd::Files) => cmd_files(&ctx),
        Some(Cmd::Completions { shell }) => {
            clap_complete::generate(*shell, &mut Cli::command(), "pd", &mut std::io::stdout());
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Opening a file
// ---------------------------------------------------------------------------

struct Open {
    resolved: Resolved,
    store: Store,
    registry: Registry,
}

/// Resolve the task file and load it. `may_create` is true only for commands that
/// legitimately bring a file into being.
fn open(cli: &Cli, ctx: &Ctx, may_create: bool) -> Result<Open> {
    let cwd = std::env::current_dir()?;
    let mut registry = Registry::load()?;

    let opts = ResolveOpts {
        explicit: cli.file.as_deref().map(Path::new),
        global: cli.global,
        here: cli.here,
        may_create,
        cwd,
    };

    let resolved = store::resolve(&opts, &registry)?.ok_or_else(|| {
        AppError::not_found("no task file for this directory").with_hint(
            "use --here to create one, -f <path> to target a file, or -g for the global list",
        )
    })?;

    // A file we would have to bring into existence.
    if resolved.how == How::Created && !resolved.path.exists() {
        if !may_create {
            return Err(AppError::not_found("no task file for this directory").with_hint(
                "use --here to create one, -f <path> to target a file, or -g for the global list",
            ));
        }
        if !cli.here {
            // Agents must opt in deliberately; scattering task files is worse than an error.
            if !ctx.tty {
                return Err(AppError::not_found(format!(
                    "no task file for {}",
                    resolved.path.parent().unwrap_or(Path::new(".")).display()
                ))
                .with_hint(
                    "pass --here to create one, -f <path> to target an existing file, or -g for the global list",
                ));
            }
            if !confirm(&format!(
                "No task file for {}. Create one here?",
                resolved.path.parent().unwrap_or(Path::new(".")).display()
            ))? {
                return Err(AppError::not_found("no task file, and creation declined"));
            }
        }
        ctx.note(&format!(
            "created {} in {}",
            store::FILE_NAME,
            resolved.path.parent().unwrap_or(Path::new(".")).display()
        ));
        maybe_gitignore(ctx, &resolved.path)?;
    }

    // Adoption is only worth announcing when something is about to be written. A read
    // that lands on a discovered file is harmless; a write to one you did not expect is
    // not, so that case always speaks up.
    if let How::Adopted(sig) = &resolved.how {
        if cli.cmd.as_ref().is_some_and(Cmd::mutates) {
            ctx.note(&format!("{} · {}", tilde(&resolved.path), sig.describe()));
        }
    }
    for s in &resolved.suggestions {
        ctx.note(&format!("also tracking {} · {}", tilde(&s.path), s.why));
    }

    let store = store::load(&resolved.path)?;

    if store.torn_lines > 0 {
        ctx.note(&format!(
            "{} unreadable line(s) in the log were skipped",
            store.torn_lines
        ));
    }

    registry.upsert(&resolved.path, &ctx.ts());
    nudge_if_slow(ctx, &store, &mut registry);
    registry.save()?;

    Ok(Open {
        resolved,
        store,
        registry,
    })
}

/// The tool notices it has got slow and says so. It never compacts on its own.
fn nudge_if_slow(ctx: &Ctx, store: &Store, registry: &mut Registry) {
    if store.replay_micros < NUDGE_MICROS || !ctx.tty {
        return;
    }
    let today = ctx.now.date_naive().to_string();
    let entry = registry.upsert(&store.path, &ctx.ts());
    if entry.last_nudge.as_deref() == Some(today.as_str()) {
        return;
    }
    entry.last_nudge = Some(today);
    ctx.note(&format!(
        "this log took {}ms to read — `pd compact` will archive it and speed things up",
        store.replay_micros / 1000
    ));
}

/// The project a task file belongs to, shortened for human eyes: the containing
/// directory rather than the dotfile itself, with `$HOME` collapsed to `~`.
fn tilde(task_file: &Path) -> String {
    let dir = task_file.parent().unwrap_or(task_file);
    match dirs::home_dir().and_then(|h| dir.strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => dir.display().to_string(),
    }
}

fn confirm(question: &str) -> Result<bool> {
    eprint!("{question} [Y/n] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let a = line.trim().to_lowercase();
    Ok(a.is_empty() || a == "y" || a == "yes")
}

fn maybe_gitignore(ctx: &Ctx, task_file: &Path) -> Result<()> {
    if !ctx.tty {
        return Ok(());
    }
    let Some(dir) = task_file.parent() else {
        return Ok(());
    };
    let Some(root) = store::git_root(dir) else {
        return Ok(());
    };
    // The glob, not the bare name: the lock and the archive sit beside the log and would
    // otherwise show up in `git status` forever.
    let pattern = format!("{}*", store::FILE_NAME);
    let gitignore = root.join(".gitignore");
    let existing = std::fs::read_to_string(&gitignore).unwrap_or_default();
    if existing
        .lines()
        .any(|l| matches!(l.trim(), x if x == pattern || x == store::FILE_NAME))
    {
        return Ok(());
    }
    if !confirm(&format!("Add {pattern} to .gitignore?"))? {
        return Ok(());
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("{pattern}\n"));
    std::fs::write(&gitignore, out)?;
    ctx.note(&format!("added {pattern} to {}", gitignore.display()));
    Ok(())
}

// ---------------------------------------------------------------------------
// Targeting
// ---------------------------------------------------------------------------

fn is_path_like(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// Resolve a user-supplied target (id or dotted path) to a permanent id.
///
/// Path-addressed writes from a non-terminal caller must pin the log sequence they read,
/// because a path is a position and positions shift.
fn target_id(ctx: &Ctx, store: &Store, target: &str, is_write: bool) -> Result<String> {
    if store.state.contains_id(target) {
        return Ok(target.to_string());
    }

    if is_path_like(target) {
        if is_write && !ctx.tty && ctx.expect_seq != Some(store.state.last_seq) {
            return Err(AppError::conflict(format!(
                "refusing a path-addressed write from a non-interactive caller (log is at seq {})",
                store.state.last_seq
            ))
            .with_hint(
                "address the task by its id, or pass --expect-seq <n> with the seq you last read",
            ));
        }
        if let Some(id) = store.state.paths().id_at(target) {
            return Ok(id.to_string());
        }
    }

    Err(AppError::not_found(format!("no task {target:?}"))
        .with_hint("run `pd list` to see paths and ids"))
}

// ---------------------------------------------------------------------------
// JSON shapes
// ---------------------------------------------------------------------------

fn task_json(t: &Task, path: Option<&str>) -> Value {
    json!({
        "id": t.id,
        "path": path,
        "text": t.text,
        "state": match t.state {
            TaskState::Open => "open",
            TaskState::Done => "done",
            TaskState::Dropped => "dropped",
        },
        "priority": t.priority,
        "due": t.due,
        "parent": t.parent,
        "created_at": t.created_at,
        "completed_at": t.completed_at,
    })
}

fn envelope(open: &Open, extra: Value) -> Value {
    envelope_at(&open.resolved.path, open.store.state.last_seq, extra)
}

/// Every payload carries the resolved file and the log sequence it reflects, so a caller
/// can pass that seq straight back as `--expect-seq`.
fn envelope_at(path: &Path, seq: u64, extra: Value) -> Value {
    let mut v = json!({ "file": path, "seq": seq });
    if let (Some(map), Some(extra)) = (v.as_object_mut(), extra.as_object()) {
        for (k, val) in extra {
            map.insert(k.clone(), val.clone());
        }
    }
    v
}

fn command_schema() -> Value {
    let cmd = Cli::command();
    let commands: Vec<Value> = cmd
        .get_subcommands()
        .map(|sc| {
            let args: Vec<Value> = sc
                .get_arguments()
                .map(|a| {
                    json!({
                        "name": a.get_id().as_str(),
                        "long": a.get_long(),
                        "short": a.get_short().map(|c| c.to_string()),
                        "required": a.is_required_set(),
                        "takes_value": a.get_num_args().map(|n| n.takes_values()).unwrap_or(false),
                        "help": a.get_help().map(|h| h.to_string()),
                    })
                })
                .collect();
            json!({
                "name": sc.get_name(),
                "about": sc.get_about().map(|a| a.to_string()),
                "args": args,
            })
        })
        .collect();

    json!({
        "name": "pd",
        "version": env!("CARGO_PKG_VERSION"),
        "about": cmd.get_about().map(|a| a.to_string()),
        "exit_codes": {
            "0": "success",
            "1": "not found",
            "2": "usage error",
            "3": "conflict (stale path)",
            "4": "I/O error",
        },
        "conventions": {
            "stdout": "data only",
            "stderr": "all other output",
            "identity": "`id` is permanent; `path` is positional and shifts. Agents should use `id`.",
            "staleness": "path-addressed writes without a TTY require --expect-seq matching the current seq",
        },
        "commands": commands,
    })
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Flag beats project setting beats global config beats insertion order. Every one of
/// those is parsed into a `Sort` at its own boundary, so an unknown key fails the same
/// way whichever of the three it came from.
fn effective_sort(cli_sort: Option<&str>, open: &Open) -> Result<Option<Sort>> {
    if let Some(s) = cli_sort {
        return Ok(Some(s.parse()?));
    }
    if let Some(e) = open.registry.get(&open.resolved.path) {
        if let Some(s) = e.sort {
            return Ok(Some(s));
        }
    }
    Ok(config::Config::load()?.sort)
}

fn cmd_list(
    cli: &Cli,
    ctx: &Ctx,
    all: bool,
    sort: Option<&str>,
    priority: Option<&str>,
    filter: &[String],
) -> Result<()> {
    let open = open(cli, ctx, false)?;
    let filter = filter.join(" ");
    let filter = if filter.trim().is_empty() {
        None
    } else {
        Some(filter)
    };

    let key = effective_sort(sort, &open)?;
    let mut rows = render::tree_rows(&open.store.state, all, filter.as_deref(), key);

    if let Some(p) = priority {
        let want = cli::parse_priority(p).map_err(AppError::usage)?;
        rows.retain(|r| r.priority == want);
        // Selecting by priority cuts across the tree, so the survivors are a worklist,
        // not a tree — a kept child whose parent was filtered out would otherwise render
        // indented under whichever unrelated row happened to precede it. Flatten and
        // re-sort; the `path` column still reports where each task really lives.
        for r in rows.iter_mut() {
            r.depth = 1;
            r.gap = false;
        }
        if let Some(key) = key {
            render::sort_flat(&mut rows, key);
        }
    }

    if ctx.json {
        let paths = open.store.state.paths();
        let tasks: Vec<Value> = rows
            .iter()
            .filter_map(|r| open.store.state.get(&r.id))
            .map(|t| task_json(t, paths.path_of(&t.id)))
            .collect();
        ctx.emit(envelope(&open, json!({ "tasks": tasks })));
        return Ok(());
    }

    if rows.is_empty() {
        let msg = match &filter {
            Some(f) => format!("nothing matching {f:?}"),
            None => "nothing open".to_string(),
        };
        println!("{}", ctx.style.dim(&msg));
        return Ok(());
    }

    println!("{}", render::rows(&rows, ctx.style, ctx.now));

    let open_count = open
        .store
        .state
        .tasks
        .iter()
        .filter(|t| t.state.is_open())
        .count();
    let done_count = open
        .store
        .state
        .tasks
        .iter()
        .filter(|t| t.state == TaskState::Done)
        .count();
    if ctx.style.color {
        println!(
            "\n{}",
            ctx.style
                .dim(&format!("{open_count} open · {done_count} done"))
        );
    }
    Ok(())
}

fn cmd_add(
    cli: &Cli,
    ctx: &Ctx,
    text: &[String],
    priority: Option<&str>,
    due: Option<&str>,
    under: Option<&str>,
) -> Result<()> {
    let open = open(cli, ctx, true)?;
    let text = text.join(" ");
    if text.trim().is_empty() {
        return Err(AppError::usage("a task needs some text"));
    }

    let priority = match priority {
        Some(p) => cli::parse_priority(p).map_err(AppError::usage)?,
        None => None,
    };
    let due = match due {
        Some(d) => dates::parse(d, ctx.now).map_err(|e| {
            AppError::usage(e.to_string()).with_hint("try: fri, tomorrow 9am, dec 25, 2026-12-25")
        })?,
        None => None,
    };

    let parent = match under {
        Some(u) => {
            let pid = target_id(ctx, &open.store, u, true)?;
            let depth = open.store.state.depth(&pid);
            if depth >= MAX_DEPTH {
                return Err(AppError::usage(format!(
                    "nesting is limited to {MAX_DEPTH} levels; {u:?} is already at depth {depth}"
                )));
            }
            Some(pid)
        }
        None => None,
    };

    let id = store::new_id(&|s| open.store.state.contains_id(s));
    let ev = Event::new(
        open.store.state.last_seq + 1,
        ctx.ts(),
        ctx.actor,
        Kind::Created,
        &id,
    )
    .with_data(Data {
        text: Some(text.clone()),
        parent: parent.clone(),
        priority,
        due: due.clone(),
        ..Default::default()
    });

    store::append(&open.resolved.path, &[ev])?;

    let after = store::load(&open.resolved.path)?;
    let paths = after.state.paths();
    let task = after
        .state
        .get(&id)
        .ok_or_else(|| AppError::io("the task did not survive the write"))?;

    if ctx.json {
        ctx.emit(envelope_at(
            &open.resolved.path,
            after.state.last_seq,
            json!({ "task": task_json(task, paths.path_of(&id)) }),
        ));
    } else {
        let path = paths.path_of(&id).unwrap_or("-");
        println!(
            "{} {}  {}",
            ctx.style.accent(path),
            task.text,
            ctx.style.dim(&id)
        );
    }
    Ok(())
}

fn cmd_close(
    cli: &Cli,
    ctx: &Ctx,
    target: &str,
    message: &Option<String>,
    kind: Kind,
) -> Result<()> {
    let open = open(cli, ctx, false)?;
    let id = target_id(ctx, &open.store, target, true)?;
    let task = open
        .store
        .state
        .get(&id)
        .expect("resolved id exists")
        .clone();

    // Guard against no-ops so the ledger stays meaningful.
    match (kind, task.state) {
        (Kind::Completed, TaskState::Done) => {
            return Err(AppError::usage(format!("{:?} is already done", task.text)))
        }
        (Kind::Uncompleted, TaskState::Open) => {
            return Err(AppError::usage(format!("{:?} is already open", task.text)))
        }
        (Kind::Dropped, TaskState::Dropped) => {
            return Err(AppError::usage(format!(
                "{:?} is already dropped",
                task.text
            )))
        }
        _ => {}
    }

    // Completing a parent completes its open descendants — each logged individually, so
    // the ledger records what actually happened to every task. They share a batch id with
    // the primary event, so `pd undo` reverts the action as a whole.
    let cascade_targets: Vec<String> = if kind == Kind::Completed || kind == Kind::Dropped {
        open.store
            .state
            .descendants(&id)
            .into_iter()
            .filter(|d| d.state.is_open())
            .map(|d| d.id.clone())
            .collect()
    } else {
        Vec::new()
    };

    let first_seq = open.store.state.last_seq + 1;
    let primary_seq = first_seq + cascade_targets.len() as u64;

    let mut events: Vec<Event> = cascade_targets
        .iter()
        .enumerate()
        .map(|(i, did)| {
            Event::new(first_seq + i as u64, ctx.ts(), ctx.actor, kind, did)
                .in_batch(primary_seq)
                .with_data(Data {
                    cascaded_from: Some(id.clone()),
                    ..Default::default()
                })
        })
        .collect();

    events.push(
        Event::new(primary_seq, ctx.ts(), ctx.actor, kind, &id)
            .in_batch(primary_seq)
            .with_note(message.clone()),
    );
    let cascaded = cascade_targets;

    store::append(&open.resolved.path, &events)?;

    let after = store::load(&open.resolved.path)?;
    let verb = match kind {
        Kind::Completed => "done",
        Kind::Uncompleted => "reopened",
        Kind::Dropped => "dropped",
        _ => "updated",
    };

    if ctx.json {
        let t = after
            .state
            .get(&id)
            .map(|t| task_json(t, None))
            .unwrap_or(Value::Null);
        ctx.emit(envelope_at(
            &open.resolved.path,
            after.state.last_seq,
            json!({ "task": t, "cascaded": cascaded, "action": verb }),
        ));
    } else {
        println!("{verb}  {}", task.text);
        if !cascaded.is_empty() {
            println!(
                "{}",
                ctx.style
                    .dim(&format!("       and {} subtask(s)", cascaded.len()))
            );
        }
    }
    Ok(())
}

fn cmd_undo(cli: &Cli, ctx: &Ctx) -> Result<()> {
    let open = open(cli, ctx, false)?;
    let events = store::read_events(&open.resolved.path)?;

    let compensated: Vec<u64> = events.iter().filter_map(|e| e.undo_of).collect();

    // Only a batch's primary event is a candidate; undoing half a cascade would leave the
    // tree in a state the user never asked for.
    let candidate = events.iter().rev().find(|e| {
        e.undo_of.is_none()
            && e.is_primary()
            && !compensated.contains(&e.seq)
            && e.ev.inverse().is_some()
    });

    let Some(orig) = candidate else {
        return Err(AppError::not_found("nothing left to undo").with_hint(
            "`created` events are not undoable — use `pd drop <id>` to abandon a task",
        ));
    };

    // Revert every member of the batch, not just the event the user pointed at.
    let group: Vec<&Event> = match orig.batch {
        Some(b) => events.iter().filter(|e| e.batch == Some(b)).collect(),
        None => vec![orig],
    };

    let base = open.store.state.last_seq;
    let mut compensating = Vec::new();
    for (i, e) in group.iter().enumerate() {
        let Some(inverse) = e.ev.inverse() else {
            continue;
        };
        compensating.push(
            Event::new(base + 1 + i as u64, ctx.ts(), ctx.actor, inverse, &e.id)
                .with_data(Data {
                    // Swap from/to so the compensating event restores the previous value.
                    from: e.data.to.clone(),
                    to: e.data.from.clone(),
                    from_parent: e.data.to_parent.clone(),
                    to_parent: e.data.from_parent.clone(),
                    ..Default::default()
                })
                .undoing(e.seq),
        );
    }

    let reverted = compensating.len();
    store::append(&open.resolved.path, &compensating)?;

    let after = store::load(&open.resolved.path)?;
    if ctx.json {
        let t = after.state.get(&orig.id).map(|t| task_json(t, None));
        ctx.emit(envelope_at(
            &open.resolved.path,
            after.state.last_seq,
            json!({
                "undone": format!("{:?}", orig.ev).to_lowercase(),
                "undo_of": orig.seq,
                "events_reverted": reverted,
                "task": t,
            }),
        ));
    } else {
        let what = after
            .state
            .get(&orig.id)
            .map(|t| t.text.clone())
            .unwrap_or_else(|| orig.id.clone());
        let verb = format!("{:?}", orig.ev).to_lowercase();
        println!("undid {verb} on {what}");
        if reverted > 1 {
            println!(
                "{}",
                ctx.style
                    .dim(&format!("      and {} subtask(s)", reverted - 1))
            );
        }
    }
    Ok(())
}

/// Shared shape for the four single-field mutations.
fn mutate(
    cli: &Cli,
    ctx: &Ctx,
    target: &str,
    kind: Kind,
    build: impl FnOnce(&Task, &State) -> Result<(Data, String)>,
) -> Result<()> {
    let open = open(cli, ctx, false)?;
    let id = target_id(ctx, &open.store, target, true)?;
    let task = open
        .store
        .state
        .get(&id)
        .expect("resolved id exists")
        .clone();

    let (data, human) = build(&task, &open.store.state)?;

    let ev = Event::new(
        open.store.state.last_seq + 1,
        ctx.ts(),
        ctx.actor,
        kind,
        &id,
    )
    .with_data(data);
    store::append(&open.resolved.path, &[ev])?;

    let after = store::load(&open.resolved.path)?;
    if ctx.json {
        let paths = after.state.paths();
        let t = after
            .state
            .get(&id)
            .map(|t| task_json(t, paths.path_of(&id)))
            .unwrap_or(Value::Null);
        ctx.emit(envelope_at(
            &open.resolved.path,
            after.state.last_seq,
            json!({ "task": t }),
        ));
    } else {
        println!("{human}");
    }
    Ok(())
}

fn cmd_edit(cli: &Cli, ctx: &Ctx, target: &str, text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(AppError::usage("a task needs some text"));
    }
    let text = text.to_string();
    mutate(cli, ctx, target, Kind::Edited, move |task, _| {
        Ok((
            Data {
                from: Some(json!(task.text)),
                to: Some(json!(text)),
                ..Default::default()
            },
            format!("edited  {text}"),
        ))
    })
}

fn cmd_pri(cli: &Cli, ctx: &Ctx, target: &str, priority: &str) -> Result<()> {
    let p = cli::parse_priority(priority).map_err(AppError::usage)?;
    mutate(cli, ctx, target, Kind::Reprioritized, move |task, _| {
        if task.priority == p {
            return Err(AppError::usage("that is already the priority"));
        }
        let shown = p.map(|n| format!("p{n}")).unwrap_or_else(|| "none".into());
        Ok((
            Data {
                from: Some(task.priority.map(Value::from).unwrap_or(Value::Null)),
                to: Some(p.map(Value::from).unwrap_or(Value::Null)),
                ..Default::default()
            },
            format!("{shown}  {}", task.text),
        ))
    })
}

fn cmd_due(cli: &Cli, ctx: &Ctx, target: &str, when: &str) -> Result<()> {
    let due = dates::parse(when, ctx.now).map_err(|e| {
        AppError::usage(e.to_string()).with_hint("try: fri, tomorrow 9am, dec 25, none")
    })?;
    let now = ctx.now;
    mutate(cli, ctx, target, Kind::Rescheduled, move |task, _| {
        let shown = match &due {
            Some(d) => dates::humanize(d, now),
            None => "no due date".into(),
        };
        Ok((
            Data {
                from: Some(task.due.clone().map(Value::from).unwrap_or(Value::Null)),
                to: Some(due.clone().map(Value::from).unwrap_or(Value::Null)),
                ..Default::default()
            },
            format!("{shown}  {}", task.text),
        ))
    })
}

fn cmd_mv(cli: &Cli, ctx: &Ctx, target: &str, under: Option<&str>, top: bool) -> Result<()> {
    if under.is_none() && !top {
        return Err(AppError::usage("say where to: --under <path|id> or --top"));
    }

    // The new parent has to be resolved against the same state as the target, so this
    // one does its own open() rather than going through `mutate`.
    let open = open(cli, ctx, false)?;
    let id = target_id(ctx, &open.store, target, true)?;
    let task = open
        .store
        .state
        .get(&id)
        .expect("resolved id exists")
        .clone();

    let new_parent: Option<String> = match under {
        Some(u) => {
            let pid = target_id(ctx, &open.store, u, true)?;
            if open.store.state.would_cycle(&id, &pid) {
                return Err(AppError::usage("that would put a task inside itself"));
            }
            let subtree_height = open
                .store
                .state
                .descendants(&id)
                .iter()
                .map(|d| open.store.state.depth(&d.id))
                .max()
                .unwrap_or(open.store.state.depth(&id))
                - open.store.state.depth(&id)
                + 1;
            if open.store.state.depth(&pid) + subtree_height > MAX_DEPTH {
                return Err(AppError::usage(format!(
                    "that would nest deeper than {MAX_DEPTH} levels"
                )));
            }
            Some(pid)
        }
        None => None,
    };

    let ev = Event::new(
        open.store.state.last_seq + 1,
        ctx.ts(),
        ctx.actor,
        Kind::Moved,
        &id,
    )
    .with_data(Data {
        from_parent: Some(task.parent.clone().map(Value::from).unwrap_or(Value::Null)),
        to_parent: Some(new_parent.clone().map(Value::from).unwrap_or(Value::Null)),
        ..Default::default()
    });
    store::append(&open.resolved.path, &[ev])?;

    let after = store::load(&open.resolved.path)?;
    let paths = after.state.paths();
    if ctx.json {
        let t = after
            .state
            .get(&id)
            .map(|t| task_json(t, paths.path_of(&id)))
            .unwrap_or(Value::Null);
        ctx.emit(envelope_at(
            &open.resolved.path,
            after.state.last_seq,
            json!({ "task": t }),
        ));
    } else {
        println!(
            "moved  {}  {}",
            task.text,
            ctx.style.dim(paths.path_of(&id).unwrap_or("-"))
        );
    }
    Ok(())
}

fn cmd_note(cli: &Cli, ctx: &Ctx, target: &str, message: &str) -> Result<()> {
    let open = open(cli, ctx, false)?;
    let id = target_id(ctx, &open.store, target, true)?;
    let task = open
        .store
        .state
        .get(&id)
        .expect("resolved id exists")
        .clone();

    let ev = Event::new(
        open.store.state.last_seq + 1,
        ctx.ts(),
        ctx.actor,
        Kind::Noted,
        &id,
    )
    .with_note(Some(message.to_string()));
    store::append(&open.resolved.path, &[ev])?;

    let after = store::load(&open.resolved.path)?;
    if ctx.json {
        let t = after
            .state
            .get(&id)
            .map(|t| task_json(t, None))
            .unwrap_or(Value::Null);
        ctx.emit(envelope_at(
            &open.resolved.path,
            after.state.last_seq,
            json!({ "task": t, "note": message }),
        ));
    } else {
        println!("noted  {}", task.text);
    }
    Ok(())
}

fn cmd_log(cli: &Cli, ctx: &Ctx, target: Option<&str>, limit: usize) -> Result<()> {
    let open = open(cli, ctx, false)?;
    let events = store::read_events(&open.resolved.path)?;

    let id = match target {
        Some(t) => Some(target_id(ctx, &open.store, t, false)?),
        None => None,
    };
    let filtered: Vec<&Event> = events
        .iter()
        .filter(|e| id.as_ref().is_none_or(|i| &e.id == i))
        .collect();
    let shown: Vec<&Event> = filtered.iter().rev().take(limit).rev().copied().collect();

    if ctx.json {
        ctx.emit(envelope(&open, json!({ "events": shown })));
        return Ok(());
    }

    if shown.is_empty() {
        println!("{}", ctx.style.dim("no events"));
        return Ok(());
    }

    for e in shown {
        let text = open
            .store
            .state
            .get(&e.id)
            .map(|t| t.text.clone())
            .unwrap_or_else(|| e.id.clone());
        let when = DateTime::parse_from_rfc3339(&e.ts)
            .map(|d| dates::humanize(&d.format("%Y-%m-%dT%H:%M").to_string(), ctx.now))
            .unwrap_or_else(|_| e.ts.clone());
        let verb = format!("{:?}", e.ev).to_lowercase();
        let actor = match e.actor {
            Actor::Cli => "you",
            Actor::Agent => "agent",
        };
        let mut line = format!(
            "  {} {} {} {}",
            ctx.style.dim(&format!("{:>4}", e.seq)),
            ctx.style.accent(&format!("{verb:<14}")),
            text,
            ctx.style.dim(&format!("· {actor} · {when}")),
        );
        if let Some(n) = &e.note {
            line.push_str(&format!("\n       {}", ctx.style.dim(&format!("“{n}”"))));
        }
        println!("{line}");
    }
    Ok(())
}

fn cmd_compact(cli: &Cli, ctx: &Ctx) -> Result<()> {
    let open = open(cli, ctx, false)?;
    let snap = Event::new(
        open.store.state.last_seq + 1,
        ctx.ts(),
        ctx.actor,
        Kind::Compacted,
        "",
    )
    .with_data(state::snapshot(&open.store.state));

    let before = store::compact(&open.resolved.path, &snap)?;
    let archive = open.resolved.path.with_file_name(store::ARCHIVE_NAME);

    if ctx.json {
        ctx.emit(json!({
            "file": open.resolved.path,
            "events_archived": before,
            "archive": archive,
            "tasks": open.store.state.tasks.len(),
        }));
    } else {
        println!(
            "archived {before} events to {}\n{} tasks carried forward",
            archive.display(),
            open.store.state.tasks.len()
        );
    }
    Ok(())
}

fn cmd_config(
    cli: &Cli,
    ctx: &Ctx,
    key: Option<&str>,
    value: Option<&str>,
    here: bool,
) -> Result<()> {
    let mut cfg = config::Config::load()?;

    // Reading the resolved chain needs a file; setting a global does not.
    let opened = open(cli, ctx, false).ok();

    match (key, value) {
        (Some("sort"), Some(v)) => {
            let parsed: Sort = v.parse()?;
            if here {
                let mut o = opened.ok_or_else(|| {
                    AppError::not_found("--here needs a task file in this directory")
                })?;
                let ts = ctx.ts();
                o.registry.upsert(&o.resolved.path, &ts).sort = Some(parsed);
                o.registry.save()?;
                if !ctx.json {
                    println!("sort = {v} for {}", o.resolved.path.display());
                }
            } else {
                cfg.sort = Some(parsed);
                cfg.save()?;
                if !ctx.json {
                    println!("sort = {v} globally");
                }
            }
            if ctx.json {
                ctx.emit(json!({ "sort": v, "scope": if here { "project" } else { "global" } }));
            }
            return Ok(());
        }
        (Some(k), _) if k != "sort" => {
            return Err(AppError::usage(format!(
                "unknown config key {k:?}; the only key is `sort`"
            )))
        }
        _ => {}
    }

    // No arguments: print the resolution chain, so the ordering is never a mystery.
    let project = opened
        .as_ref()
        .and_then(|o| o.registry.get(&o.resolved.path))
        .and_then(|e| e.sort);
    let effective = project.or(cfg.sort);

    if ctx.json {
        ctx.emit(json!({
            "sort": {
                "effective": effective.map_or("insertion", Sort::as_str),
                "project": project.map(Sort::as_str),
                "global": cfg.sort.map(Sort::as_str),
                "default": "insertion",
            }
        }));
        return Ok(());
    }

    println!("sort");
    println!(
        "  {}  {}",
        ctx.style.dim("--sort flag "),
        ctx.style.dim("(one-shot)")
    );
    println!(
        "  {}  {}",
        ctx.style.dim("project     "),
        project.map_or_else(|| ctx.style.dim("unset"), |s| s.to_string())
    );
    println!(
        "  {}  {}",
        ctx.style.dim("global      "),
        cfg.sort
            .map_or_else(|| ctx.style.dim("unset"), |s| s.to_string())
    );
    println!("  {}  insertion", ctx.style.dim("default     "));
    println!(
        "\n{}",
        ctx.style.accent(&format!(
            "→ {}",
            effective.map_or("insertion", Sort::as_str)
        ))
    );
    Ok(())
}

fn cmd_files(ctx: &Ctx) -> Result<()> {
    let reg = Registry::load()?;
    if ctx.json {
        let files: Vec<Value> = reg
            .entries
            .iter()
            .map(|e| {
                json!({
                    "path": e.path,
                    "remote": e.remote,
                    "last_used": e.last_used,
                    "sort": e.sort,
                })
            })
            .collect();
        ctx.emit(json!({ "files": files }));
        return Ok(());
    }
    if reg.entries.is_empty() {
        println!("{}", ctx.style.dim("no task files yet"));
        return Ok(());
    }
    for e in &reg.entries {
        let open_count = store::load(&e.path)
            .map(|s| s.state.tasks.iter().filter(|t| t.state.is_open()).count())
            .unwrap_or(0);
        println!(
            "  {}  {}",
            e.path.display(),
            ctx.style.dim(&format!("{open_count} open"))
        );
    }
    Ok(())
}

fn cmd_all(ctx: &Ctx) -> Result<()> {
    let reg = Registry::load()?;
    let mut files: Vec<Value> = Vec::new();
    let mut printed = false;

    for e in &reg.entries {
        let Ok(st) = store::load(&e.path) else {
            continue;
        };
        // Each file is shown in its own configured order, so `pd all` and `pd list` never
        // disagree about how the same project is sorted.
        let rows = render::tree_rows(&st.state, false, None, e.sort);
        if rows.is_empty() {
            continue;
        }

        if ctx.json {
            let paths = st.state.paths();
            let tasks: Vec<Value> = rows
                .iter()
                .filter_map(|r| st.state.get(&r.id))
                .map(|t| task_json(t, paths.path_of(&t.id)))
                .collect();
            files.push(json!({ "file": e.path, "seq": st.state.last_seq, "tasks": tasks }));
        } else {
            if printed {
                println!();
            }
            printed = true;
            println!("{}", ctx.style.accent(&label_for(&e.path)));
            println!("{}", render::rows(&rows, ctx.style, ctx.now));
        }
    }

    if ctx.json {
        ctx.emit(json!({ "files": files }));
    } else if !printed {
        println!("{}", ctx.style.dim("nothing open anywhere"));
    }
    Ok(())
}

fn label_for(path: &Path) -> String {
    let dir = path.parent().unwrap_or(Path::new("/"));
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    match dir.strip_prefix(&home) {
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => dir.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_like_targets_are_recognised() {
        assert!(is_path_like("2"));
        assert!(is_path_like("2.1.3"));
        assert!(!is_path_like("a7f"));
        assert!(!is_path_like(""));
    }

    #[test]
    fn the_schema_lists_every_command() {
        let schema = command_schema();
        let names: Vec<&str> = schema["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        for expected in [
            "add", "done", "reopen", "drop", "undo", "edit", "pri", "due", "mv", "note", "list",
            "all", "log", "compact", "config", "files",
        ] {
            assert!(names.contains(&expected), "schema is missing {expected}");
        }
        assert!(schema["exit_codes"]["3"].is_string());
    }
}
