//! Threads (`IF-DB-001` v7 `messages.thread_id`, `thread_participants`;
//! `IF-WIRE-014`, ADR-0013).
//!
//! A thread is open once its root (a message with `thread_id IS NULL`) has
//! a reply. Participant rows are written only by the send transaction
//! (`INSERT OR IGNORE`, registered agents only), by a participant's mute
//! toggle and by the agent deletion cascade.

use std::collections::HashMap;

use platform_wire::{
    AgentId, JoinedVia, Message, MessageId, RoomId, ThreadParticipant, ThreadSummary, ThreadView,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params, params_from_iter};

use super::agents::stored_agent_id;
use super::messages::{stored_message_id, stored_seq};
use super::rooms::stored_room_id;
use crate::store::StoreError;

/// A participant's id and whether it muted the thread.
pub struct ParticipantMute {
    pub agent_id: AgentId,
    pub muted: bool,
}

/// The room of `root` when it is the root of an open thread: the message
/// exists, is in no thread itself and has at least one reply.
pub fn open_root_room(conn: &Connection, root: &MessageId) -> Result<Option<RoomId>, StoreError> {
    let room: Option<String> = conn
        .query_row(
            "SELECT r.room_id FROM messages r
             WHERE r.message_id = ?1 AND r.thread_id IS NULL
               AND EXISTS (SELECT 1 FROM messages t WHERE t.thread_id = r.message_id)",
            params![root.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    room.map(|room| stored_room_id(&room)).transpose()
}

/// `GET /v1/threads/{root_id}` for a caller allowed to read it; `None`
/// when `root` is no open thread.
pub fn view(conn: &Connection, root: &MessageId) -> Result<Option<ThreadView>, StoreError> {
    let stored: Option<(String, String, i64, i64, String)> = conn
        .query_row(
            "SELECT r.room_id, r.sender_id, s.reply_count, s.last_seq, l.created_at
             FROM messages r
             JOIN (SELECT COUNT(*) AS reply_count, MAX(event_seq) AS last_seq
                   FROM messages WHERE thread_id = ?1) s
             JOIN messages l ON l.event_seq = s.last_seq
             WHERE r.message_id = ?1 AND r.thread_id IS NULL",
            params![root.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((room_id, sender_id, reply_count, last_seq, last_message_at)) = stored else {
        return Ok(None);
    };
    let room_id = stored_room_id(&room_id)?;
    Ok(Some(ThreadView {
        thread_id: root.clone(),
        room_kind: room_id.kind(),
        room_id,
        root_sender_id: stored_agent_id(sender_id)?,
        participants: participants(conn, root)?,
        reply_count: stored_count(reply_count)?,
        last_event_seq: stored_seq(last_seq, "messages.event_seq")?,
        last_message_at,
    }))
}

/// Whether `agent` is a participant of the thread rooted at `root`.
pub fn is_participant(
    conn: &Connection,
    root: &MessageId,
    agent: &AgentId,
) -> Result<bool, StoreError> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM thread_participants WHERE thread_id = ?1 AND agent_id = ?2",
            params![root.as_str(), agent.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

/// `PUT /v1/threads/{root_id}/participation`: sets `muted` unless it
/// already has that value. `None` when `agent` is no participant (which
/// includes `root` not being an open thread).
pub fn set_muted(
    tx: &Transaction,
    root: &MessageId,
    agent: &AgentId,
    muted: bool,
) -> Result<Option<ThreadParticipant>, StoreError> {
    let Some(current) = participant(tx, root, agent)? else {
        return Ok(None);
    };
    if current.muted == muted {
        return Ok(Some(current));
    }
    tx.execute(
        "UPDATE thread_participants SET muted = ?3 WHERE thread_id = ?1 AND agent_id = ?2",
        params![root.as_str(), agent.as_str(), muted],
    )?;
    Ok(Some(ThreadParticipant { muted, ..current }))
}

/// The summary of every open thread rooted at one of `roots`, in one query
/// covered by `messages_thread_event(thread_id, event_seq)`.
pub fn summaries(
    conn: &Connection,
    roots: &[&MessageId],
) -> Result<HashMap<MessageId, ThreadSummary>, StoreError> {
    if roots.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; roots.len()].join(", ");
    let mut statement = conn.prepare(&format!(
        "SELECT thread_id, COUNT(*), MAX(event_seq) FROM messages
         WHERE thread_id IN ({placeholders}) GROUP BY thread_id"
    ))?;
    let rows = statement.query_map(
        params_from_iter(roots.iter().map(|id| id.as_str())),
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        },
    )?;
    let mut found = HashMap::new();
    for row in rows {
        let (root, count, last_seq) = row?;
        found.insert(
            stored_message_id(root)?,
            ThreadSummary {
                reply_count: stored_count(count)?,
                last_event_seq: stored_seq(last_seq, "messages.event_seq")?,
            },
        );
    }
    Ok(found)
}

/// Fills `thread_summary` of every root among `messages` that has replies
/// (`IF-WIRE-012`: counted when read).
pub fn attach_summaries<'a>(
    conn: &Connection,
    messages: impl IntoIterator<Item = &'a mut Message>,
) -> Result<(), StoreError> {
    let mut roots: Vec<&'a mut Message> = messages
        .into_iter()
        .filter(|message| message.thread_id.is_none())
        .collect();
    let ids: Vec<MessageId> = roots.iter().map(|root| root.message_id.clone()).collect();
    let found = summaries(conn, &ids.iter().collect::<Vec<_>>())?;
    for root in &mut roots {
        root.thread_summary = found.get(&root.message_id).copied();
    }
    Ok(())
}

/// Whether the thread rooted at `root` already has a reply.
pub(crate) fn has_replies(conn: &Connection, root: &MessageId) -> Result<bool, StoreError> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM messages WHERE thread_id = ?1 LIMIT 1",
            params![root.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

/// Registers `agent` as a participant unless it already is one or is not
/// a registered agent (`INSERT OR IGNORE`, `IF-WIRE-014`).
pub(crate) fn join(
    tx: &Transaction,
    root: &MessageId,
    agent: &AgentId,
    via: JoinedVia,
    joined_at: &str,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT OR IGNORE INTO thread_participants (thread_id, agent_id, joined_via, muted, joined_at)
         SELECT ?1, agent_id, ?3, 0, ?4 FROM agents WHERE agent_id = ?2",
        params![root.as_str(), agent.as_str(), via.as_str(), joined_at],
    )?;
    Ok(())
}

/// Every participant of the thread with its mute flag, in ASCII order.
pub(crate) fn mutes(
    conn: &Connection,
    root: &MessageId,
) -> Result<Vec<ParticipantMute>, StoreError> {
    let mut statement = conn.prepare(
        "SELECT agent_id, muted FROM thread_participants WHERE thread_id = ?1 ORDER BY agent_id",
    )?;
    let rows = statement.query_map(params![root.as_str()], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
    })?;
    let mut found = Vec::new();
    for row in rows {
        let (agent_id, muted) = row?;
        found.push(ParticipantMute {
            agent_id: stored_agent_id(agent_id)?,
            muted,
        });
    }
    Ok(found)
}

const PARTICIPANT_COLUMNS: &str = "agent_id, joined_via, muted, joined_at";

/// The participants in `joined_at` order, then `agent_id`.
fn participants(conn: &Connection, root: &MessageId) -> Result<Vec<ThreadParticipant>, StoreError> {
    let mut statement = conn.prepare(&format!(
        "SELECT {PARTICIPANT_COLUMNS} FROM thread_participants WHERE thread_id = ?1
         ORDER BY joined_at, agent_id"
    ))?;
    let rows = statement.query_map(params![root.as_str()], StoredParticipant::from_row)?;
    let mut found = Vec::new();
    for row in rows {
        found.push(row?.into_participant()?);
    }
    Ok(found)
}

fn participant(
    conn: &Connection,
    root: &MessageId,
    agent: &AgentId,
) -> Result<Option<ThreadParticipant>, StoreError> {
    let stored = conn
        .query_row(
            &format!(
                "SELECT {PARTICIPANT_COLUMNS} FROM thread_participants
                 WHERE thread_id = ?1 AND agent_id = ?2"
            ),
            params![root.as_str(), agent.as_str()],
            StoredParticipant::from_row,
        )
        .optional()?;
    stored.map(StoredParticipant::into_participant).transpose()
}

fn stored_count(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::Invariant("negative reply count".to_owned()))
}

/// A [`PARTICIPANT_COLUMNS`] row before validation.
struct StoredParticipant {
    agent_id: String,
    joined_via: String,
    muted: bool,
    joined_at: String,
}

impl StoredParticipant {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            agent_id: row.get(0)?,
            joined_via: row.get(1)?,
            muted: row.get(2)?,
            joined_at: row.get(3)?,
        })
    }

    fn into_participant(self) -> Result<ThreadParticipant, StoreError> {
        let joined_via = JoinedVia::parse(&self.joined_via).ok_or_else(|| {
            StoreError::Invariant(format!(
                "thread_participants.joined_via '{}' is unknown",
                self.joined_via
            ))
        })?;
        Ok(ThreadParticipant {
            agent_id: stored_agent_id(self.agent_id)?,
            joined_via,
            muted: self.muted,
            joined_at: self.joined_at,
        })
    }
}
