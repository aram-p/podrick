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

    fn pd(&self) -> Command {
        let mut c = Command::cargo_bin("pd").expect("binary");
        c.current_dir(self.dir.path())
            .env("HOME", self.dir.path())
            .env("XDG_DATA_HOME", self.dir.path().join("data"))
            .env("XDG_CONFIG_HOME", self.dir.path().join("cfg"))
            .env("PODRICK_NOW", "2026-08-12T14:30:00+04:00")
            .env_remove("NO_COLOR");
        c
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

    let open = s.json(&["--json"]);
    assert_eq!(open["tasks"].as_array().unwrap().len(), 1, "done is hidden");

    for form in [
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
    s.run(&["config", "sort", "alpha", "--here"]);

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

/// A priority filter cuts across the tree. Every survivor must be one of the tasks that
/// matched — never a stray child re-indented under whatever happened to sort above it.
#[test]
fn a_priority_filter_never_reparents_what_it_keeps() {
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

    // Filtered, the p2 parent is gone and its child sorts on its own merits.
    assert_eq!(
        order(&s.json(&["list", "-p", "p1", "--sort", "alpha", "--json"])),
        ["apple", "ekko child", "zebra"],
        "the child used to be dragged to the end, glued to an unrelated root"
    );

    // The rendered tree must not imply a parent that was filtered away.
    let out = s.run(&["list", "-p", "p1", "--sort", "alpha", "--no-color"]);
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let shown = line.split('\t').nth(3).unwrap_or("");
        assert!(
            !shown.starts_with(' '),
            "no row may render as someone's subtask here: {line:?}"
        );
    }
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

    let out = Command::cargo_bin("pd")
        .unwrap()
        .current_dir(&sub)
        .env("HOME", s.path())
        .env("PODRICK_NOW", "2026-08-12T14:30:00+04:00")
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
        let mut c = std::process::Command::new(assert_cmd::cargo::cargo_bin("pd"));
        c.current_dir(s.path())
            .env("HOME", s.path())
            .env("PODRICK_NOW", "2026-08-12T14:30:00+04:00")
            .args(["add", &format!("task {i}")])
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

fn order(v: &Value) -> Vec<String> {
    v["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["text"].as_str().unwrap().to_string())
        .collect()
}
