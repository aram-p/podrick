//! Output. Near-monochrome, one accent, no boxes — and all of it vanishes when stdout
//! is not a terminal.

use chrono::{DateTime, Local};

use crate::dates;
use crate::state::{State, TaskState};

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
pub fn tree_rows(state: &State, include_all: bool, filter: Option<&str>) -> Vec<Row> {
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
    for root in state.open_roots() {
        let mut subtree = collect(state, root, 1, &paths, &needle);
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
) -> Vec<Row> {
    let children: Vec<Row> = state
        .open_children(&task.id)
        .into_iter()
        .flat_map(|c| collect(state, c, depth + 1, paths, needle))
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

/// Sort siblings within the tree. Never flattens: the tree structure always wins.
pub fn sort_rows(rows: &mut [Row], key: &str, state: &State, now: DateTime<Local>) {
    // Rows are a pre-order flattening, so sorting in place would break the tree. Sort
    // sibling groups by rebuilding runs of equal depth under the same parent.
    let _ = (state, now);
    sort_runs(rows, key, now);
}

fn sort_runs(rows: &mut [Row], key: &str, now: DateTime<Local>) {
    // Identify contiguous runs at the same depth whose members are siblings, and order
    // each run independently. Subtrees move with their root.
    let mut i = 0;
    while i < rows.len() {
        let depth = rows[i].depth;
        let mut groups: Vec<(usize, usize)> = Vec::new(); // (start, len) of each subtree
        let mut j = i;
        while j < rows.len() && rows[j].depth >= depth {
            if rows[j].depth == depth {
                groups.push((j, 1));
            } else if let Some(last) = groups.last_mut() {
                last.1 += 1;
            }
            j += 1;
        }
        if groups.len() > 1 {
            let mut blocks: Vec<Vec<Row>> = groups
                .iter()
                .map(|&(s, l)| rows[s..s + l].iter().map(clone_row).collect())
                .collect();
            blocks.sort_by(|a, b| cmp_rows(&a[0], &b[0], key, now));
            let mut out = Vec::new();
            for b in blocks {
                out.extend(b);
            }
            for (k, r) in out.into_iter().enumerate() {
                rows[i + k] = r;
            }
        }
        // Recurse into each subtree's children.
        for &(s, l) in &groups {
            if l > 1 {
                sort_runs(&mut rows[s + 1..s + l], key, now);
            }
        }
        i = j;
    }
}

fn clone_row(r: &Row) -> Row {
    Row {
        id: r.id.clone(),
        path: r.path.clone(),
        depth: r.depth,
        text: r.text.clone(),
        state: r.state,
        priority: r.priority,
        due: r.due.clone(),
        gap: r.gap,
    }
}

fn cmp_rows(a: &Row, b: &Row, key: &str, _now: DateTime<Local>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match key {
        "priority" => a.priority.unwrap_or(4).cmp(&b.priority.unwrap_or(4)),
        "due" => match (&a.due, &b.due) {
            (Some(x), Some(y)) => x.cmp(y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        },
        "alpha" => a.text.to_lowercase().cmp(&b.text.to_lowercase()),
        _ => Ordering::Equal, // "created" is insertion order, which is what we already have
    }
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

    #[test]
    fn sorting_keeps_subtrees_with_their_parent() {
        let mut rs = vec![
            row("aaa", "b-parent", 1, Some(3)),
            row("bbb", "child of b", 2, None),
            row("ccc", "a-parent", 1, Some(1)),
        ];
        sort_rows(&mut rs, "priority", &State::default(), now());
        assert_eq!(rs[0].id, "ccc", "p1 root sorts first");
        assert_eq!(rs[1].id, "aaa");
        assert_eq!(rs[2].id, "bbb", "the child stayed with its parent");
    }

    #[test]
    fn alpha_sort_orders_siblings() {
        let mut rs = vec![row("aaa", "zebra", 1, None), row("bbb", "apple", 1, None)];
        sort_rows(&mut rs, "alpha", &State::default(), now());
        assert_eq!(rs[0].text, "apple");
    }
}
