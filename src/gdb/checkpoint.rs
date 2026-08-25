//! rr-style checkpoints (`qRRCmd checkpoint` / `vRun;c<N>`).
//!
//! rr's checkpoints are real process snapshots; a TTD trace needs none —
//! jumping to a recorded position is free — so a checkpoint is just a saved
//! `TtdPosition` plus the rr-compatible text the client parses back.
//!
//! Self-contained state + formatting, hence unit-testable without a stub.

use std::collections::BTreeMap;

use crate::ttd::types::TtdPosition;

/// Render a trace position as rr's `when` string (`SEQ:STEPS`, hex).
pub fn format_position(pos: TtdPosition) -> String {
    format!("{:X}:{:X}", pos.sequence, pos.steps)
}

/// A saved trace position.
struct Checkpoint {
    position: TtdPosition,
    when: String,
    where_: String,
}

/// The checkpoint table backing `qRRCmd checkpoint` / `info checkpoints` /
/// `delete checkpoint`.
///
/// Ids start at 1: `0` is reserved as "no checkpoint" everywhere else in the
/// stub, and rr's `vRun;c<N>` ids are 1-based too.
pub struct CheckpointTable {
    /// BTreeMap so `info checkpoints` lists ids in numeric order.
    by_id: BTreeMap<u64, Checkpoint>,
    next_id: u64,
}

impl Default for CheckpointTable {
    fn default() -> Self {
        Self::new()
    }
}

impl CheckpointTable {
    pub fn new() -> Self {
        Self {
            by_id: BTreeMap::new(),
            next_id: 1,
        }
    }

    /// Save `pos` under a fresh id, returning the id and the rr-compatible
    /// acknowledgement the client parses.
    pub fn insert(&mut self, pos: TtdPosition, where_: &str) -> (u64, String) {
        let id = self.next_id;
        self.next_id += 1;
        let when = format_position(pos);
        // `where_` is client-controlled (hex-decoded from the qRRCmd line):
        // a raw tab/newline would break the `info checkpoints` row format
        // Delve parses (split on `\t`, exactly 3 fields per line).
        let where_ = where_.replace(['\t', '\n', '\r'], " ");
        self.by_id.insert(
            id,
            Checkpoint {
                position: pos,
                when: when.clone(),
                where_: where_.clone(),
            },
        );
        // Delve parses "Checkpoint " + id + " " — keep that shape.
        let ack = if where_.is_empty() {
            format!("Checkpoint {id} at {when}")
        } else {
            format!("Checkpoint {id} at {when} {where_}")
        };
        (id, ack)
    }

    /// The position a `vRun;c<id>` restart should jump to.
    pub fn position(&self, id: u64) -> Option<TtdPosition> {
        self.by_id.get(&id).map(|c| c.position)
    }

    pub fn remove(&mut self, id: u64) -> bool {
        self.by_id.remove(&id).is_some()
    }

    /// `info checkpoints`: a header line plus one tab-separated row per
    /// checkpoint (Delve splits on `\t` and requires exactly 3 fields).
    pub fn render(&self) -> String {
        let mut out = String::from("Num\tWhen\tWhere\n");
        for (id, cp) in &self.by_id {
            let where_ = if cp.where_.is_empty() {
                "-"
            } else {
                &cp.where_
            };
            out.push_str(&format!("{}\t{}\t{}\n", id, cp.when, where_));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(sequence: u64, steps: u64) -> TtdPosition {
        TtdPosition { sequence, steps }
    }

    #[test]
    fn format_position_matches_rr_when_syntax() {
        assert_eq!(format_position(pos(1, 0)), "1:0");
        assert_eq!(format_position(pos(0xA, 0x1F)), "A:1F");
        assert_eq!(format_position(pos(0xc0, 0x1f)), "C0:1F");
    }

    #[test]
    fn insert_returns_parseable_acknowledgement() {
        let mut table = CheckpointTable::new();
        let (id, ack) = table.insert(pos(1, 0), "main.main");
        assert_eq!(id, 1);
        // Delve's parser: prefix "Checkpoint ", id up to the next space.
        assert_eq!(ack, "Checkpoint 1 at 1:0 main.main");
        assert_eq!(table.position(id), Some(pos(1, 0)));

        // No "where" string: no dangling space (Delve still finds the id).
        let (id2, ack2) = table.insert(pos(2, 5), "");
        assert_eq!(id2, 2);
        assert_eq!(ack2, "Checkpoint 2 at 2:5");
    }

    #[test]
    fn ids_are_stable_and_monotonic() {
        let mut table = CheckpointTable::new();
        let (a, _) = table.insert(pos(1, 0), "a");
        let (b, _) = table.insert(pos(2, 0), "b");
        assert!(b > a);
        // Removing one must not renumber or resurrect an id.
        assert!(table.remove(a));
        assert_eq!(table.position(a), None);
        assert!(!table.remove(a));
        assert_eq!(table.position(b), Some(pos(2, 0)));
        let (c, _) = table.insert(pos(3, 0), "c");
        assert!(c > b, "ids must never be reused");
    }

    /// `Default` must behave exactly like `new()`: id 0 is "no checkpoint"
    /// everywhere else in the stub, so a derived `Default` handing out 0
    /// would be a silent trap.
    #[test]
    fn default_matches_new() {
        let mut table = CheckpointTable::default();
        let (id, ack) = table.insert(pos(1, 0), "a");
        assert_eq!(id, 1, "ids must start at 1");
        assert_eq!(ack, "Checkpoint 1 at 1:0 a");
    }

    #[test]
    fn render_lists_checkpoints_in_id_order() {
        let mut table = CheckpointTable::new();
        let (_later_pos, _) = table.insert(pos(2, 0), "saved-later");
        let (_earlier_pos, _) = table.insert(pos(1, 0), "saved-first");
        let rendered = table.render();
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines[0], "Num\tWhen\tWhere");
        // Rows follow the id, not the insertion order and not the position.
        assert_eq!(lines[1], "1\t2:0\tsaved-later");
        assert_eq!(lines[2], "2\t1:0\tsaved-first");
    }

    #[test]
    fn render_uses_dash_for_missing_where() {
        let mut table = CheckpointTable::new();
        table.insert(pos(1, 0), "");
        assert!(
            table.render().ends_with("1\t1:0\t-\n"),
            "{}",
            table.render()
        );
    }

    #[test]
    fn empty_table_renders_header_only() {
        // Delve skips the header and expects zero rows.
        assert_eq!(CheckpointTable::new().render(), "Num\tWhen\tWhere\n");
    }

    #[test]
    fn where_string_is_sanitized_for_row_format() {
        // `where_` comes hex-decoded from the client; embedded tabs/newlines
        // must not be able to forge extra `info checkpoints` columns or rows.
        let mut table = CheckpointTable::new();
        let (id, ack) = table.insert(pos(1, 0), "evil\trow\nforgery\r\n");
        assert_eq!(ack, "Checkpoint 1 at 1:0 evil row forgery  ");
        let rendered = table.render();
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].split('\t').count(), 3, "{}", rendered);
        assert!(table.position(id).is_some());
    }
}
