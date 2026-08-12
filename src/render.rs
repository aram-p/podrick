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

/// The widest the text column is ever allowed to get, however wide the terminal is.
/// Around seventy characters is the readable measure, and past it the ids drift so far
/// from their tasks that the right-hand column stops being scannable.
const TEXT_COL_MAX: usize = 72;

/// The narrowest it is allowed to get. Below this the terminal is too small for the
/// layout, and overflowing is a better answer than wrapping every title to three letters.
const TEXT_COL_MIN: usize = 16;

/// Assumed width when nobody will tell us — a pipe, or a terminal that does not answer.
const FALLBACK_COLUMNS: usize = 80;

/// How many columns there are to draw in.
///
/// `PODRICK_COLUMNS` wins, both so the tests are deterministic and so anyone whose
/// terminal lies about itself has a way out. Same escape hatch as `PODRICK_NOW`.
fn columns() -> usize {
    if let Some(n) = std::env::var("PODRICK_COLUMNS")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
    {
        return n;
    }
    terminal_size::terminal_size()
        .map(|(terminal_size::Width(w), _)| w as usize)
        .unwrap_or(FALLBACK_COLUMNS)
}

/// Break `text` to `width` columns, preferring word boundaries.
///
/// A word longer than the whole column is split rather than allowed to overhang — a
/// pasted URL should not push the id off the screen for every other row too.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut len = 0usize;

    for word in text.split_whitespace() {
        let wlen = word.chars().count();
        if len > 0 && len + 1 + wlen > width {
            lines.push(std::mem::take(&mut line));
            len = 0;
        }
        // Still too long on a line of its own: hard-split it across as many as it needs.
        if wlen > width {
            let mut rest: &str = word;
            while rest.chars().count() > width {
                let cut = rest
                    .char_indices()
                    .nth(width - len.min(width))
                    .map(|(i, _)| i)
                    .unwrap_or(rest.len());
                if len > 0 {
                    line.push(' ');
                }
                line.push_str(&rest[..cut]);
                lines.push(std::mem::take(&mut line));
                len = 0;
                rest = &rest[cut..];
            }
            line.push_str(rest);
            len = rest.chars().count();
            continue;
        }
        if len > 0 {
            line.push(' ');
            len += 1;
        }
        line.push_str(word);
        len += wlen;
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

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
    rows_at(rows, st, now, columns())
}

/// The layout itself, with the terminal width handed in rather than discovered — so the
/// tests can ask for a 40-column terminal without racing each other over an env var.
fn rows_at(rows: &[Row], st: Style, now: DateTime<Local>, cols: usize) -> String {
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

    // Only as wide as the list actually needs. A list with no dates in it should not pay
    // eleven columns for a date column, least of all on a narrow terminal.
    let due_w = rows
        .iter()
        .map(|r| due_visible_len(&r.due, now))
        .max()
        .unwrap_or(0);
    let id_w = rows.iter().map(|r| r.id.chars().count()).max().unwrap_or(0);

    // Everything on the line that is not the text: two-column gutter, priority bar,
    // bullet, the space after it, the two-space gap, then the date and id columns.
    let chrome = 2 + 2 + 1 + 1 + 2 + if due_w > 0 { due_w + 1 } else { 0 } + id_w;

    let natural = rows
        .iter()
        .map(|r| 2 * r.depth.saturating_sub(1) + r.text.chars().count())
        .max()
        .unwrap_or(0);
    let width = natural
        .min(TEXT_COL_MAX)
        .min(cols.saturating_sub(chrome))
        .max(TEXT_COL_MIN);

    let mut out = String::new();
    for r in rows {
        if r.gap {
            out.push('\n');
        }
        // The bullet carries the indentation, not the text — that is what makes a tree
        // read as a tree. The text column is what is left of `width` after that indent.
        let indent = "  ".repeat(r.depth.saturating_sub(1));
        let indent_w = indent.chars().count();
        let lines = wrap_text(&r.text, width.saturating_sub(indent_w).max(1));

        let sym = match r.state {
            TaskState::Open => format!("{indent}{}", symbol(r.state)),
            _ => st.dim(&format!("{indent}{}", symbol(r.state))),
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
        let due_col = if due_w > 0 {
            format!(
                "{due}{} ",
                " ".repeat(due_w.saturating_sub(due_visible_len(&r.due, now)))
            )
        } else {
            String::new()
        };

        // The date and the id belong to the task, so they sit on its first line; the rest
        // of a wrapped title hangs under the text, clear of the bullet column.
        for (i, line) in lines.iter().enumerate() {
            let shown = match r.state {
                TaskState::Open => line.clone(),
                _ => st.dim(line),
            };
            let pad = " ".repeat(width.saturating_sub(indent_w + line.chars().count()) + 2);
            if i == 0 {
                out.push_str(&format!(
                    "  {}{} {shown}{pad}{due_col}{}\n",
                    st.priority_bar(r.priority),
                    sym,
                    st.dim(&r.id),
                ));
            } else {
                out.push_str(&format!("      {}{shown}\n", " ".repeat(indent_w)));
            }
        }
    }
    out.trim_end().to_string()
}

fn due_visible_len(due: &Option<String>, now: DateTime<Local>) -> usize {
    match due {
        Some(d) => dates::humanize(d, now).chars().count(),
        None => 0,
    }
}

/// What a task has to match to be listed.
///
/// Both criteria behave the same way, which is the point: a task that fails is still shown
/// when a descendant passes. A filtered view stays a tree rather than orphaning its own
/// results under whichever unrelated row happens to precede them.
#[derive(Default, Clone)]
pub struct Filter {
    /// Case-insensitive substring of the task text.
    needle: Option<String>,
    /// Exact priority, where the outer `Option` is "was a priority asked for" and the
    /// inner one is the task's own unset-able priority.
    priority: Option<Option<u8>>,
}

impl Filter {
    pub fn text(needle: Option<&str>) -> Filter {
        Filter {
            needle: needle.map(str::to_lowercase),
            priority: None,
        }
    }

    pub fn with_priority(mut self, p: Option<Option<u8>>) -> Filter {
        self.priority = p;
        self
    }

    fn admits(&self, task: &Task) -> bool {
        let text_ok = match &self.needle {
            None => true,
            Some(n) => task.text.to_lowercase().contains(n.as_str()),
        };
        let pri_ok = match self.priority {
            None => true,
            Some(want) => task.priority == want,
        };
        text_ok && pri_ok
    }
}

/// Which closed tasks a listing carries under its open tree.
///
/// Done tasks are shown by default: a list you finished things on should say so, and a
/// task tracker that hides its own evidence of progress is dispiriting to look at.
/// Dropped is a different claim — "decided against this" — and belongs with `--all`
/// rather than in the everyday view.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Show {
    /// Open tasks only, the pre-0.2 default. `pd list --open`.
    Open,
    /// Open and done. The default.
    #[default]
    Done,
    /// Everything, dropped included. `pd list --all`.
    All,
}

impl Show {
    /// Whether a closed task of this state belongs in the listing.
    fn admits(self, state: TaskState) -> bool {
        match self {
            Show::Open => false,
            Show::Done => state == TaskState::Done,
            Show::All => true,
        }
    }
}

/// Build rows for the default tree view.
///
/// `sort` orders siblings **during** the walk, while the tree is still a tree. An earlier
/// version flattened first and then tried to rediscover the structure by reading depth
/// adjacency out of the flat list, which mistook "a depth-2 row after a depth-1 row" for
/// "a child of it" and glued unrelated subtrees together. `filter` is applied in the same
/// walk, and for the same reason.
pub fn tree_rows(state: &State, show: Show, filter: &Filter, sort: Option<Sort>) -> Vec<Row> {
    let paths = state.paths();
    let mut rows = Vec::new();

    // A blank line separates subtrees, but only where one actually exists. Flat lists
    // stay tight — the default view of ten items has to fit on a glance.
    let mut prev_had_children = false;
    for root in ordered(state.open_roots(), sort) {
        let mut subtree = collect(state, root, 1, &paths, filter, sort);
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

    if show != Show::Open {
        // Closed tasks sit in a flat block under the tree rather than back under their
        // parents. The open tree is the part you act on, and it stays legible only if
        // finished work does not push its live siblings apart.
        let mut closed: Vec<&crate::state::Task> = state
            .tasks
            .iter()
            .filter(|t| !t.state.is_open() && show.admits(t.state) && filter.admits(t))
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
    filter: &Filter,
    sort: Option<Sort>,
) -> Vec<Row> {
    let children: Vec<Row> = ordered(state.open_children(&task.id), sort)
        .into_iter()
        .flat_map(|c| collect(state, c, depth + 1, paths, filter, sort))
        .collect();

    // A parent is kept when it matches, or when any descendant does — otherwise a
    // filtered view would orphan its own results.
    if !filter.admits(task) && children.is_empty() {
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

    /// Every visible line of a coloured render, with the ANSI stripped, as the terminal
    /// would count them.
    fn visible_lines(out: &str) -> Vec<String> {
        let mut plain = String::new();
        let mut chars = out.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                plain.push(c);
            }
        }
        plain
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect()
    }

    #[test]
    fn no_line_is_wider_than_the_terminal() {
        let long = "write AGENTS.md with the RTL-safety and native-divergence rules";
        let rs = [
            row("rj8", "stand up the repo skeleton", 1, Some(1)),
            row("cxc", "init the pnpm monorepo with apps/api", 2, None),
            row("7r3", long, 2, None),
        ];
        for cols in [40usize, 56, 64, 72, 100, 200] {
            for line in visible_lines(&rows_at(&rs, Style { color: true }, now(), cols)) {
                assert!(
                    line.chars().count() <= cols.max(TEXT_COL_MIN + 12),
                    "at {cols} columns a line ran to {}: {line:?}",
                    line.chars().count()
                );
            }
        }
    }

    /// The bug this fixes: a title past the column pushed its id rightwards, so the ids
    /// no longer formed a column.
    #[test]
    fn every_id_lands_in_the_same_column() {
        let rs = [
            row("rj8", "short", 1, Some(1)),
            row("cxc", "a middling sort of task title here", 2, None),
            row(
                "7r3",
                "a title long enough that it has to wrap onto a second line",
                2,
                None,
            ),
        ];
        let lines = visible_lines(&rows_at(&rs, Style { color: true }, now(), 72));
        let columns_of_ids: Vec<usize> = lines
            .iter()
            .filter_map(|l| l.rfind(|c: char| !c.is_whitespace()).map(|_| l))
            .filter(|l| ["rj8", "cxc", "7r3"].iter().any(|id| l.ends_with(id)))
            .map(|l| l.chars().count() - 3)
            .collect();
        assert_eq!(columns_of_ids.len(), 3, "one id per task: {lines:?}");
        assert!(
            columns_of_ids.iter().all(|c| *c == columns_of_ids[0]),
            "ids must form a column, got {columns_of_ids:?} in {lines:?}"
        );
    }

    /// A wrapped title hangs under the text, not under the bullet, and keeps its id on
    /// the first line where the task is.
    #[test]
    fn a_wrapped_title_hangs_under_its_own_text() {
        let rs = [row(
            "7r3",
            "a title long enough that it has to wrap onto a second line",
            2,
            None,
        )];
        let lines = visible_lines(&rows_at(&rs, Style { color: true }, now(), 50));
        assert_eq!(lines.len(), 2, "should wrap once: {lines:?}");
        assert!(lines[0].ends_with("7r3"), "id on the first line");
        assert!(!lines[1].contains("7r3"), "and only there");
        // Counted in characters, not bytes — the bullet is three bytes wide and one
        // column wide, and it is the column that has to line up.
        let text_col = lines[0][..lines[0].find("a title").expect("text on line 1")]
            .chars()
            .count();
        let cont_col = lines[1].chars().take_while(|c| *c == ' ').count();
        assert_eq!(cont_col, text_col, "continuation aligns with the text");
    }

    /// A list with no dates in it should not reserve a date column — that waste is most
    /// of what pushed a narrow terminal into wrapping.
    #[test]
    fn a_list_without_dates_spends_nothing_on_a_date_column() {
        let text = "a task title of a very particular length indeed";
        let dated = Row {
            due: Some("2026-08-20".into()),
            ..row("aaa", text, 1, None)
        };
        let undated = row("aaa", text, 1, None);

        let width = |r: Row| {
            visible_lines(&rows_at(&[r], Style { color: true }, now(), 200))[0]
                .chars()
                .count()
        };
        assert!(
            width(undated) < width(dated),
            "the date column has to disappear when nothing is dated"
        );
    }

    #[test]
    fn a_word_longer_than_the_column_is_split_rather_than_overhanging() {
        let lines = wrap_text(&"x".repeat(30), 10);
        assert!(
            lines.iter().all(|l| l.chars().count() <= 10),
            "got {lines:?}"
        );
        assert_eq!(lines.concat(), "x".repeat(30), "and nothing is lost");
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
        let rs = tree_rows(&st, Show::Open, &Filter::default(), Some(Sort::Priority));
        assert_eq!(rs[0].id, "ccc", "p1 root sorts first");
        assert_eq!(rs[1].id, "aaa");
        assert_eq!(rs[2].id, "bbb", "the child stayed with its parent");
        assert_eq!(rs[2].depth, 2, "and stayed a child");
    }

    #[test]
    fn alpha_sort_orders_siblings() {
        let st = state_of(&[("aaa", "zebra", None, None), ("bbb", "apple", None, None)]);
        let rs = tree_rows(&st, Show::Open, &Filter::default(), Some(Sort::Alpha));
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
        let rs = tree_rows(&st, Show::Open, &Filter::default(), Some(Sort::Alpha));

        let order: Vec<&str> = rs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(order, ["apple", "dee parent", "ekko child", "zebra"]);
        let child = rs.iter().find(|r| r.id == "ccc").unwrap();
        assert_eq!(child.depth, 2);
        // The row before it is its real parent, not whatever sorted next to it.
        let pos = rs.iter().position(|r| r.id == "ccc").unwrap();
        assert_eq!(rs[pos - 1].id, "bbb");
    }

    /// A priority filter keeps a non-matching parent as context, exactly as a text filter
    /// does. Without it a p1 subtask renders at the same indent as unrelated p1 roots,
    /// which reads as "these are peers" when they are not.
    #[test]
    fn a_priority_filter_keeps_the_parent_that_did_not_match() {
        let st = state_of(&[
            ("aaa", "ship the migration", None, Some(3)),
            ("bbb", "backfill in batches", Some("aaa"), Some(1)),
            ("ccc", "unrelated", None, Some(4)),
        ]);
        let f = Filter::default().with_priority(Some(Some(1)));
        let rs = tree_rows(&st, Show::Open, &f, None);

        let order: Vec<&str> = rs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(order, ["ship the migration", "backfill in batches"]);
        assert_eq!(rs[1].depth, 2, "the match is still shown as a child");
        assert_eq!(rs[0].depth, 1, "and its parent is still its parent");
    }

    /// The context is context, not a result: a parent kept only to hold up a match must
    /// not drag in its other children.
    #[test]
    fn a_kept_parent_does_not_bring_its_non_matching_siblings() {
        let st = state_of(&[
            ("aaa", "parent", None, Some(3)),
            ("bbb", "urgent", Some("aaa"), Some(1)),
            ("ccc", "not urgent", Some("aaa"), Some(4)),
        ]);
        let f = Filter::default().with_priority(Some(Some(1)));
        let rs = tree_rows(&st, Show::Open, &f, None);

        let order: Vec<&str> = rs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(order, ["parent", "urgent"]);
    }

    /// `--priority none` selects the tasks nobody ranked. It is a real query, so it must
    /// not be confused with "no priority filter was given".
    #[test]
    fn filtering_for_unset_priority_is_a_real_query() {
        let st = state_of(&[
            ("aaa", "ranked", None, Some(1)),
            ("bbb", "unranked", None, None),
        ]);
        let f = Filter::default().with_priority(Some(None));
        let rs = tree_rows(&st, Show::Open, &f, None);
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].text, "unranked");
    }

    #[test]
    fn text_and_priority_filters_both_have_to_pass() {
        let st = state_of(&[
            ("aaa", "write the parser", None, Some(1)),
            ("bbb", "write the docs", None, Some(3)),
            ("ccc", "read the parser", None, Some(1)),
        ]);
        let f = Filter::text(Some("write")).with_priority(Some(Some(1)));
        let rs = tree_rows(&st, Show::Open, &f, None);
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].text, "write the parser");
    }
}
