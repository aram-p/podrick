//! The ledger. Every mutation appends one line of JSON here; nothing is ever rewritten.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    /// A human at a terminal.
    Cli,
    /// Anything without a TTY on stdin.
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Created,
    Completed,
    Uncompleted,
    Dropped,
    Edited,
    Reprioritized,
    Rescheduled,
    Moved,
    Noted,
    Compacted,
}

impl Kind {
    /// The event that undoes this one. `None` means the event is not undoable.
    pub fn inverse(self) -> Option<Kind> {
        match self {
            Kind::Completed => Some(Kind::Uncompleted),
            // Undoing a reopen re-completes; undoing a drop reopens.
            Kind::Uncompleted => Some(Kind::Completed),
            Kind::Dropped => Some(Kind::Uncompleted),
            Kind::Edited => Some(Kind::Edited),
            Kind::Reprioritized => Some(Kind::Reprioritized),
            Kind::Rescheduled => Some(Kind::Rescheduled),
            Kind::Moved => Some(Kind::Moved),
            // `created` has no compensating event that preserves history, and `noted`
            // / `compacted` are not state changes.
            Kind::Created | Kind::Noted | Kind::Compacted => None,
        }
    }
}

/// Per-event payload. Every field is optional; which ones are populated depends on `Kind`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Data {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    /// Previous value, for `edited` / `reprioritized` / `rescheduled`. May be `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<Value>,
    /// New value, for the same three. May be `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_parent: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_parent: Option<Value>,
    /// Full state, carried by `compacted`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Vec<crate::state::Task>>,
    /// Set when a parent's completion cascaded to this descendant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cascaded_from: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub v: u32,
    pub seq: u64,
    /// RFC 3339, with local offset.
    pub ts: String,
    pub actor: Actor,
    pub ev: Kind,
    /// The task this event concerns. Empty for file-level events like `compacted`.
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The seq this event compensates for, when produced by `pd undo`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undo_of: Option<u64>,
    /// Groups the events of a single user action — a completion and the subtasks it
    /// cascaded to. Set to the seq of the action's primary event, on every member
    /// including the primary itself. `pd undo` reverts a whole batch, never half of one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch: Option<u64>,
    #[serde(default)]
    pub data: Data,
}

impl Event {
    pub fn new(seq: u64, ts: String, actor: Actor, ev: Kind, id: impl Into<String>) -> Self {
        Event {
            v: FORMAT_VERSION,
            seq,
            ts,
            actor,
            ev,
            id: id.into(),
            note: None,
            undo_of: None,
            batch: None,
            data: Data::default(),
        }
    }

    pub fn with_note(mut self, note: Option<String>) -> Self {
        self.note = note;
        self
    }

    pub fn with_data(mut self, data: Data) -> Self {
        self.data = data;
        self
    }

    pub fn undoing(mut self, seq: u64) -> Self {
        self.undo_of = Some(seq);
        self
    }

    pub fn in_batch(mut self, primary_seq: u64) -> Self {
        self.batch = Some(primary_seq);
        self
    }

    /// The primary event of its batch — the one the user actually asked for.
    pub fn is_primary(&self) -> bool {
        self.batch.is_none_or(|b| b == self.seq)
    }
}

/// Parse a JSONL log. Unparseable lines are skipped rather than fatal: a torn line
/// should cost you one event, not the whole file.
pub fn parse_log(contents: &str) -> (Vec<Event>, usize) {
    let mut events = Vec::new();
    let mut skipped = 0;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Event>(line) {
            Ok(e) => events.push(e),
            Err(_) => skipped += 1,
        }
    }
    (events, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let e = Event::new(
            1,
            "2026-08-12T14:03:11+04:00".into(),
            Actor::Cli,
            Kind::Created,
            "a7f",
        )
        .with_data(Data {
            text: Some("write the spec".into()),
            ..Default::default()
        });
        let line = serde_json::to_string(&e).unwrap();
        let (back, skipped) = parse_log(&line);
        assert_eq!(skipped, 0);
        assert_eq!(back[0].id, "a7f");
        assert_eq!(back[0].data.text.as_deref(), Some("write the spec"));
    }

    #[test]
    fn absent_fields_are_omitted() {
        let e = Event::new(1, "t".into(), Actor::Agent, Kind::Completed, "a7f");
        let line = serde_json::to_string(&e).unwrap();
        assert!(!line.contains("snapshot"));
        assert!(!line.contains("note"));
        assert!(line.contains("\"actor\":\"agent\""));
    }

    #[test]
    fn a_torn_line_costs_one_event_not_the_file() {
        let good =
            serde_json::to_string(&Event::new(1, "t".into(), Actor::Cli, Kind::Created, "a7f"))
                .unwrap();
        let log = format!("{good}\n{{\"v\":1,\"seq\":2,\"ts\"\n{good}\n");
        let (events, skipped) = parse_log(&log);
        assert_eq!(events.len(), 2);
        assert_eq!(skipped, 1);
    }
}
