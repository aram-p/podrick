//! End-to-end tests of the contract an agent depends on: exit codes, the JSON shape,
//! the staleness guard, and the stdout/stderr split.

use std::path::Path;
use std::process::Output;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// A sandboxed project directory with its own HOME, so the registry and config of the
/// machine running the tests are never touched.
struct Sandbox {
    dir: TempDir,
}

impl Sandbox {
    fn new() -> Sandbox {
        let dir = TempDir::new().expect("tempdir");
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .expect("git init");
        Sandbox { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Where `dirs::config_dir()` lands given the sandboxed `HOME` and `XDG_CONFIG_HOME`.
    /// Both CI platforms run these tests, so both answers have to be right.
    fn config_dir(&self) -> std::path::PathBuf {
        if cfg!(target_os = "macos") {
            self.path().join("Library/Application Support/podrick")
        } else {
            self.path().join("cfg/podrick")
        }
    }

    /// Likewise for `dirs::data_dir()`, which on macOS shares that same directory.
    fn data_dir(&self) -> std::path::PathBuf {
        if cfg!(target_os = "macos") {
            self.path().join("Library/Application Support/podrick")
        } else {
            self.path().join("data/podrick")
        }
    }

    /// The sandboxed environment every invocation needs. Tests that need to `spawn`
    /// several at once take this; everything else takes [`Sandbox::pd`]. Neither builds
    /// the environment by hand — an omitted `XDG_*` silently escapes the sandbox.
    fn cmd(&self) -> std::process::Command {
        let mut c = std::process::Command::new(assert_cmd::cargo::cargo_bin("pd"));
        c.current_dir(self.dir.path())
            .env("HOME", self.dir.path())
            .env("XDG_DATA_HOME", self.dir.path().join("data"))
            .env("XDG_CONFIG_HOME", self.dir.path().join("cfg"))
            .env("PODRICK_NOW", "2026-08-12T14:30:00+04:00")
            .env_remove("NO_COLOR");
        c
    }

    fn pd(&self) -> Command {
        Command::from_std(self.cmd())
    }

    /// Run and parse the JSON payload, asserting success.
    fn json(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "`pd {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "`pd {}` did not emit JSON ({e}): {:?}",
                args.join(" "),
                String::from_utf8_lossy(&out.stdout)
            )
        })
    }

    fn run(&self, args: &[&str]) -> Output {
        self.pd().args(args).output().expect("run pd")
    }

    /// Add a task and return its permanent id.
    fn add(&self, args: &[&str]) -> String {
        let mut full = vec!["--here", "add"];
        full.extend_from_slice(args);
        full.push("--json");
        self.json(&full)["task"]["id"]
            .as_str()
            .expect("an id")
            .to_string()
    }
}

// ---------------------------------------------------------------------------

#[test]
fn an_agent_is_refused_rather_than_littering_the_filesystem() {
    let s = Sandbox::new();
    let out = s.run(&["add", "hello"]);
    assert_eq!(out.status.code(), Some(1), "should exit 1 (not found)");
    assert!(
        !s.path().join(".podrick").exists(),
        "no file may be created"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--here"),
        "the error must name the fix: {stderr}"
    );
}

#[test]
fn here_creates_the_file_and_returns_the_task() {
    let s = Sandbox::new();
    let v = s.json(&["--here", "add", "fix", "the", "flaky", "test", "--json"]);
    assert_eq!(v["task"]["text"], "fix the flaky test");
    assert_eq!(v["task"]["state"], "open");
    assert_eq!(v["task"]["path"], "1");
    assert!(v["task"]["id"].as_str().is_some_and(|s| s.len() == 3));
    assert!(s.path().join(".podrick").exists());
}

#[test]
fn flags_are_not_swallowed_by_unquoted_text() {
    let s = Sandbox::new();
    let v = s.json(&[
        "--here",
        "add",
        "ship",
        "the",
        "migration",
        "-p2",
        "-d",
        "fri",
        "--json",
    ]);
    assert_eq!(v["task"]["text"], "ship the migration");
    assert_eq!(v["task"]["priority"], 2);
    assert_eq!(v["task"]["due"], "2026-08-14");
}

/// `list` is the implied command, so its own flags have to work without naming it —
/// including the short ones, which clap will not accept ahead of a subcommand.
#[test]
fn list_flags_work_without_naming_list() {
    let s = Sandbox::new();
    let id = s.add(&["archive the old logs"]);
    s.run(&["done", &id]);
    s.add(&["decide on the TUI"]);

    let open = s.json(&["-o", "--json"]);
    assert_eq!(open["tasks"].as_array().unwrap().len(), 1, "-o hides done");

    for form in [
        vec!["--json"],
        vec!["-a", "--json"],
        vec!["--all", "--json"],
        vec!["list", "-a", "--json"],
    ] {
        let v = s.json(&form);
        assert_eq!(
            v["tasks"].as_array().unwrap().len(),
            2,
            "`pd {}` should show the done task too",
            form.join(" ")
        );
    }

    // A short flag *and* a filter: the inserted `list` has to land in front of both.
    let v = s.json(&["-a", "archive", "--json"]);
    let texts = order(&v);
    assert_eq!(texts, vec!["archive the old logs"], "filter still applied");

    // `--help` describes pd, not pd list.
    let out = s.run(&["--help"]);
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("Commands:"),
        "top-level help survives normalisation: {help:?}"
    );
}

/// Done is evidence of progress and belongs in the everyday view. Dropped is a different
/// claim — "decided against this" — and stays behind `--all`.
#[test]
fn done_shows_by_default_but_dropped_does_not() {
    let s = Sandbox::new();
    let finished = s.add(&["archive the old logs"]);
    let abandoned = s.add(&["rewrite it in Haskell"]);
    s.add(&["decide on the TUI"]);
    s.run(&["done", &finished]);
    s.run(&["drop", &abandoned]);

    let texts = order(&s.json(&["--json"]));
    assert_eq!(
        texts,
        vec!["decide on the TUI", "archive the old logs"],
        "the default carries done, under the open tree, and not dropped"
    );

    assert_eq!(
        order(&s.json(&["-o", "--json"])),
        vec!["decide on the TUI"],
        "-o is the old open-only view"
    );

    let all = order(&s.json(&["-a", "--json"]));
    assert!(
        all.contains(&"rewrite it in Haskell".to_string()),
        "--all reaches the dropped one: {all:?}"
    );
}

#[test]
fn every_payload_carries_the_file_and_seq() {
    let s = Sandbox::new();
    s.add(&["first"]);
    let v = s.json(&["list", "--json"]);
    assert!(v["file"].as_str().unwrap().ends_with(".podrick"));
    assert_eq!(v["seq"], 1);
}

#[test]
fn stdout_stays_clean_while_chatter_goes_to_stderr() {
    let s = Sandbox::new();
    let out = s
        .pd()
        .args(["--here", "add", "first", "--json"])
        .output()
        .unwrap();
    // The "created .podrick" note must not pollute the data channel.
    serde_json::from_slice::<Value>(&out.stdout).expect("stdout is pure JSON");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("created"),
        "the creation note belongs on stderr"
    );
}

#[test]
fn piped_output_has_no_escape_codes() {
    let s = Sandbox::new();
    s.add(&["first", "-p1"]);
    let out = s.run(&["list"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains('\x1b'),
        "piped output must be plain: {stdout:?}"
    );
}

// ---------------------------------------------------------------------------
// Identity and the staleness guard
// ---------------------------------------------------------------------------

#[test]
fn a_path_addressed_write_from_an_agent_is_refused_without_expect_seq() {
    let s = Sandbox::new();
    s.add(&["first"]);
    let out = s.run(&["done", "1"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "stale-path writes exit 3 (conflict)"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("--expect-seq"));
}

#[test]
fn a_path_addressed_write_succeeds_with_the_right_seq() {
    let s = Sandbox::new();
    s.add(&["first"]);
    let seq = s.json(&["list", "--json"])["seq"]
        .as_u64()
        .unwrap()
        .to_string();
    let out = s.run(&["done", "1", "--expect-seq", &seq]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_stale_seq_is_rejected() {
    let s = Sandbox::new();
    s.add(&["first"]);
    s.add(&["second"]); // the log moved on
    let out = s.run(&["done", "1", "--expect-seq", "1"]);
    assert_eq!(out.status.code(), Some(3));
}

#[test]
fn id_addressed_writes_never_need_a_seq() {
    let s = Sandbox::new();
    let id = s.add(&["first"]);
    let out = s.run(&["done", &id]);
    assert!(
        out.status.success(),
        "ids do not shift, so they are always safe"
    );
}

#[test]
fn an_unknown_target_exits_not_found() {
    let s = Sandbox::new();
    s.add(&["first"]);
    let out = s.run(&["done", "zzz"]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn errors_are_machine_readable_under_json() {
    let s = Sandbox::new();
    s.add(&["first"]);
    let out = s.pd().args(["done", "zzz", "--json"]).output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).expect("JSON error payload");
    assert_eq!(v["code"], 1);
    assert!(v["error"].as_str().is_some());
    assert!(v["hint"].as_str().is_some());
}

// ---------------------------------------------------------------------------
// The tree
// ---------------------------------------------------------------------------

#[test]
fn nesting_is_capped_at_four_levels() {
    let s = Sandbox::new();
    let mut parent = s.add(&["level1"]);
    for level in 2..=4 {
        parent = s.add(&[&format!("level{level}"), "--under", &parent]);
    }
    let out = s.run(&["add", "level5", "--under", &parent]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "too-deep nesting is a usage error"
    );
}

#[test]
fn completing_a_parent_cascades_and_logs_each_subtask() {
    let s = Sandbox::new();
    let parent = s.add(&["parent"]);
    s.add(&["child", "--under", &parent]);
    let child = s.json(&["list", "--json"])["tasks"][1]["id"]
        .as_str()
        .unwrap()
        .to_string();
    s.add(&["grandchild", "--under", &child]);

    let v = s.json(&["done", &parent, "--json"]);
    assert_eq!(v["cascaded"].as_array().unwrap().len(), 2);

    let events = s.json(&["log", "--json"])["events"]
        .as_array()
        .unwrap()
        .clone();
    let completed = events.iter().filter(|e| e["ev"] == "completed").count();
    assert_eq!(
        completed, 3,
        "every task's completion is its own ledger entry"
    );
}

#[test]
fn a_task_whose_parent_was_completed_stays_reachable() {
    let s = Sandbox::new();
    let parent = s.add(&["parent"]);
    let child = s.add(&["child", "--under", &parent]);
    s.run(&["done", &parent]);
    s.run(&["reopen", &child]);

    let tasks = s.json(&["list", "--json"])["tasks"]
        .as_array()
        .unwrap()
        .clone();
    let found = tasks
        .iter()
        .find(|t| t["id"] == child.as_str())
        .expect("child is listed");
    assert_eq!(
        found["path"], "1",
        "it surfaces as a root rather than vanishing"
    );
}

#[test]
fn moving_a_task_into_its_own_subtree_is_refused() {
    let s = Sandbox::new();
    let parent = s.add(&["parent"]);
    let child = s.add(&["child", "--under", &parent]);
    let out = s.run(&["mv", &parent, "--under", &child]);
    assert_eq!(out.status.code(), Some(2));
}

// ---------------------------------------------------------------------------
// The ledger
// ---------------------------------------------------------------------------

#[test]
fn nothing_is_ever_deleted_from_the_log() {
    let s = Sandbox::new();
    let id = s.add(&["first"]);
    s.run(&["done", &id, "-m", "shipped"]);
    s.run(&["reopen", &id]);
    s.run(&["done", &id]);

    let events = s.json(&["log", "--json"])["events"]
        .as_array()
        .unwrap()
        .clone();
    let kinds: Vec<&str> = events.iter().map(|e| e["ev"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["created", "completed", "uncompleted", "completed"]);
    assert_eq!(events[1]["note"], "shipped");
}

#[test]
fn events_record_who_did_them() {
    let s = Sandbox::new();
    s.add(&["first"]);
    let events = s.json(&["log", "--json"])["events"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(events[0]["actor"], "agent", "no TTY in a test harness");
}

#[test]
fn dropped_is_distinct_from_completed() {
    let s = Sandbox::new();
    let id = s.add(&["first"]);
    s.run(&["drop", &id, "-m", "decided against it"]);
    let v = s.json(&["list", "--all", "--json"]);
    assert_eq!(v["tasks"][0]["state"], "dropped");
}

#[test]
fn undo_reverts_a_cascade_as_one_action() {
    let s = Sandbox::new();
    let parent = s.add(&["parent"]);
    s.add(&["child", "--under", &parent]);
    s.run(&["done", &parent]);

    let v = s.json(&["undo", "--json"]);
    assert_eq!(
        v["events_reverted"], 2,
        "half a cascade is never a valid state"
    );

    let tasks = s.json(&["list", "--json"])["tasks"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(tasks.len(), 2, "the whole subtree is open again");
}

#[test]
fn undo_walks_backwards_through_the_log() {
    let s = Sandbox::new();
    let id = s.add(&["first"]);
    s.run(&["pri", &id, "p1"]);
    s.run(&["done", &id]);

    s.run(&["undo"]); // the completion
    assert_eq!(s.json(&["list", "--json"])["tasks"][0]["state"], "open");
    s.run(&["undo"]); // the priority change
    assert_eq!(
        s.json(&["list", "--json"])["tasks"][0]["priority"],
        Value::Null
    );
}

#[test]
fn a_creation_cannot_be_undone() {
    let s = Sandbox::new();
    s.add(&["first"]);
    let out = s.run(&["undo"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("drop"));
}

#[test]
fn compact_archives_the_log_and_preserves_state() {
    let s = Sandbox::new();
    let id = s.add(&["first"]);
    s.add(&["second"]);
    s.run(&["done", &id]);

    let before = s.json(&["list", "--all", "--json"])["tasks"]
        .as_array()
        .unwrap()
        .len();
    let v = s.json(&["compact", "--json"]);
    assert_eq!(v["events_archived"], 3);
    assert!(s.path().join(".podrick.archive").exists());

    let after = s.json(&["list", "--all", "--json"])["tasks"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(before, after, "compaction changes the log, never the state");
}

// ---------------------------------------------------------------------------
// Sorting, config, discovery
// ---------------------------------------------------------------------------

#[test]
fn insertion_order_is_the_default_and_sorting_is_one_shot() {
    let s = Sandbox::new();
    s.add(&["zebra", "-p4"]);
    s.add(&["apple", "-p1"]);

    let default: Vec<String> = order(&s.json(&["list", "--json"]));
    assert_eq!(
        default,
        ["zebra", "apple"],
        "insertion order wins by default"
    );

    let sorted: Vec<String> = order(&s.json(&["list", "--sort", "priority", "--json"]));
    assert_eq!(sorted, ["apple", "zebra"]);

    let after: Vec<String> = order(&s.json(&["list", "--json"]));
    assert_eq!(after, ["zebra", "apple"], "a one-shot sort must not stick");
}

#[test]
fn a_project_sort_overrides_the_global_one() {
    let s = Sandbox::new();
    s.add(&["zebra"]);
    s.add(&["apple"]);
    s.run(&["config", "sort", "created"]);
    s.run(&["config", "sort", "alpha", "--project"]);

    let v = s.json(&["config", "--json"]);
    assert_eq!(v["sort"]["global"], "created");
    assert_eq!(v["sort"]["project"], "alpha");
    assert_eq!(v["sort"]["effective"], "alpha");
    assert_eq!(order(&s.json(&["list", "--json"])), ["apple", "zebra"]);
}

#[test]
fn an_unknown_sort_key_is_a_usage_error() {
    let s = Sandbox::new();
    s.add(&["first"]);
    assert_eq!(s.run(&["list", "--sort", "banana"]).status.code(), Some(2));
}

/// `--project` used to be spelled `--here`, which is also a global flag meaning "skip
/// discovery and use cwd". One token set both, so scoping a setting to the project you
/// were actually working in was impossible whenever the file came from discovery — the
/// global sense forced resolution back to cwd, where there was no file.
#[test]
fn a_project_setting_works_against_a_file_found_by_discovery() {
    let s = Sandbox::new();
    let proj = s.path().join("proj");
    std::fs::create_dir(&proj).unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&proj)
        .status()
        .expect("git init");

    let out = s
        .pd()
        .current_dir(&proj)
        .args(["--here", "add", "zebra", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let out = s
        .pd()
        .current_dir(&proj)
        .args(["add", "apple", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());

    // From the parent, the file is reachable only through the registry.
    assert_eq!(order(&s.json(&["list", "--json"])), ["zebra", "apple"]);

    let out = s.run(&["config", "sort", "alpha", "--project"]);
    assert!(
        out.status.success(),
        "scoping to an adopted project must work: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // It attached to *that* file's entry, not to a new one in cwd.
    assert!(!s.path().join(".podrick").exists(), "no stray file created");
    assert_eq!(order(&s.json(&["list", "--json"])), ["apple", "zebra"]);
    assert_eq!(s.json(&["config", "--json"])["sort"]["project"], "alpha");
}

/// Same bad value, same answer, whichever layer it came from. It used to be exit 2 from
/// the flag and a silent exit 0 from the file.
#[test]
fn an_unknown_sort_key_in_the_config_file_fails_like_the_flag() {
    let s = Sandbox::new();
    s.add(&["zebra"]);
    s.add(&["apple"]);

    let cfg = s.config_dir();
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join("config.toml"), "sort = \"banana\"\n").unwrap();

    let out = s.run(&["list"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a bad persisted key must not pass"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("banana"), "names the offending value: {err:?}");

    // And a good one still works, so the check is not just refusing everything.
    std::fs::write(cfg.join("config.toml"), "sort = \"alpha\"\n").unwrap();
    assert_eq!(order(&s.json(&["list", "--json"])), ["apple", "zebra"]);
}

/// A priority filter keeps the tree. A p1 subtask of a p2 task is still that task's
/// subtask, and rendering it at the same indent as unrelated p1 roots says otherwise.
/// The parent comes along as context, exactly as it does for a text search.
#[test]
fn a_priority_filter_keeps_matches_under_their_real_parent() {
    let s = Sandbox::new();
    s.add(&["zebra", "-p1"]);
    let parent = s.add(&["dee parent", "-p2"]);
    s.add(&["ekko child", "-p1", "--under", &parent]);
    s.add(&["apple", "-p1"]);

    // Unfiltered, the tree is intact and only siblings move.
    assert_eq!(
        order(&s.json(&["list", "--sort", "alpha", "--json"])),
        ["apple", "dee parent", "ekko child", "zebra"]
    );

    // Filtered, the p2 parent stays — but only because a descendant matched.
    assert_eq!(
        order(&s.json(&["list", "-p", "p1", "--sort", "alpha", "--json"])),
        ["apple", "dee parent", "ekko child", "zebra"]
    );

    // And it is still rendered as the child's parent, not as a peer.
    let out = s.run(&["list", "-p", "p1", "--sort", "alpha", "--no-color"]);
    let text = String::from_utf8_lossy(&out.stdout);
    let child = text
        .lines()
        .find(|l| l.contains("ekko child"))
        .expect("the match is listed");
    assert!(
        child.split('\t').nth(3).unwrap_or("").starts_with(' '),
        "the match must still render indented under its parent: {child:?}"
    );
}

/// The parent is context, not a result — it must not drag its other children in with it.
#[test]
fn a_priority_filter_does_not_widen_to_the_whole_subtree() {
    let s = Sandbox::new();
    let parent = s.add(&["parent", "-p3"]);
    s.add(&["urgent", "-p1", "--under", &parent]);
    s.add(&["not urgent", "-p4", "--under", &parent]);

    assert_eq!(
        order(&s.json(&["list", "-p", "p1", "--json"])),
        ["parent", "urgent"]
    );
}

#[test]
fn search_keeps_a_matching_child_visible_under_its_parent() {
    let s = Sandbox::new();
    let parent = s.add(&["parent"]);
    s.add(&["find me", "--under", &parent]);
    s.add(&["unrelated"]);

    let tasks = s.json(&["find me", "--json"])["tasks"]
        .as_array()
        .unwrap()
        .clone();
    let texts: Vec<&str> = tasks.iter().map(|t| t["text"].as_str().unwrap()).collect();
    assert_eq!(texts, ["parent", "find me"], "a match is never orphaned");
}

#[test]
fn the_file_is_found_by_walking_up_from_a_subdirectory() {
    let s = Sandbox::new();
    s.add(&["first"]);
    let sub = s.path().join("deep/nested");
    std::fs::create_dir_all(&sub).unwrap();

    let out = s
        .pd()
        .current_dir(&sub)
        .args(["list", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["tasks"].as_array().unwrap().len(), 1);
}

#[test]
fn the_schema_describes_the_whole_surface_in_one_call() {
    let s = Sandbox::new();
    let out = s.pd().args(["--help", "--json"]).output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).expect("schema JSON");
    let names: Vec<&str> = v["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    for expected in ["add", "done", "reopen", "drop", "undo", "log", "compact"] {
        assert!(names.contains(&expected), "schema is missing {expected}");
    }
    assert_eq!(v["exit_codes"]["3"], "conflict (stale path)");
}

#[test]
fn concurrent_writers_produce_a_well_formed_log() {
    let s = Sandbox::new();
    s.add(&["first"]);

    let mut kids = Vec::new();
    for i in 0..8 {
        let mut c = s.cmd();
        c.args(["add", &format!("task {i}")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        kids.push(c.spawn().expect("spawn"));
    }
    for mut k in kids {
        k.wait().expect("wait");
    }

    let contents = std::fs::read_to_string(s.path().join(".podrick")).unwrap();
    let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 9, "every write landed");
    for l in &lines {
        serde_json::from_str::<Value>(l).expect("no torn lines under concurrency");
    }
}

/// The ledger has always been locked. The registry was not, and every `pd` rewrites it
/// wholesale — even a bare `pd list`, which refreshes `last_used`. Eight processes each
/// registering a different file is a plain lost-update race: each loads, adds its own
/// entry, and writes the whole file back over everyone else's.
#[test]
fn concurrent_writers_cannot_lose_a_registry_entry() {
    let s = Sandbox::new();

    // Each needs its own repo: `--here` means "this project", and inside one repo every
    // subdirectory resolves to the same file at its root.
    let names: Vec<String> = (0..8).map(|i| format!("p{i}")).collect();
    for name in &names {
        let dir = s.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .status()
            .expect("git init");
    }

    let mut kids = Vec::new();
    for name in &names {
        let mut c = s.cmd();
        c.current_dir(s.path().join(name))
            .args(["--here", "add", name])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        kids.push(c.spawn().expect("spawn"));
    }
    for mut k in kids {
        k.wait().expect("wait");
    }

    let contents =
        std::fs::read_to_string(s.data_dir().join("registry.jsonl")).expect("registry exists");
    let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
    for l in &lines {
        serde_json::from_str::<Value>(l).expect("no torn lines under concurrency");
    }
    assert_eq!(lines.len(), names.len(), "no registration lost: {contents}");

    // And `pd files` agrees, which is the way a user would notice.
    let listed = s.json(&["files", "--json"]);
    assert_eq!(listed["files"].as_array().unwrap().len(), names.len());
}

/// Discovery is helpful but it is also surprising, so it announces itself — but only when
/// it matters. A read that lands on an adopted file changes nothing; a write to one you
/// did not expect is the whole reason the notice exists.
#[test]
fn an_adopted_file_is_announced_on_writes_and_kept_quiet_on_reads() {
    let s = Sandbox::new();
    let proj = s.path().join("proj");
    std::fs::create_dir(&proj).unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&proj)
        .status()
        .expect("git init");

    // Register a file that lives one level down, then act from the parent, where walking
    // up finds nothing and only the registry can help.
    let id = {
        let out = s
            .pd()
            .current_dir(&proj)
            .args(["--here", "add", "write the parser", "--json"])
            .output()
            .expect("run pd");
        assert!(out.status.success());
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        v["task"]["id"].as_str().unwrap().to_string()
    };
    assert!(proj.join(".podrick").exists(), "file landed in proj/");

    let stderr = |args: &[&str]| -> String {
        let out = s.pd().args(args).output().expect("run pd");
        assert!(
            out.status.success(),
            "`pd {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stderr).to_string()
    };

    let read = stderr(&["list"]);
    assert!(
        !read.contains("same directory tree"),
        "a read must not narrate discovery: {read:?}"
    );

    let write = stderr(&["note", &id, "-m", "still here"]);
    assert!(
        write.contains("same directory tree"),
        "a write must say where it landed: {write:?}"
    );
    assert!(
        write.contains("proj"),
        "the notice names the project it adopted: {write:?}"
    );
    assert!(
        !write.contains(".podrick"),
        "the notice shows the project, not the dotfile: {write:?}"
    );
}

/// A global flag that takes a value used to swallow the subcommand after it: argv
/// normalisation asked clap which flags take values, clap had not been built yet and so
/// answered "none", and `pd --file x add hi` became `pd list --file x add hi` — a search
/// for "add hi" that quietly added nothing.
#[test]
fn a_value_taking_global_flag_does_not_swallow_the_command_after_it() {
    let s = Sandbox::new();
    let file = s.path().join("side.podrick");
    let file = file.to_str().unwrap();

    for form in [
        vec!["--file", file, "add", "written"],
        vec!["-f", file, "add", "written twice"],
    ] {
        let out = s.pd().args(&form).output().expect("run pd");
        assert!(
            out.status.success(),
            "`pd {}` failed: {}",
            form.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let v = s.json(&["--file", file, "list", "--json"]);
    assert_eq!(
        order(&v),
        ["written", "written twice"],
        "both adds have to have landed"
    );

    // And the implied `list` still gets inserted when there really is no subcommand.
    assert_eq!(
        order(&s.json(&["--file", file, "twice", "--json"])),
        ["written twice"],
        "a bare filter after a value flag is still a search"
    );
}

#[test]
fn a_batch_is_one_undoable_action() {
    let s = Sandbox::new();
    let a = s.add(&["one"]);
    let b = s.add(&["two"]);
    let c = s.add(&["three"]);

    let ops = format!(
        "{{\"op\":\"edit\",\"id\":\"{a}\",\"text\":\"One\"}}\n\
         {{\"op\":\"edit\",\"id\":\"{b}\",\"text\":\"Two\"}}\n\
         {{\"op\":\"pri\",\"id\":\"{c}\",\"priority\":\"p1\"}}\n"
    );
    let out = s.pd().arg("batch").write_stdin(ops).output().expect("run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(order(&s.json(&["list", "--json"])), ["One", "Two", "three"]);

    // One undo, not three. The point of the feature.
    let v = s.json(&["undo", "--json"]);
    assert_eq!(v["events_reverted"], 3);
    assert_eq!(
        order(&s.json(&["list", "--json"])),
        ["one", "two", "three"],
        "every change goes back together"
    );
    assert_eq!(
        s.json(&["list", "--json"])["tasks"][2]["priority"],
        Value::Null
    );
}

#[test]
fn a_batch_that_fails_anywhere_writes_nothing() {
    let s = Sandbox::new();
    let a = s.add(&["one"]);
    let b = s.add(&["two"]);
    let before = std::fs::read_to_string(s.path().join(".podrick")).unwrap();

    let ops = format!(
        "{{\"op\":\"edit\",\"id\":\"{a}\",\"text\":\"One\"}}\n\
         {{\"op\":\"pri\",\"id\":\"{b}\",\"priority\":\"p9\"}}\n"
    );
    let out = s.pd().arg("batch").write_stdin(ops).output().expect("run");
    assert_eq!(out.status.code(), Some(2), "a bad op is a usage error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("line 2"), "name the line: {stderr}");

    assert_eq!(
        std::fs::read_to_string(s.path().join(".podrick")).unwrap(),
        before,
        "line 1 must not have landed"
    );
}

#[test]
fn a_batch_operation_sees_what_the_ones_before_it_did() {
    let s = Sandbox::new();
    let parent = s.add(&["parent"]);
    let child = s.add(&["child", "--under", &parent]);

    // `reopen` on an already-open task is a no-op and is skipped. Here it is not a no-op,
    // because the `done` on the line above cascaded the child closed first — which only
    // holds if each op is judged against the state its predecessors leave behind.
    let ops = format!(
        "{{\"op\":\"done\",\"id\":\"{parent}\"}}\n\
         {{\"op\":\"reopen\",\"id\":\"{child}\"}}\n"
    );
    let out = s.pd().arg("batch").write_stdin(ops).output().expect("run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let tasks = s.json(&["list", "--all", "--json"])["tasks"].clone();
    let state = |id: &str| -> String {
        tasks
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .expect("task")["state"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(state(&parent), "done");
    assert_eq!(
        state(&child),
        "open",
        "the reopen took effect after the cascade"
    );

    // And all three events — the cascade and the reopen — are still one action.
    assert_eq!(s.json(&["undo", "--json"])["events_reverted"], 3);
}

fn order(v: &Value) -> Vec<String> {
    v["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["text"].as_str().unwrap().to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// People
// ---------------------------------------------------------------------------

/// The people currently on a task, straight off the JSON contract — display forms, which
/// is what the payload carries.
fn people_of(v: &Value) -> Vec<String> {
    v["people"]
        .as_array()
        .expect("a task always carries `people`")
        .iter()
        .map(|p| p.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn linking_a_person_is_idempotent() {
    let s = Sandbox::new();
    let id = s.add(&["order the brake pads"]);

    let v = s.json(&["who", &id, "meri", "--json"]);
    assert_eq!(people_of(&v["task"]), ["Meri"], "shown capitalised");
    assert_eq!(v["changed"].as_array().unwrap().len(), 1);

    // Again, and in another spelling: one person, one link, and nothing new in the log.
    let again = s.json(&["who", &id, "MERI", "--json"]);
    assert_eq!(people_of(&again["task"]), ["Meri"]);
    assert!(
        again["changed"].as_array().unwrap().is_empty(),
        "a re-link appends nothing"
    );
    assert_eq!(
        again["seq"], v["seq"],
        "and so does not move the log forward"
    );

    let linked = s.json(&["log", "--json"])["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["ev"] == "linked")
        .count();
    assert_eq!(linked, 1, "exactly one `linked` event exists");
}

#[test]
fn a_task_carries_many_people_and_gives_them_up_one_at_a_time() {
    let s = Sandbox::new();
    let id = s.add(&["chase the car-parts supplier"]);
    s.run(&["who", &id, "meri", "gegham", "anna"]);

    let v = s.json(&["list", "--json"]);
    assert_eq!(people_of(&v["tasks"][0]), ["Meri", "Gegham", "Anna"]);

    let after = s.json(&["who", &id, "--remove", "gegham", "--json"]);
    assert_eq!(people_of(&after["task"]), ["Meri", "Anna"]);
    assert_eq!(after["action"], "unlinked");

    // Unlinking someone who was never there is a no-op, not an error.
    let noop = s.run(&["who", &id, "--remove", "suren"]);
    assert!(noop.status.success());
    assert_eq!(
        people_of(&s.json(&["list", "--json"])["tasks"][0]),
        ["Meri", "Anna"]
    );
}

#[test]
fn names_are_trimmed_and_collapsed_but_keep_their_capitals() {
    let s = Sandbox::new();
    let id = s.add(&["file the insurance claim"]);
    s.run(&["who", &id, "  Meri   Sargsyan "]);
    assert_eq!(
        people_of(&s.json(&["list", "--json"])["tasks"][0]),
        ["Meri Sargsyan"]
    );
}

/// The first spelling wins, so a name someone took care over is not undone by the next
/// person to type it in a hurry. Title-casing at render time would mangle all three of
/// these, which is why the display form is stored rather than derived.
#[test]
fn a_typed_capitalisation_is_kept_verbatim_and_never_re_cased() {
    let s = Sandbox::new();
    for (i, name) in ["McDonald", "de Vries", "O'Brien"].iter().enumerate() {
        let id = s.add(&[&format!("task {i}")]);
        s.run(&["who", &id, name]);
        // A second link in another spelling finds the same person and changes nothing.
        s.run(&["who", &id, &name.to_lowercase()]);
        assert_eq!(
            people_of(&s.json(&["list", "--json"])["tasks"][i]),
            [*name],
            "the first spelling stands"
        );
    }
    let names: Vec<String> = s.json(&["people", "--json"])["people"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect();
    // Ties are broken on the folded key, so a capital letter never moves anybody:
    // "de vries" sorts before "mcdonald" before "o'brien".
    assert_eq!(names, ["de Vries", "McDonald", "O'Brien"]);
}

/// Only the display changes, so every spelling still finds them.
#[test]
fn matching_ignores_case_however_the_name_is_shown() {
    let s = Sandbox::new();
    let id = s.add(&["order the brake pads"]);
    s.run(&["who", &id, "McDonald"]);
    for spelling in ["McDonald", "mcdonald", "MCDONALD", "  McDonald  "] {
        let v = s.json(&["list", "--person", spelling, "--json"]);
        assert_eq!(
            v["tasks"].as_array().unwrap().len(),
            1,
            "{spelling:?} has to find them"
        );
        assert_eq!(people_of(&v["tasks"][0]), ["McDonald"]);
    }
}

/// Fixing a capitalisation is a rename, and it must not be swallowed as a no-op: the
/// folded identity does not move, but the spelling does.
#[test]
fn a_rename_that_only_changes_the_case_still_lands() {
    let s = Sandbox::new();
    let id = s.add(&["order the brake pads"]);
    s.run(&["who", &id, "mcdonald"]);
    assert_eq!(
        people_of(&s.json(&["list", "--json"])["tasks"][0]),
        ["Mcdonald"],
        "the lower-case default gets it wrong, which is what the rename is for"
    );

    let v = s.json(&["people", "rename", "mcdonald", "McDonald", "--json"]);
    assert_eq!(v["to"], "McDonald");
    assert_eq!(
        people_of(&s.json(&["list", "--json"])["tasks"][0]),
        ["McDonald"]
    );
    // Still one person, still matched by any spelling.
    assert_eq!(
        s.json(&["people", "--json"])["people"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        s.json(&["list", "--person", "MCDONALD", "--json"])["tasks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Once it is pinned, saying it again is the no-op it looks like.
    let out = s.run(&["people", "rename", "mcdonald", "McDonald"]);
    assert_eq!(out.status.code(), Some(2));

    // And undo puts the old spelling back, not merely the old identity.
    s.run(&["undo"]);
    assert_eq!(
        people_of(&s.json(&["list", "--json"])["tasks"][0]),
        ["Mcdonald"]
    );
}

/// A ledger written by a build that had no display forms at all. Every name in it is
/// folded, and it has to come back reading like a name.
#[test]
fn a_ledger_without_display_forms_replays_capitalised() {
    let s = Sandbox::new();
    let log = concat!(
        r#"{"v":1,"seq":1,"ts":"2026-01-01T10:00:00+04:00","actor":"cli","ev":"created","id":"a7f","data":{"text":"order the brake pads"}}"#,
        "
",
        r#"{"v":1,"seq":2,"ts":"2026-01-01T10:01:00+04:00","actor":"cli","ev":"linked","id":"a7f","data":{"person":"meri sargsyan"}}"#,
        "
",
        r#"{"v":1,"seq":3,"ts":"2026-01-01T10:02:00+04:00","actor":"cli","ev":"linked","id":"a7f","data":{"person":"gegham"}}"#,
        "
",
        r#"{"v":1,"seq":4,"ts":"2026-01-01T10:03:00+04:00","actor":"cli","ev":"renamed","id":"","data":{"from":"gegham","to":"anna"}}"#,
        "
",
    );
    let path = s.path().join("old.podrick");
    std::fs::write(&path, log).unwrap();
    let file = path.to_str().unwrap();

    let v = s.json(&["-f", file, "list", "--json"]);
    assert_eq!(people_of(&v["tasks"][0]), ["Meri Sargsyan", "Anna"]);
    let people = s.json(&["-f", file, "people", "--json"])["people"].clone();
    assert_eq!(people[0]["name"], "Anna");
    assert_eq!(people[1]["name"], "Meri Sargsyan");

    // And it keeps working: a new link in a chosen spelling pins that spelling.
    s.run(&["-f", file, "who", "a7f", "McDonald"]);
    assert_eq!(
        people_of(&s.json(&["-f", file, "list", "--json"])["tasks"][0]),
        ["Meri Sargsyan", "Anna", "McDonald"]
    );
}

#[test]
fn the_people_list_is_derived_and_ordered_by_what_is_open() {
    let s = Sandbox::new();
    let a = s.add(&["brake pads"]);
    let b = s.add(&["wing mirror"]);
    let c = s.add(&["service book"]);
    let d = s.add(&["winter tyres"]);
    let e = s.add(&["headlight bulb"]);
    s.run(&["who", &a, "meri"]);
    s.run(&["who", &e, "meri"]);
    s.run(&["who", &b, "meri"]);
    s.run(&["who", &c, "meri"]);
    s.run(&["who", &d, "gegham"]);
    s.run(&["done", &b]);
    s.run(&["drop", &c]);

    // Busiest first. The dropped task counts towards neither total — "decided against
    // this" is not work outstanding and not work done.
    let people = s.json(&["people", "--json"])["people"].clone();
    assert_eq!(people[0]["name"], "Meri");
    assert_eq!(people[0]["open"], 2);
    assert_eq!(people[0]["done"], 1);
    assert_eq!(people[1]["name"], "Gegham");
    assert_eq!(people[1]["open"], 1);
    assert_eq!(people[1]["done"], 0);
    assert_eq!(
        people.as_array().unwrap().len(),
        2,
        "nobody exists but the people some task names"
    );
}

#[test]
fn renaming_a_person_carries_every_link_with_it() {
    let s = Sandbox::new();
    let a = s.add(&["brake pads"]);
    let b = s.add(&["wing mirror"]);
    s.run(&["who", &a, "meri"]);
    s.run(&["who", &b, "meri", "gegham"]);

    let v = s.json(&["people", "rename", "meri", "Meri Sargsyan", "--json"]);
    assert_eq!(v["tasks"], 2);
    assert_eq!(v["from"], "Meri");
    assert_eq!(v["to"], "Meri Sargsyan");

    let tasks = s.json(&["list", "--json"])["tasks"].clone();
    assert_eq!(people_of(&tasks[0]), ["Meri Sargsyan"]);
    assert_eq!(people_of(&tasks[1]), ["Meri Sargsyan", "Gegham"]);

    // Renaming onto a name that is already there merges rather than duplicating, and the
    // spelling typed for the target is the one that stands.
    s.run(&["people", "rename", "gegham", "meri sargsyan"]);
    let tasks = s.json(&["list", "--json"])["tasks"].clone();
    assert_eq!(people_of(&tasks[1]), ["Meri Sargsyan"]);

    let out = s.run(&["people", "rename", "nobody", "somebody"]);
    assert_eq!(out.status.code(), Some(1), "an unknown name is not found");
}

#[test]
fn removing_a_person_unlinks_them_everywhere_as_one_action() {
    let s = Sandbox::new();
    let a = s.add(&["brake pads"]);
    let b = s.add(&["wing mirror"]);
    s.run(&["who", &a, "meri"]);
    s.run(&["who", &b, "meri", "gegham"]);

    let v = s.json(&["people", "remove", "meri", "--json"]);
    assert_eq!(v["tasks"], 2);
    assert_eq!(v["removed"], "Meri");
    assert_eq!(v["people"][0]["name"], "Gegham");

    let tasks = s.json(&["list", "--json"])["tasks"].clone();
    assert!(people_of(&tasks[0]).is_empty());
    assert_eq!(people_of(&tasks[1]), ["Gegham"]);

    // One action, so one undo puts both links back.
    assert_eq!(s.json(&["undo", "--json"])["events_reverted"], 2);
    let tasks = s.json(&["list", "--json"])["tasks"].clone();
    assert_eq!(people_of(&tasks[0]), ["Meri"]);
    // Link order is the order people were linked in, not a ranking: a restored link
    // lands at the end, where a fresh one would.
    assert_eq!(people_of(&tasks[1]), ["Gegham", "Meri"]);
}

#[test]
fn the_person_filter_keeps_a_non_matching_parent_as_context() {
    let s = Sandbox::new();
    let parent = s.add(&["sell the car"]);
    let child = s.add(&["order the brake pads", "--under", &parent]);
    s.add(&["unrelated errand"]);
    s.run(&["who", &child, "meri"]);

    let texts = order(&s.json(&["list", "--person", "meri", "--json"]));
    assert_eq!(
        texts,
        ["sell the car", "order the brake pads"],
        "a match is shown under its real parent, and nothing else comes along"
    );

    let none = s.run(&["list", "--person", "suren"]);
    assert!(none.status.success(), "an unknown person is an empty list");
    assert!(String::from_utf8_lossy(&none.stdout).contains("@Suren"));
}

#[test]
fn undo_reverts_a_link_an_unlink_and_a_rename() {
    let s = Sandbox::new();
    let id = s.add(&["brake pads"]);

    s.run(&["who", &id, "meri", "gegham"]);
    s.run(&["who", &id, "--remove", "gegham"]);
    s.run(&["people", "rename", "meri", "mary"]);
    assert_eq!(
        people_of(&s.json(&["list", "--json"])["tasks"][0]),
        ["Mary"]
    );

    s.run(&["undo"]); // the rename
    assert_eq!(
        people_of(&s.json(&["list", "--json"])["tasks"][0]),
        ["Meri"]
    );
    s.run(&["undo"]); // the unlink
    assert_eq!(
        people_of(&s.json(&["list", "--json"])["tasks"][0]),
        ["Meri", "Gegham"]
    );
    // The two links were one action, so they go back together.
    assert_eq!(s.json(&["undo", "--json"])["events_reverted"], 2);
    assert!(people_of(&s.json(&["list", "--json"])["tasks"][0]).is_empty());
}

#[test]
fn compaction_carries_the_links_forward() {
    let s = Sandbox::new();
    let a = s.add(&["brake pads"]);
    let b = s.add(&["wing mirror"]);
    s.run(&["who", &a, "meri", "McDonald"]);
    s.run(&["who", &b, "meri"]);
    s.run(&["people", "rename", "meri", "Meri S"]);

    let before = s.json(&["people", "--json"])["people"].clone();
    s.json(&["compact", "--json"]);

    assert_eq!(
        s.json(&["people", "--json"])["people"],
        before,
        "the snapshot is the state, links included"
    );
    assert_eq!(
        people_of(&s.json(&["list", "--json"])["tasks"][0]),
        ["Meri S", "McDonald"],
        "a snapshot carries the spellings, not just the links"
    );
    // And the fresh log — one `compacted` line — still replays them.
    let events = s.json(&["log", "--json"])["events"].clone();
    assert_eq!(events.as_array().unwrap().len(), 1);
    assert_eq!(events[0]["ev"], "compacted");
}

#[test]
fn a_batch_can_link_people_and_undo_the_lot() {
    let s = Sandbox::new();
    let a = s.add(&["brake pads"]);
    let b = s.add(&["wing mirror"]);
    s.run(&["who", &b, "gegham"]);

    let ops = format!(
        "{{\"op\":\"who\",\"id\":\"{a}\",\"people\":[\"meri\",\"gegham\"]}}\n\
         {{\"op\":\"who\",\"id\":\"{b}\",\"people\":[\"gegham\"],\"remove\":true}}\n"
    );
    let out = s.pd().arg("batch").write_stdin(ops).output().expect("run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let tasks = s.json(&["list", "--json"])["tasks"].clone();
    assert_eq!(people_of(&tasks[0]), ["Meri", "Gegham"]);
    assert!(people_of(&tasks[1]).is_empty());

    assert_eq!(s.json(&["undo", "--json"])["events_reverted"], 3);
    let tasks = s.json(&["list", "--json"])["tasks"].clone();
    assert!(people_of(&tasks[0]).is_empty());
    assert_eq!(people_of(&tasks[1]), ["Gegham"]);
}

#[test]
fn people_show_up_in_the_rendered_list_and_in_the_log() {
    let s = Sandbox::new();
    let id = s.add(&["brake pads"]);
    s.run(&["who", &id, "meri"]);

    // Piped output is tab-separated, with the people in their own trailing column.
    let listed = String::from_utf8_lossy(&s.run(&["list"]).stdout).to_string();
    assert!(
        listed.trim_end().ends_with("\tMeri"),
        "the people column is the last one: {listed:?}"
    );

    let logged = String::from_utf8_lossy(&s.run(&["log"]).stdout).to_string();
    assert!(logged.contains("linked"), "{logged}");
    assert!(logged.contains("@Meri"), "{logged}");

    s.run(&["people", "rename", "meri", "Mary"]);
    let logged = String::from_utf8_lossy(&s.run(&["log"]).stdout).to_string();
    assert!(
        logged.contains("@Meri → @Mary"),
        "a rename names both sides: {logged}"
    );
}

#[test]
fn a_path_addressed_link_from_an_agent_is_refused_without_expect_seq() {
    let s = Sandbox::new();
    s.add(&["brake pads"]);
    let out = s.run(&["who", "1", "meri"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "stale-path guard covers `who` too"
    );
}
