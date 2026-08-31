//! Derived state. The log is the truth; everything here is replayed from it.

use std::collections::{BTreeMap, HashMap, HashSet};

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
    /// The people involved, normalised, in the order they were linked. Defaulted rather
    /// than required, because it also has to load out of a `compacted` snapshot written
    /// before people existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub people: Vec<String>,
    /// Insertion order. The default view never deviates from it.
    pub order: u64,
}

#[derive(Debug, Default)]
pub struct State {
    /// Insertion order, always.
    pub tasks: Vec<Task>,
    pub last_seq: u64,
    /// Folded key to the spelling it should be shown in. Tasks store keys, so this is
    /// the only place a person's capitalisation lives — first link wins, and
    /// `pd people rename` is how it is changed.
    displays: BTreeMap<String, String>,
    /// The `order` the next created task will take. Carried on the state rather than
    /// recomputed, so applying one event stays O(tasks) instead of O(tasks) per scan.
    next_order: u64,
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
        let mut state = State::default();
        for e in events {
            state.apply(e);
        }
        state
    }

    /// Fold one event into the state.
    ///
    /// The whole of replay is this function in a loop, which is what lets a command
    /// building several events at once — `pd batch` — validate each one against the state
    /// its predecessors leave behind, without replaying the log from the top every time.
    pub fn apply(&mut self, e: &Event) {
        self.last_seq = self.last_seq.max(e.seq);

        if e.ev == Kind::Compacted {
            // A snapshot replaces everything before it.
            if let Some(snap) = &e.data.snapshot {
                self.tasks = snap.clone();
                self.next_order = self.tasks.iter().map(|t| t.order + 1).max().unwrap_or(0);
                // A snapshot written before display forms existed carries none, which
                // leaves every name to the capitalisation default rather than losing it.
                self.displays = e.data.people.clone().unwrap_or_default();
            }
            return;
        }

        if e.ev == Kind::Renamed {
            // The rename is the event; the links are not rewritten in the log, they are
            // resolved here on the way past. Merging onto a name that already exists is
            // allowed, so the dedupe is not optional.
            //
            // `from` and `to` are display forms and are folded here, which is what lets a
            // rename that changes nothing but the capitalisation say something: the keys
            // match, the spelling does not, and `undo` restores the old one by the same
            // swap it uses for an `edited`. A log from before display forms existed put
            // folded names in both, and folding a folded name changes nothing.
            let (Some(from_raw), Some(to_raw)) = (
                e.data.from.as_ref().and_then(|v| v.as_str()),
                e.data.to.as_ref().and_then(|v| v.as_str()),
            ) else {
                return;
            };
            let from = fold(from_raw);
            let to = fold(to_raw);
            self.displays.remove(&from);
            self.displays.insert(to.clone(), to_raw.to_string());
            if from == to {
                return;
            }
            let (from, to) = (from.as_str(), to.as_str());
            for t in &mut self.tasks {
                if !t.people.iter().any(|p| p == from) {
                    continue;
                }
                for p in t.people.iter_mut() {
                    if p == from {
                        *p = to.to_string();
                    }
                }
                let mut seen = HashSet::new();
                t.people.retain(|p| seen.insert(p.clone()));
            }
            return;
        }

        if e.ev == Kind::Created {
            // A parent that would close a loop is dropped, not obeyed. Keeping the
            // task at the root costs a level of nesting; honouring the cycle costs
            // every later tree walk.
            let parent = match &e.data.parent {
                Some(p) if closes_cycle(&self.tasks, &e.id, p) => None,
                other => other.clone(),
            };
            self.tasks.push(Task {
                id: e.id.clone(),
                text: e.data.text.clone().unwrap_or_default(),
                state: TaskState::Open,
                priority: e.data.priority,
                due: e.data.due.clone(),
                parent,
                created_at: e.ts.clone(),
                completed_at: None,
                people: Vec::new(),
                order: self.next_order,
            });
            self.next_order += 1;
            return;
        }

        // Resolved before the mutable borrow below, since it has to read the rest of
        // the tree. `None` here means "leave the parent alone".
        let move_to = if e.ev == Kind::Moved {
            let to = e.data.to_parent.as_ref().and_then(|v| v.as_str());
            match to {
                Some(p) if closes_cycle(&self.tasks, &e.id, p) => None,
                Some(p) => Some(Some(p.to_string())),
                None => Some(None),
            }
        } else {
            None
        };

        // Collected inside the task borrow below and applied after it, since the display
        // map and the task list are both fields of `self`.
        let mut displays_entry: Vec<(String, Option<String>)> = Vec::new();

        let Some(t) = self.tasks.iter_mut().find(|t| t.id == e.id) else {
            // An event for a task we never saw created. Skip rather than invent one.
            return;
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
            Kind::Linked => {
                if let Some(p) = &e.data.person {
                    if !t.people.contains(p) {
                        t.people.push(p.clone());
                    }
                    // First link wins: someone who wrote `McDonald` once should not have
                    // it undone by the next person who types it in a hurry. Changing it
                    // is what `pd people rename` is for.
                    displays_entry.push((p.clone(), e.data.person_display.clone()));
                }
            }
            Kind::Unlinked => {
                if let Some(p) = &e.data.person {
                    t.people.retain(|x| x != p);
                }
            }
            Kind::Noted => {}
            Kind::Created | Kind::Compacted | Kind::Renamed => unreachable!("handled above"),
        }

        // Only spellings somebody actually chose go in. An event without one leaves no
        // entry at all rather than an entry that merely repeats the key — that is what
        // lets `stored_display` answer "nobody has pinned one", and so what makes
        // `pd people rename meri Meri` a real change on a log written before display
        // forms existed.
        for (key, display) in displays_entry {
            if let Some(display) = display {
                self.displays.entry(key).or_insert(display);
            }
        }
    }

    /// The spelling a person is shown in.
    ///
    /// A stored form with any capital in it is the caller's own and is returned verbatim.
    /// One that is entirely lower-case — all anybody typed was `meri`, or the log predates
    /// display forms entirely — is capitalised word by word, because a name rendered in
    /// lower case reads as a tag rather than as a person.
    pub fn display_of(&self, key: &str) -> String {
        shown(self.displays.get(key).map(String::as_str).unwrap_or(key))
    }

    /// The spelling as it was actually stored, before the capitalisation default. `None`
    /// when nobody has pinned one — which is what makes `pd people rename meri Meri` a
    /// real change rather than a no-op.
    pub fn stored_display(&self, key: &str) -> Option<&str> {
        self.displays.get(key).map(String::as_str)
    }

    pub fn displays_of<'a>(&self, keys: impl IntoIterator<Item = &'a String>) -> Vec<String> {
        keys.into_iter().map(|k| self.display_of(k)).collect()
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

    /// The people this file knows about, derived from the links on its tasks.
    ///
    /// There is no roster to maintain: a person exists exactly as long as some task
    /// still names them. Dropped tasks count towards neither total — "decided against
    /// this" is not work outstanding and not work done.
    ///
    /// Ordered as the list wants to be read: busiest first, ties alphabetical.
    pub fn people(&self) -> Vec<Person> {
        let mut by_name: HashMap<&str, (usize, usize)> = HashMap::new();
        for t in &self.tasks {
            for p in &t.people {
                let e = by_name.entry(p.as_str()).or_insert((0, 0));
                match t.state {
                    TaskState::Open => e.0 += 1,
                    TaskState::Done => e.1 += 1,
                    TaskState::Dropped => {}
                }
            }
        }
        let mut rows: Vec<(&str, usize, usize)> = by_name
            .into_iter()
            .map(|(name, (open, done))| (name, open, done))
            .collect();
        // Sorted on the key, not the spelling, so a capital letter never moves anybody.
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        rows.into_iter()
            .map(|(key, open, done)| Person {
                name: self.display_of(key),
                open,
                done,
            })
            .collect()
    }

    /// Every task currently linked to `person`, in insertion order.
    pub fn tasks_of(&self, person: &str) -> Vec<&Task> {
        let mut v: Vec<&Task> = self
            .tasks
            .iter()
            .filter(|t| t.people.iter().any(|p| p == person))
            .collect();
        v.sort_by_key(|t| t.order);
        v
    }

    pub fn knows_person(&self, person: &str) -> bool {
        self.tasks
            .iter()
            .any(|t| t.people.iter().any(|p| p == person))
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

/// How a stored spelling reaches the screen.
///
/// One with any capital in it is the caller's own and is returned verbatim — `McDonald`,
/// `de Vries` and `O'Brien` are not spelling mistakes, and no rule that rewrites them
/// would be right often enough to be worth being wrong about. One that is entirely lower
/// case is all anybody ever typed, or came off a log written before display forms
/// existed, so each word gets its capital: a name in lower case reads as a tag rather
/// than as a person.
pub fn shown(raw: &str) -> String {
    if raw.chars().any(char::is_uppercase) {
        return raw.to_string();
    }
    raw.split(' ')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The one spelling everything is compared against. Kept beside the state because replay
/// has to fold a rename's two names without going through the CLI's parser.
fn fold(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// One row of the derived people list.
#[derive(Debug, Clone, Serialize)]
pub struct Person {
    pub name: String,
    pub open: usize,
    pub done: usize,
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
    // Only the people some task still names. A display form nobody is linked to is not
    // state, and carrying it forward would make the snapshot grow with every rename.
    let live: HashSet<&str> = state
        .tasks
        .iter()
        .flat_map(|t| t.people.iter().map(String::as_str))
        .collect();
    let people: BTreeMap<String, String> = state
        .displays
        .iter()
        .filter(|(k, _)| live.contains(k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Data {
        snapshot: Some(state.tasks.clone()),
        people: (!people.is_empty()).then_some(people),
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
            people: vec!["meri".into()],
            order: 0,
        };
        log.push(ev(2, Kind::Compacted, "").with_data(Data {
            snapshot: Some(vec![snap]),
            people: Some([("meri".to_string(), "Meri S".to_string())].into()),
            ..Default::default()
        }));
        let s = State::replay(&log);
        assert_eq!(s.tasks.len(), 1);
        assert_eq!(s.tasks[0].id, "zzz");
        assert_eq!(
            s.display_of("meri"),
            "Meri S",
            "the snapshot carries spellings"
        );
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

    fn linked(seq: u64, id: &str, person: &str, role: Option<&str>) -> Event {
        ev(seq, Kind::Linked, id).with_data(Data {
            person: Some(person.to_lowercase()),
            person_display: Some(person.into()),
            role: role.map(String::from),
            ..Default::default()
        })
    }

    fn renamed(seq: u64, from: &str, to: &str) -> Event {
        ev(seq, Kind::Renamed, "").with_data(Data {
            from: Some(json!(from)),
            to: Some(json!(to)),
            ..Default::default()
        })
    }

    #[test]
    fn links_are_a_set_and_survive_an_unlink() {
        let log = vec![
            created(1, "aaa", "brake pads", None),
            linked(2, "aaa", "meri", None),
            linked(3, "aaa", "gegham", None),
            linked(4, "aaa", "meri", None),
            ev(5, Kind::Unlinked, "aaa").with_data(Data {
                person: Some("gegham".into()),
                ..Default::default()
            }),
        ];
        let s = State::replay(&log);
        assert_eq!(s.get("aaa").unwrap().people, ["meri"]);
    }

    /// A rename is not applied by rewriting the `linked` events; it is a line of its own
    /// that replay resolves on the way past. Renaming onto an existing name merges.
    #[test]
    fn a_rename_resolves_every_link_that_named_the_old_name() {
        let log = vec![
            created(1, "aaa", "brake pads", None),
            created(2, "bbb", "wing mirror", None),
            linked(3, "aaa", "meri", None),
            linked(4, "bbb", "meri", None),
            linked(5, "bbb", "gegham", None),
            renamed(6, "meri", "Gegham"),
        ];
        let s = State::replay(&log);
        assert_eq!(s.get("aaa").unwrap().people, ["gegham"]);
        assert_eq!(
            s.get("bbb").unwrap().people,
            ["gegham"],
            "the merge leaves one of them, not two"
        );
        assert_eq!(s.people().len(), 1);
        assert_eq!(s.people()[0].open, 2);
        assert_eq!(
            s.people()[0].name,
            "Gegham",
            "the spelling typed for the target is the one that stands"
        );
    }

    /// The first spelling wins. A display form is not derived at render time because no
    /// title-casing rule survives contact with a real surname.
    #[test]
    fn the_first_spelling_of_a_name_is_the_one_kept() {
        let log = vec![
            created(1, "aaa", "one", None),
            created(2, "bbb", "two", None),
            linked(3, "aaa", "McDonald", None),
            linked(4, "bbb", "mcdonald", None),
        ];
        let s = State::replay(&log);
        assert_eq!(s.display_of("mcdonald"), "McDonald");
        assert_eq!(s.people().len(), 1, "one person, two spellings");
        assert_eq!(
            s.get("bbb").unwrap().people,
            ["mcdonald"],
            "keys are folded"
        );
    }

    /// A name nobody capitalised gets its capitals at the edge; one somebody did is left
    /// exactly as they wrote it.
    #[test]
    fn lower_case_names_are_capitalised_and_typed_ones_are_not() {
        assert_eq!(shown("meri"), "Meri");
        assert_eq!(shown("meri sargsyan"), "Meri Sargsyan");
        assert_eq!(shown("McDonald"), "McDonald");
        assert_eq!(shown("de Vries"), "de Vries");
        assert_eq!(shown("O'Brien"), "O'Brien");
    }

    /// A log written before display forms existed carries none of them, and still has to
    /// come back reading like a name.
    #[test]
    fn a_log_without_display_forms_falls_back_to_the_key() {
        let log = vec![
            created(1, "aaa", "one", None),
            ev(2, Kind::Linked, "aaa").with_data(Data {
                person: Some("meri sargsyan".into()),
                ..Default::default()
            }),
        ];
        let s = State::replay(&log);
        assert_eq!(s.stored_display("meri sargsyan"), None);
        assert_eq!(s.display_of("meri sargsyan"), "Meri Sargsyan");
        assert_eq!(s.people()[0].name, "Meri Sargsyan");
    }

    /// The keys do not move, so nothing about the links changes — but the spelling does,
    /// which is the whole point of the command.
    #[test]
    fn a_rename_that_only_changes_the_case_still_changes_the_spelling() {
        let log = vec![
            created(1, "aaa", "one", None),
            linked(2, "aaa", "mcdonald", None),
            renamed(3, "mcdonald", "McDonald"),
        ];
        let s = State::replay(&log);
        assert_eq!(s.get("aaa").unwrap().people, ["mcdonald"]);
        assert_eq!(s.display_of("mcdonald"), "McDonald");
        assert_eq!(s.stored_display("mcdonald"), Some("McDonald"));
    }

    /// Roles are not written yet, but a log from a later `pd` that sets one still has to
    /// replay into the same links rather than being skipped.
    #[test]
    fn a_link_carrying_a_role_still_links() {
        let log = vec![
            created(1, "aaa", "brake pads", None),
            linked(2, "aaa", "meri", Some("owner")),
        ];
        let s = State::replay(&log);
        assert_eq!(s.get("aaa").unwrap().people, ["meri"]);
    }

    #[test]
    fn the_people_list_counts_open_and_done_and_ignores_dropped() {
        let log = vec![
            created(1, "aaa", "one", None),
            created(2, "bbb", "two", None),
            created(3, "ccc", "three", None),
            linked(4, "aaa", "meri", None),
            linked(5, "bbb", "meri", None),
            linked(6, "ccc", "meri", None),
            ev(7, Kind::Completed, "bbb"),
            ev(8, Kind::Dropped, "ccc"),
        ];
        let people = State::replay(&log).people();
        assert_eq!(people.len(), 1);
        assert_eq!((people[0].open, people[0].done), (1, 1));
    }

    /// Compaction has to carry the spellings, and only the ones still in use.
    #[test]
    fn a_snapshot_carries_the_live_spellings_and_drops_the_rest() {
        let log = vec![
            created(1, "aaa", "one", None),
            linked(2, "aaa", "McDonald", None),
            linked(3, "aaa", "Anna", None),
            ev(4, Kind::Unlinked, "aaa").with_data(Data {
                person: Some("anna".into()),
                ..Default::default()
            }),
        ];
        let before = State::replay(&log);
        let snap = snapshot(&before);
        let people = snap.people.clone().expect("spellings are carried");
        assert_eq!(people.get("mcdonald").map(String::as_str), Some("McDonald"));
        assert!(
            !people.contains_key("anna"),
            "a spelling nobody is linked to is not state"
        );

        let after = State::replay(&[ev(5, Kind::Compacted, "").with_data(snap)]);
        assert_eq!(after.display_of("mcdonald"), "McDonald");
        assert_eq!(after.get("aaa").unwrap().people, ["mcdonald"]);
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
