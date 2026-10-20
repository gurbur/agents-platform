//! Inbox pages and the read marker (`IF-DB-001` tables `agents.inbox_head`,
//! `inbox_rows`, `messages`, `read_state`).
//!
//! Inbox rows are events with a `kind` (`IF-WIRE-012`); S13 writes only
//! message events. A marker that moves up also runs the read-marker
//! delivery transition (`IF-DLV-001`) in the same transaction: at the last
//! woken row, the agent is idle.

use platform_wire::{AgentId, InboxItem, Seq};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use super::delivery as delivery_queries;
use super::messages::{MESSAGE_COLUMNS, StoredMessage, stored_seq};
use crate::store::StoreError;

/// `inbox_rows.kind` of a message event.
pub const MESSAGE_EVENT: &str = platform_wire::inbox::MESSAGE_KIND;

/// A validated inbox request. Absent bounds take their defaults inside
/// [`page`], from the same reads as the rows.
pub struct InboxQuery {
    pub agent_id: AgentId,
    /// Exclusive lower bound; defaults to the agent's `read_marker`.
    pub after_seq: Option<Seq>,
    /// Inclusive upper bound; defaults to the current `inbox_head`.
    pub until_seq: Option<Seq>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxSlice {
    pub read_marker: Seq,
    pub snapshot_head_seq: Seq,
    /// The lower bound actually used, for `next_after_seq` of an empty page.
    pub after_seq: Seq,
    pub items: Vec<InboxItem>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadMarkerOutcome {
    /// The marker moved up to `through_seq`.
    Advanced(Seq),
    /// `through_seq` was at or below the marker, which stays as it is.
    Unchanged(Seq),
    /// `through_seq` is beyond `inbox_head`; nothing was written.
    Ahead,
}

/// The agent's head and marker as one row.
struct Cursor {
    inbox_head: Seq,
    read_marker: Seq,
}

/// One page of `query.agent_id`'s inbox, `after_seq < inbox_seq <=
/// snapshot_head_seq` in ascending order, fetching `limit + 1` rows to learn
/// `has_more`. `None` when the agent does not exist. Reads only.
///
/// Every statement runs on the DB thread's single connection, the only
/// writer, so the marker, head and rows form one consistent snapshot.
pub fn page(conn: &Connection, query: &InboxQuery) -> Result<Option<InboxSlice>, StoreError> {
    let Some(cursor) = cursor(conn, &query.agent_id)? else {
        return Ok(None);
    };
    let after_seq = query.after_seq.unwrap_or(cursor.read_marker);
    let snapshot_head_seq = query.until_seq.unwrap_or(cursor.inbox_head);

    let fetch = i64::from(query.limit) + 1;
    let mut statement = conn.prepare(&format!(
        "SELECT r.inbox_seq, r.kind, r.woke, {MESSAGE_COLUMNS}
         FROM inbox_rows r JOIN messages m ON m.message_id = r.message_id
         WHERE r.agent_id = ?1 AND r.inbox_seq > ?2 AND r.inbox_seq <= ?3
         ORDER BY r.inbox_seq ASC
         LIMIT ?4"
    ))?;
    let rows = statement.query_map(
        params![
            query.agent_id.as_str(),
            after_seq.get(),
            snapshot_head_seq.get(),
            fetch
        ],
        StoredItem::from_row,
    )?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?.into_item()?);
    }
    let has_more = items.len() > query.limit as usize;
    items.truncate(query.limit as usize);

    Ok(Some(InboxSlice {
        read_marker: cursor.read_marker,
        snapshot_head_seq,
        after_seq,
        items,
        has_more,
    }))
}

/// Raises the agent's marker to `through_seq` if it lies in
/// `(read_marker, inbox_head]`, then applies the read-marker delivery
/// transition. `None` when the agent does not exist.
pub fn advance_read_marker(
    tx: &Transaction,
    agent_id: &AgentId,
    through_seq: Seq,
) -> Result<Option<ReadMarkerOutcome>, StoreError> {
    let Some(cursor) = cursor(tx, agent_id)? else {
        return Ok(None);
    };
    if through_seq > cursor.inbox_head {
        return Ok(Some(ReadMarkerOutcome::Ahead));
    }
    if through_seq <= cursor.read_marker {
        return Ok(Some(ReadMarkerOutcome::Unchanged(cursor.read_marker)));
    }
    tx.execute(
        "UPDATE read_state SET read_marker = ?2 WHERE agent_id = ?1",
        params![agent_id.as_str(), through_seq.get()],
    )?;
    delivery_queries::record_read_marker(tx, agent_id)?;
    Ok(Some(ReadMarkerOutcome::Advanced(through_seq)))
}

fn cursor(conn: &Connection, agent_id: &AgentId) -> Result<Option<Cursor>, StoreError> {
    let found: Option<(i64, i64)> = conn
        .query_row(
            "SELECT a.inbox_head, s.read_marker
             FROM agents a JOIN read_state s ON s.agent_id = a.agent_id
             WHERE a.agent_id = ?1",
            params![agent_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((inbox_head, read_marker)) = found else {
        return Ok(None);
    };
    Ok(Some(Cursor {
        inbox_head: stored_seq(inbox_head, "agents.inbox_head")?,
        read_marker: stored_seq(read_marker, "read_state.read_marker")?,
    }))
}

/// An inbox row joined with its message, before validation.
struct StoredItem {
    inbox_seq: i64,
    kind: String,
    woke: bool,
    message: StoredMessage,
}

impl StoredItem {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            inbox_seq: row.get(0)?,
            kind: row.get(1)?,
            woke: row.get(2)?,
            message: StoredMessage::from_row(row, 3)?,
        })
    }

    fn into_item(self) -> Result<InboxItem, StoreError> {
        if self.kind != MESSAGE_EVENT {
            return Err(StoreError::Invariant(format!(
                "inbox_rows.kind '{}' is not written by this version",
                self.kind
            )));
        }
        Ok(InboxItem::message(
            stored_seq(self.inbox_seq, "inbox_rows.inbox_seq")?,
            self.woke,
            self.message.into_message()?,
        ))
    }
}
