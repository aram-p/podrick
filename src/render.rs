//! Output. Near-monochrome, one accent, no boxes — and all of it vanishes when stdout
//! is not a terminal.

use chrono::{DateTime, Local};

use crate::config::Sort;
use crate::dates;
use crate::state::{State, Task, TaskState};

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const ACCENT: &str = "\x1b[38;5;146m";
const OVERDUE: &str = "\x1b[38;5;203m";

const P1: &str = "\x1b[38;5;203m";
const P2: &str = "\x1b[38;5;215m";
const P3: &str = "\x1b[38;5;110m";

const TEXT_COL_MAX: usize = 58;

#[derive(Clone, Copy)]
pub struct Style {
    pub color: bool,
}

impl Style {
    pub fn plain() -> Style {
        Style { color: false }
    }
    fn wrap(&self, code: &str, s: &str) -> String {
        if self.color && !s.is_empty() {
            format!("{code}{s}{RESET}")
        } else {
            s.to_string()
        }
    }
    pub fn dim(&self, s: &str) -> String {
        self.wrap(DIM, s)
    }
    pub fn accent(&self, s: &str) -> String {
        self.wrap(ACCENT, s)
    }
    pub fn overdue(&self, s: &str) -> String {
        self.wrap(OVERDUE, s)
    }
    fn priority_bar(&self, p: Option<u8>) -> String {
        let code = match p {
            Some(1) => P1,
            Some(2) => P2,
            Some(3) => P3,
            _ => return "  ".into(),
        };
        if self.color {
            format!("{code}▏{RESET} ")
        } else {
            match p {
                Some(n) => format!("{n} "),
                None => "  ".into(),
            }
        }
    }
}

pub struct Row {
    pub id: String,
    pub path: Option<String>,
    pub depth: usize,
    pub text: String,
    pub state: TaskState,
    pub priority: Option<u8>,
    pub due: Option<String>,
    /// Blank line before this row, to separate subtrees.
    pub gap: bool,
}

fn symbol(state: TaskState) -> &'static str {
    match state {
        TaskState::Open => "○",
        TaskState::Done => "●",
        TaskState::Dropped => "⊘",
    }
}

/// Render a set of rows. Plain mode emits `path<TAB>id<TAB>state<TAB>text<TAB>due`,
/// which is what a pipe or a `NO_COLOR` terminal gets.
pub fn rows(rows: &[Row], st: Style, now: DateTime<Local>) -> String {
    if !st.color {
        return rows
            .iter()
            .map(|r| {
                format!(
                    "{}\t{}\t{}\t{}{}\t{}",
                    r.path.as_deref().unwrap_or("-"),
                    r.id,
                    match r.state {
                        TaskState::Open => "open",
                        TaskState::Done => "done",
                        TaskState::Dropped => "dropped",
                    },
                    "  ".repeat(r.depth.saturating_sub(1)),
                    r.text,
                    r.due.as_deref().unwrap_or(""),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
    }

    let width = rows
        .iter()
        .map(|r| 2 * r.depth.saturating_sub(1) + r.text.chars().count())
        .max()
        .unwrap_or(0)
        .min(TEXT_COL_MAX);

    let mut out = String::new();
    for r in rows {
        if r.gap {
            out.push('\n');
        }
        // The bullet carries the indentation, not the text — that is what makes a tree
        // read as a tree.
        let indent = "  ".repeat(r.depth.saturating_sub(1));
        let visible = indent.chars().count() + r.text.chars().count();
        let pad = width.saturating_sub(visible);

        let sym = match r.state {
            TaskState::Open => format!("{indent}{}", symbol(r.state)),
            _ => st.dim(&format!("{indent}{}", symbol(r.state))),
        };
        let shown = match r.state {
            TaskState::Open => r.text.clone(),
            _ => st.dim(&r.text),
        };

        let due = match &r.due {
            Some(d) => {
                let h = dates::humanize(d, now);
                if dates::is_overdue(d, now) && r.state == TaskState::Open {
                    st.overdue(&h)
                } else {
                    st.dim(&h)
                }
            }
            None => String::new(),
        };
        let due_pad = 10usize.saturating_sub(due_visible_len(&r.due, now));

        out.push_str(&format!(
            "  {}{} {}{} {}{} {}\n",
            st.priority_bar(r.priority),
            sym,
            shown,
            " ".repeat(pad + 2),
            due,
            " ".repeat(due_pad),
            st.dim(&r.id),
        ));
    }
    out.trim_end().to_string()
}

fn due_visible_len(due: &Option<String>, now: DateTime<Local>) -> usize {
    match due {
        Some(d) => dates::humanize(d, now).chars().count(),
        None => 0,
    }
}

/// Build rows for the default tree view.
///
/// `sort` orders siblings **during** the walk, while the tree is still a tree. An earlier
/// version flattened first and then tried to rediscover the structure by reading depth
/// adjacency out of the flat list, which mistook "a depth-2 row after a depth-1 row" for
/// "a child of it" and glued unrelated subtrees together.
pub fn tree_rows(
    state: &State,
    include_all: bool,
    filter: Option<&str>,
    sort: Option<Sort>,
) -> Vec<Row> {
    let paths = state.paths();
    let mut rows = Vec::new();
    let needle = filter.map(|f| f.to_lowercase());

    fn matches(text: &str, needle: &Option<String>) -> bool {
        match needle {
            None => true,
            Some(n) => text.to_lowercase().contains(n.as_str()),
        }
    }

    // A blank line separates subtrees, but only where one actually exists. Flat lists
    // stay tight — the default view of ten items has to fit on a glance.
    let mut prev_had_children = false;
    for root in ordered(state.open_roots(), sort) {
        let mut subtree = collect(state, root, 1, &paths, &needle, sort);
        if subtree.is_empty() {
            continue;
        }
        let has_children = subtree.len() > 1;
        if let Some(first) = subtree.first_mut() {
            first.gap = !rows.is_empty() && (has_children || prev_had_children);
        }
        prev_had_children = has_children;
        rows.extend(subtree);
    }

    if include_all {
        let mut closed: Vec<&crate::state::Task> = state
            .tasks
            .iter()
            .filter(|t| !t.state.is_open() && matches(&t.text, &needle))
            .collect();
        closed.sort_by_key(|t| t.order);
        for (i, t) in closed.iter().enumerate() {
            rows.push(Row {
                id: t.id.clone(),
                path: None,
                depth: 1,
                text: t.text.clone(),
                state: t.state,
                priority: t.priority,
                due: t.due.clone(),
                gap: i == 0 && !rows.is_empty(),
            });
        }
    }

    rows
}

fn collect(
    state: &State,
    task: &crate::state::Task,
    depth: usize,
    paths: &crate::state::Paths,
    needle: &Option<String>,
    sort: Option<Sort>,
) -> Vec<Row> {
    let children: Vec<Row> = ordered(state.open_children(&task.id), sort)
        .into_iter()
        .flat_map(|c| collect(state, c, depth + 1, paths, needle, sort))
        .collect();

    let self_matches = match needle {
        None => true,
        Some(n) => task.text.to_lowercase().contains(n.as_str()),
    };

    // A parent is kept when it matches, or when any descendant does — otherwise a
    // search would orphan its results.
    if !self_matches && children.is_empty() {
        return vec![];
    }

    let mut out = vec![Row {
        id: task.id.clone(),
        path: paths.path_of(&task.id).map(String::from),
        depth,
        text: task.text.clone(),
        state: task.state,
        priority: task.priority,
        due: task.due.clone(),
        gap: false,
    }];
    out.extend(children);
    out
}

/// Order one set of siblings. Subtrees move with their parent because they are still
/// attached to it, not because an algorithm inferred the attachment afterwards.
fn ordered(mut tasks: Vec<&Task>, sort: Option<Sort>) -> Vec<&Task> {
    let Some(key) = sort else {
        return tasks; // insertion order, which open_roots/open_children already give
    };
    // Stable, so equal keys keep insertion order rather than an arbitrary one.
    tasks.sort_by(|a, b| cmp_tasks(a, b, key));
    tasks
}

/// The fields any ordering looks at. Tasks and rows both reduce to this, so the sibling
/// sort and the flat sort cannot drift apart.
struct SortKey<'a> {
    priority: Option<u8>,
    due: Option<&'a str>,
    text: &'a str,
    order: u64,
}

impl<'a> From<&'a Task> for SortKey<'a> {
    fn from(t: &'a Task) -> SortKey<'a> {
        SortKey {
            priority: t.priority,
            due: t.due.as_deref(),
            text: &t.text,
            order: t.order,
        }
    }
}

impl<'a> From<&'a Row> for SortKey<'a> {
    fn from(r: &'a Row) -> SortKey<'a> {
        // Rows are already in insertion order when they reach a flat sort, so `order` is
        // constant and `Sort::Created` degenerates to "leave it alone".
        SortKey {
            priority: r.priority,
            due: r.due.as_deref(),
            text: &r.text,
            order: 0,
        }
    }
}

fn cmp_keys(a: &SortKey, b: &SortKey, key: Sort) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match key {
        // Unset priority sorts as p4: it is the documented default, not a separate rank.
        Sort::Priority => a.priority.unwrap_or(4).cmp(&b.priority.unwrap_or(4)),
        // Dated before undated: a list sorted by due date is a list of what is coming up.
        Sort::Due => match (a.due, b.due) {
            (Some(x), Some(y)) => x.cmp(y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        },
        Sort::Alpha => a.text.to_lowercase().cmp(&b.text.to_lowercase()),
        Sort::Created => a.order.cmp(&b.order),
    }
}

fn cmp_tasks(a: &Task, b: &Task, key: Sort) -> std::cmp::Ordering {
    cmp_keys(&a.into(), &b.into(), key)
}

/// Order an already-flattened list. Used only by the priority filter, which selects tasks
/// out of the tree and so has no sibling structure left to preserve.
pub fn sort_flat(rows: &mut [Row], key: Sort) {
    rows.sort_by(|a, b| cmp_keys(&a.into(), &b.into(), key));
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 8, 12, 14, 30, 0)
            .single()
            .unwrap()
    }

    fn row(id: &str, text: &str, depth: usize, pri: Option<u8>) -> Row {
        Row {
            id: id.into(),
            path: Some("1".into()),
            depth,
            text: text.into(),
            state: TaskState::Open,
            priority: pri,
            due: None,
            gap: false,
        }
    }

    #[test]
    fn plain_mode_emits_no_escape_codes() {
        let out = rows(
            &[row("a7f", "write the spec", 1, Some(1))],
            Style::plain(),
            now(),
        );
        assert!(
            !out.contains('\x1b'),
            "plain output must be free of ANSI: {out:?}"
        );
        assert!(out.contains("a7f"));
        assert!(out.contains("write the spec"));
    }

    #[test]
    fn colour_mode_emits_escape_codes() {
        let out = rows(&[row("a7f", "x", 1, Some(1))], Style { color: true }, now());
        assert!(out.contains('\x1b'));
    }

    /// Sorting now happens during the tree walk, so these go through `tree_rows` rather
    /// than a post-hoc reordering of a flat list. The assertions are the originals.
    fn state_of(specs: &[(&str, &str, Option<&str>, Option<u8>)]) -> State {
        use crate::event::{Actor, Data, Event, Kind};
        let log: Vec<Event> = specs
            .iter()
            .enumerate()
            .map(|(i, (id, text, parent, pri))| {
                Event::new(
                    i as u64 + 1,
                    format!("2026-08-12T10:{:02}:00+04:00", i),
                    Actor::Cli,
                    Kind::Created,
                    *id,
                )
                .with_data(Data {
                    text: Some((*text).into()),
                    parent: parent.map(String::from),
                    priority: *pri,
                    ..Default::default()
                })
            })
            .collect();
        State::replay(&log)
    }

    #[test]
    fn sorting_keeps_subtrees_with_their_parent() {
        let st = state_of(&[
            ("aaa", "b-parent", None, Some(3)),
            ("bbb", "child of b", Some("aaa"), None),
            ("ccc", "a-parent", None, Some(1)),
        ]);
        let rs = tree_rows(&st, false, None, Some(Sort::Priority));
        assert_eq!(rs[0].id, "ccc", "p1 root sorts first");
        assert_eq!(rs[1].id, "aaa");
        assert_eq!(rs[2].id, "bbb", "the child stayed with its parent");
        assert_eq!(rs[2].depth, 2, "and stayed a child");
    }

    #[test]
    fn alpha_sort_orders_siblings() {
        let st = state_of(&[("aaa", "zebra", None, None), ("bbb", "apple", None, None)]);
        let rs = tree_rows(&st, false, None, Some(Sort::Alpha));
        assert_eq!(rs[0].text, "apple");
    }

    /// The defect this replaced: a depth-2 row landing after an unrelated depth-1 row was
    /// read as its child, so sorting glued foreign subtrees together.
    #[test]
    fn a_child_is_never_reattached_to_the_wrong_parent_by_sorting() {
        let st = state_of(&[
            ("aaa", "zebra", None, Some(1)),
            ("bbb", "dee parent", None, Some(2)),
            ("ccc", "ekko child", Some("bbb"), Some(1)),
            ("ddd", "apple", None, Some(1)),
        ]);
        let rs = tree_rows(&st, false, None, Some(Sort::Alpha));

        let order: Vec<&str> = rs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(order, ["apple", "dee parent", "ekko child", "zebra"]);
        let child = rs.iter().find(|r| r.id == "ccc").unwrap();
        assert_eq!(child.depth, 2);
        // The row before it is its real parent, not whatever sorted next to it.
        let pos = rs.iter().position(|r| r.id == "ccc").unwrap();
        assert_eq!(rs[pos - 1].id, "bbb");
    }

    #[test]
    fn a_flat_sort_orders_what_the_priority_filter_selected() {
        let mut rs = vec![
            row("aaa", "zebra", 1, Some(1)),
            row("bbb", "apple", 1, Some(1)),
        ];
        sort_flat(&mut rs, Sort::Alpha);
        assert_eq!(rs[0].text, "apple");
    }
}
