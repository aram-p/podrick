//! Derived state. The log is the truth; everything here is replayed from it.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::event::{Data, Event, Kind};

pub const MAX_DEPTH: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    Open,
    Done,
    Dropped,
}

impl TaskState {
    pub fn is_open(self) -> bool {
        self == TaskState::Open
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub text: String,
    pub state: TaskState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    /// Insertion order. The default view never deviates from it.
    pub order: u64,
}

#[derive(Debug, Default)]
pub struct State {
    /// Insertion order, always.
    pub tasks: Vec<Task>,
    pub last_seq: u64,
}

/// Would giving `child` the parent `new_parent` close a loop?
///
/// Answered by walking up from `new_parent`; if `child` is already somewhere above it,
/// the link would point the tree back into itself. The visited set is what makes this
/// safe to call while replaying a log that may *already* contain a cycle.
fn closes_cycle(tasks: &[Task], child: &str, new_parent: &str) -> bool {
    if child == new_parent {
        return true;
    }
    let mut seen: HashSet<&str> = HashSet::new();
    let mut cur = Some(new_parent);
    while let Some(id) = cur {
        if id == child {
            return true;
        }
        if !seen.insert(id) {
            return true; // already looping; refuse to add to it
        }
        cur = tasks
            .iter()
            .find(|t| t.id == id)
            .and_then(|t| t.parent.as_deref());
    }
    false
}

impl State {
    pub fn replay(events: &[Event]) -> State {
        let mut tasks: Vec<Task> = Vec::new();
        let mut last_seq = 0u64;
        let mut next_order = 0u64;

        for e in events {
            last_seq = last_seq.max(e.seq);

            if e.ev == Kind::Compacted {
                // A snapshot replaces everything before it.
                if let Some(snap) = &e.data.snapshot {
                    tasks = snap.clone();
                    next_order = tasks.iter().map(|t| t.order + 1).max().unwrap_or(0);
                }
                continue;
            }

            if e.ev == Kind::Created {
                // A parent that would close a loop is dropped, not obeyed. Keeping the
                // task at the root costs a level of nesting; honouring the cycle costs
                // every later tree walk.
                let parent = match &e.data.parent {
                    Some(p) if closes_cycle(&tasks, &e.id, p) => None,
                    other => other.clone(),
                };
                tasks.push(Task {
                    id: e.id.clone(),
                    text: e.data.text.clone().unwrap_or_default(),
                    state: TaskState::Open,
                    priority: e.data.priority,
                    due: e.data.due.clone(),
                    parent,
                    created_at: e.ts.clone(),
                    completed_at: None,
                    order: next_order,
                });
                next_order += 1;
                continue;
            }

            // Resolved before the mutable borrow below, since it has to read the rest of
            // the tree. `None` here means "leave the parent alone".
            let move_to = if e.ev == Kind::Moved {
                let to = e.data.to_parent.as_ref().and_then(|v| v.as_str());
                match to {
                    Some(p) if closes_cycle(&tasks, &e.id, p) => None,
                    Some(p) => Some(Some(p.to_string())),
                    None => Some(None),
                }
            } else {
                None
            };

            let Some(t) = tasks.iter_mut().find(|t| t.id == e.id) else {
                // An event for a task we never saw created. Skip rather than invent one.
                continue;
            };

            match e.ev {
                Kind::Completed => {
                    t.state = TaskState::Done;
                    t.completed_at = Some(e.ts.clone());
                }
                Kind::Uncompleted => {
                    t.state = TaskState::Open;
                    t.completed_at = None;
                }
                Kind::Dropped => {
                    t.state = TaskState::Dropped;
                    t.completed_at = Some(e.ts.clone());
                }
                Kind::Edited => {
                    if let Some(to) = e.data.to.as_ref().and_then(|v| v.as_str()) {
                        t.text = to.to_string();
                    }
                }
                Kind::Reprioritized => {
                    t.priority = e.data.to.as_ref().and_then(|v| v.as_u64()).map(|n| n as u8);
                }
                Kind::Rescheduled => {
                    t.due = e
                        .data
                        .to
                        .as_ref()
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
                Kind::Moved => {
                    if let Some(p) = move_to {
                        t.parent = p;
                    }
                }
                Kind::Noted => {}
                Kind::Created | Kind::Compacted => unreachable!("handled above"),
            }
        }

        State { tasks, last_seq }
    }

    pub fn get(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn contains_id(&self, id: &str) -> bool {
        self.tasks.iter().any(|t| t.id == id)
    }

    /// Open children of `parent`, in insertion order.
    pub fn open_children(&self, parent: &str) -> Vec<&Task> {
        let mut v: Vec<&Task> = self
            .tasks
            .iter()
            .filter(|t| t.state.is_open() && t.parent.as_deref() == Some(parent))
            .collect();
        v.sort_by_key(|t| t.order);
        v
    }

    /// Open tasks with no *open* parent. A task whose parent was completed or dropped
    /// surfaces here rather than becoming invisible — an unreachable task is the worst
    /// failure this tool could have.
    pub fn open_roots(&self) -> Vec<&Task> {
        let mut v: Vec<&Task> = self
            .tasks
            .iter()
            .filter(|t| {
                t.state.is_open()
                    && match &t.parent {
                        None => true,
                        Some(p) => !self.get(p).is_some_and(|p| p.state.is_open()),
                    }
            })
            .collect();
        v.sort_by_key(|t| t.order);
        v
    }

    /// Every descendant of `id`, at any depth, in insertion order.
    ///
    /// Replay guarantees an acyclic tree, so the visited set is belt-and-braces rather
    /// than load-bearing — but it is what turns "we believe this terminates" into "this
    /// terminates", and the cost is one hash per task.
    pub fn descendants(&self, id: &str) -> Vec<&Task> {
        let mut out = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut frontier = vec![id];
        while let Some(cur) = frontier.pop() {
            if !seen.insert(cur) {
                continue;
            }
            let mut kids: Vec<&Task> = self
                .tasks
                .iter()
                .filter(|t| t.parent.as_deref() == Some(cur))
                .collect();
            kids.sort_by_key(|t| t.order);
            for k in kids {
                frontier.push(&k.id);
                out.push(k);
            }
        }
        out.sort_by_key(|t| t.order);
        out
    }

    /// Depth of a task in the tree, counting from 1.
    pub fn depth(&self, id: &str) -> usize {
        let mut depth = 1;
        let mut seen: HashSet<&str> = HashSet::new();
        let mut cur = self.get(id).and_then(|t| t.parent.as_deref());
        while let Some(p) = cur {
            if !seen.insert(p) {
                break;
            }
            depth += 1;
            cur = self.get(p).and_then(|t| t.parent.as_deref());
        }
        depth
    }

    /// Would parenting `child` under `parent` create a cycle?
    pub fn would_cycle(&self, child: &str, parent: &str) -> bool {
        if child == parent {
            return true;
        }
        self.descendants(child).iter().any(|t| t.id == parent)
    }

    /// Dotted paths, assigned over **open tasks only**, in insertion order.
    ///
    /// Paths therefore always address something you can act on, and done/dropped tasks
    /// are addressable only by id. This is what keeps `pd done 2.1` unambiguous.
    pub fn paths(&self) -> Paths {
        let mut by_id = HashMap::new();
        let mut by_path = HashMap::new();

        fn walk(
            s: &State,
            nodes: Vec<&Task>,
            prefix: &str,
            by_id: &mut HashMap<String, String>,
            by_path: &mut HashMap<String, String>,
        ) {
            for (i, t) in nodes.iter().enumerate() {
                let path = if prefix.is_empty() {
                    format!("{}", i + 1)
                } else {
                    format!("{}.{}", prefix, i + 1)
                };
                by_id.insert(t.id.clone(), path.clone());
                by_path.insert(path.clone(), t.id.clone());
                walk(s, s.open_children(&t.id), &path, by_id, by_path);
            }
        }

        walk(self, self.open_roots(), "", &mut by_id, &mut by_path);
        Paths { by_id, by_path }
    }
}

pub struct Paths {
    pub by_id: HashMap<String, String>,
    pub by_path: HashMap<String, String>,
}

impl Paths {
    pub fn path_of(&self, id: &str) -> Option<&str> {
        self.by_id.get(id).map(|s| s.as_str())
    }
    pub fn id_at(&self, path: &str) -> Option<&str> {
        self.by_path.get(path).map(|s| s.as_str())
    }
}

/// Build a `compacted` snapshot payload from current state.
pub fn snapshot(state: &State) -> Data {
    Data {
        snapshot: Some(state.tasks.clone()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Actor;
    use serde_json::json;

    fn ev(seq: u64, kind: Kind, id: &str) -> Event {
        Event::new(
            seq,
            format!("2026-08-12T10:{:02}:00+04:00", seq),
            Actor::Cli,
            kind,
            id,
        )
    }

    fn created(seq: u64, id: &str, text: &str, parent: Option<&str>) -> Event {
        ev(seq, Kind::Created, id).with_data(Data {
            text: Some(text.into()),
            parent: parent.map(String::from),
            ..Default::default()
        })
    }

    #[test]
    fn replays_a_simple_log() {
        let log = vec![
            created(1, "aaa", "first", None),
            created(2, "bbb", "second", None),
            ev(3, Kind::Completed, "aaa"),
        ];
        let s = State::replay(&log);
        assert_eq!(s.last_seq, 3);
        assert_eq!(s.get("aaa").unwrap().state, TaskState::Done);
        assert_eq!(s.get("bbb").unwrap().state, TaskState::Open);
    }

    #[test]
    fn done_then_undone_then_done_lands_done() {
        let log = vec![
            created(1, "aaa", "x", None),
            ev(2, Kind::Completed, "aaa"),
            ev(3, Kind::Uncompleted, "aaa"),
            ev(4, Kind::Completed, "aaa"),
        ];
        let s = State::replay(&log);
        assert_eq!(s.get("aaa").unwrap().state, TaskState::Done);
        assert!(s.get("aaa").unwrap().completed_at.is_some());
    }

    #[test]
    fn paths_cover_open_tasks_only() {
        let log = vec![
            created(1, "aaa", "first", None),
            created(2, "bbb", "second", None),
            created(3, "ccc", "sub of second", Some("bbb")),
            ev(4, Kind::Completed, "aaa"),
        ];
        let s = State::replay(&log);
        let p = s.paths();
        // "aaa" is done, so it holds no path and "bbb" becomes 1.
        assert_eq!(p.path_of("aaa"), None);
        assert_eq!(p.path_of("bbb"), Some("1"));
        assert_eq!(p.path_of("ccc"), Some("1.1"));
        assert_eq!(p.id_at("1.1"), Some("ccc"));
    }

    #[test]
    fn a_task_whose_parent_is_done_stays_visible() {
        let log = vec![
            created(1, "aaa", "parent", None),
            created(2, "bbb", "child", Some("aaa")),
            ev(3, Kind::Completed, "aaa"),
        ];
        let s = State::replay(&log);
        // The child is still open, so it must be addressable — as a root.
        assert_eq!(s.paths().path_of("bbb"), Some("1"));
    }

    #[test]
    fn snapshot_replaces_prior_history() {
        let mut log = vec![created(1, "aaa", "gone after compact", None)];
        let snap = Task {
            id: "zzz".into(),
            text: "only survivor".into(),
            state: TaskState::Open,
            priority: None,
            due: None,
            parent: None,
            created_at: "2026-08-12T10:00:00+04:00".into(),
            completed_at: None,
            order: 0,
        };
        log.push(ev(2, Kind::Compacted, "").with_data(Data {
            snapshot: Some(vec![snap]),
            ..Default::default()
        }));
        let s = State::replay(&log);
        assert_eq!(s.tasks.len(), 1);
        assert_eq!(s.tasks[0].id, "zzz");
    }

    #[test]
    fn edits_and_reprioritizes() {
        let log = vec![
            created(1, "aaa", "old text", None),
            ev(2, Kind::Edited, "aaa").with_data(Data {
                from: Some(json!("old text")),
                to: Some(json!("new text")),
                ..Default::default()
            }),
            ev(3, Kind::Reprioritized, "aaa").with_data(Data {
                from: Some(json!(null)),
                to: Some(json!(1)),
                ..Default::default()
            }),
        ];
        let s = State::replay(&log);
        assert_eq!(s.get("aaa").unwrap().text, "new text");
        assert_eq!(s.get("aaa").unwrap().priority, Some(1));
    }

    #[test]
    fn depth_and_cycles() {
        let log = vec![
            created(1, "aaa", "1", None),
            created(2, "bbb", "2", Some("aaa")),
            created(3, "ccc", "3", Some("bbb")),
        ];
        let s = State::replay(&log);
        assert_eq!(s.depth("aaa"), 1);
        assert_eq!(s.depth("ccc"), 3);
        assert!(s.would_cycle("aaa", "ccc"));
        assert!(!s.would_cycle("ccc", "aaa"));
    }

    /// A `moved` that points a task under its own descendant. Before the guard this
    /// replayed into a loop, and every later tree walk spun on it.
    #[test]
    fn a_move_that_would_close_a_loop_is_refused_at_replay() {
        let log = vec![
            created(1, "aaa", "A", None),
            created(2, "bbb", "B", Some("aaa")),
            ev(3, Kind::Moved, "aaa").with_data(Data {
                to_parent: Some(json!("bbb")),
                ..Default::default()
            }),
        ];
        let s = State::replay(&log);

        assert_eq!(s.get("aaa").unwrap().parent, None, "the move was dropped");
        assert_eq!(s.get("bbb").unwrap().parent.as_deref(), Some("aaa"));
        // The real symptom: these three used to never return.
        assert_eq!(s.descendants("aaa").len(), 1);
        assert_eq!(s.depth("bbb"), 2);
        assert_eq!(s.paths().path_of("bbb"), Some("1.1"));
        assert_eq!(s.open_roots().len(), 1, "both tasks stay reachable");
    }

    /// The variant the `moved` guard alone would miss: two `created` events naming each
    /// other, which needs no `moved` at all.
    #[test]
    fn two_creates_naming_each_other_do_not_form_a_loop() {
        let log = vec![
            created(1, "aaa", "A", Some("bbb")),
            created(2, "bbb", "B", Some("aaa")),
        ];
        let s = State::replay(&log);

        // "aaa" named a parent that did not exist yet, so it kept it; "bbb" would have
        // closed the loop, so its parent was dropped.
        assert_eq!(s.get("bbb").unwrap().parent, None);
        assert_eq!(s.descendants("bbb").len(), 1);
        assert_eq!(s.tasks.len(), 2, "neither task was lost");
        assert!(!s.paths().by_id.is_empty(), "both remain addressable");
    }

    /// A task pointing at a parent that never existed is not a cycle — it must keep
    /// surfacing at the root rather than being swallowed.
    #[test]
    fn a_dangling_parent_leaves_the_task_reachable() {
        let log = vec![created(1, "aaa", "A", Some("nope"))];
        let s = State::replay(&log);
        assert_eq!(s.open_roots().len(), 1);
        assert_eq!(s.depth("aaa"), 2, "depth counts the missing link");
    }
}
