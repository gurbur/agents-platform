//! Reactions (`IF-DB-001` v8 `reactions`, `reaction_events`,
//! `sequences('change_seq')`, `inbox_rows.change_seq`; `IF-WIRE-016`,
//! ADR-0014).
//!
//! `reactions` is the current state with tombstones (`removed = 1`);
//! `reaction_events` is the append-only log of every addition and removal
//! that the change feed and the author's `reaction` inbox event point at.
//! A repeated addition and a removal of nothing write nothing. Who may
//! react is who may read the message (S15's single-message rule).

use std::collections::HashMap;
use std::time::SystemTime;

use platform_wire::{
    AgentId, Message, MessageId, ReactResponse, ReactionChange, ReactionFeedPage, ReactionKind,
    ReactionSummary, RoomId, Seq, WakePolicy,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params, params_from_iter};

use super::agents::{agent_exists, stored_agent_id};
use super::delivery as delivery_queries;
use super::messages::{self as message_queries, stored_message_id, stored_seq};
use super::reaction_kinds::{self as kind_queries, stored_kind};
use super::rooms::{self as room_queries, Viewer};
use crate::store::StoreError;

/// `inbox_rows.kind` of a reaction event.
pub const REACTION_EVENT: &str = platform_wire::inbox::REACTION_KIND;

/// Who reacts: the calling agent, or the owner as an allowlisted identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reactor {
    Agent(AgentId),
    /// The owner reads every message; the identity must be registered.
    Owner(AgentId),
}

impl Reactor {
    pub fn agent_id(&self) -> &AgentId {
        match self {
            Reactor::Agent(agent_id) | Reactor::Owner(agent_id) => agent_id,
        }
    }

    fn viewer(&self) -> Viewer {
        match self {
            Reactor::Agent(agent_id) => Viewer::Agent(agent_id.clone()),
            Reactor::Owner(_) => Viewer::Owner,
        }
    }
}

/// A `PUT` or `DELETE …/reactions/{kind}` that passed every check not
/// needing the database (`IF-WIRE-016` 1–7). `created_at` is used only if
/// something is written; `wake` only by an addition.
pub struct ReactCommand {
    pub reactor: Reactor,
    pub message_id: MessageId,
    pub kind: ReactionKind,
    pub wake: bool,
    pub created_at: String,
}

/// Why a reaction request writes nothing (`IF-WIRE-016` 8–11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReactRejection {
    /// 8: the owner's identity is not registered.
    UnknownSender,
    /// 9: no such message, or the agent cannot read it.
    MessageNotFound,
    /// 10: the message's room is archived.
    RoomArchived,
    /// 11: the kind is not registered or is disabled (`PUT` only).
    UnknownKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReactOutcome {
    /// 13: added now. `author_row` says whether the author got a `reaction`
    /// inbox row, `woken` whether that row is in the wake set W.
    Created {
        response: ReactResponse,
        author_row: bool,
        woken: bool,
    },
    /// 12: already active; nothing was written.
    Existing(ReactResponse),
    Rejected(ReactRejection),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnreactOutcome {
    /// An active reaction was removed by this change.
    Removed(Seq),
    /// There was nothing to remove; nothing was written.
    Absent,
    Rejected(ReactRejection),
}

/// One page of a room's change feed.
pub struct FeedQuery {
    pub room: RoomId,
    /// `None` asks only for the room's current cursor.
    pub after: Option<Seq>,
    pub limit: u32,
}

/// `PUT /v1/messages/{message_id}/reactions/{kind}`, steps 8–13 of
/// `IF-WIRE-016` in one transaction. Every rejection happens before the
/// first write.
pub fn put(tx: &Transaction, command: &ReactCommand) -> Result<ReactOutcome, StoreError> {
    // 8–10
    let message = match target(tx, &command.reactor, &command.message_id)? {
        Ok(message) => message,
        Err(rejection) => return Ok(ReactOutcome::Rejected(rejection)),
    };
    // 11
    if !kind_queries::is_enabled(tx, &command.kind)? {
        return Ok(ReactOutcome::Rejected(ReactRejection::UnknownKind));
    }
    let agent_id = command.reactor.agent_id();
    // 12
    if let Some(active) = active(tx, &command.message_id, agent_id, &command.kind)? {
        let response = respond(tx, command, active.change_seq, active.created_at)?;
        return Ok(ReactOutcome::Existing(response));
    }
    // 13
    let change_seq = next_change_seq(tx)?;
    record_event(tx, change_seq, command, false)?;
    tx.execute(
        "INSERT INTO reactions (message_id, agent_id, kind, created_at, removed, change_seq)
         VALUES (?1, ?2, ?3, ?4, 0, ?5)
         ON CONFLICT (message_id, agent_id, kind)
         DO UPDATE SET removed = 0, created_at = excluded.created_at,
                       change_seq = excluded.change_seq",
        params![
            command.message_id.as_str(),
            agent_id.as_str(),
            command.kind.as_str(),
            command.created_at,
            change_seq.get()
        ],
    )?;
    let (author_row, woken) = notify_author(tx, command, &message, change_seq)?;
    let response = respond(tx, command, change_seq, command.created_at.clone())?;
    Ok(ReactOutcome::Created {
        response,
        author_row,
        woken,
    })
}

/// `DELETE /v1/messages/{message_id}/reactions/{kind}`, steps 8–10 of
/// `IF-WIRE-016` and then the removal of an active reaction. The kind's
/// registration is not checked. No inbox row, no delivery transition.
pub fn remove(tx: &Transaction, command: &ReactCommand) -> Result<UnreactOutcome, StoreError> {
    if let Err(rejection) = target(tx, &command.reactor, &command.message_id)? {
        return Ok(UnreactOutcome::Rejected(rejection));
    }
    let agent_id = command.reactor.agent_id();
    if active(tx, &command.message_id, agent_id, &command.kind)?.is_none() {
        return Ok(UnreactOutcome::Absent);
    }
    let change_seq = next_change_seq(tx)?;
    record_event(tx, change_seq, command, true)?;
    tx.execute(
        "UPDATE reactions SET removed = 1, change_seq = ?4
         WHERE message_id = ?1 AND agent_id = ?2 AND kind = ?3",
        params![
            command.message_id.as_str(),
            agent_id.as_str(),
            command.kind.as_str(),
            change_seq.get()
        ],
    )?;
    Ok(UnreactOutcome::Removed(change_seq))
}

/// The current reactions of every message in `ids` in one query: by kind
/// in registry order (`sort`, then slug), agents in ASCII order. Removed
/// reactions are left out; a disabled kind's and a deleted agent's stay.
pub fn summaries(
    conn: &Connection,
    ids: &[&MessageId],
) -> Result<HashMap<MessageId, Vec<ReactionSummary>>, StoreError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; ids.len()].join(", ");
    let mut statement = conn.prepare(&format!(
        "SELECT r.message_id, r.kind, r.agent_id
         FROM reactions r JOIN reaction_kinds k ON k.slug = r.kind
         WHERE r.removed = 0 AND r.message_id IN ({placeholders})
         ORDER BY r.message_id, k.sort, k.slug, r.agent_id"
    ))?;
    let rows = statement.query_map(params_from_iter(ids.iter().map(|id| id.as_str())), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut found: HashMap<MessageId, Vec<ReactionSummary>> = HashMap::new();
    for row in rows {
        let (message_id, kind, agent_id) = row?;
        let kinds = found.entry(stored_message_id(message_id)?).or_default();
        let kind = stored_kind(kind)?;
        let agent_id = stored_agent_id(agent_id)?;
        match kinds.last_mut() {
            Some(last) if last.kind == kind => {
                last.agents.push(agent_id);
                last.count += 1;
            }
            _ => kinds.push(ReactionSummary {
                kind,
                count: 1,
                agents: vec![agent_id],
            }),
        }
    }
    Ok(found)
}

/// Fills `reactions` of every message (`IF-WIRE-012`: counted when read).
pub fn attach_summaries<'a>(
    conn: &Connection,
    messages: impl IntoIterator<Item = &'a mut Message>,
) -> Result<(), StoreError> {
    let mut messages: Vec<&'a mut Message> = messages.into_iter().collect();
    let ids: Vec<MessageId> = messages
        .iter()
        .map(|message| message.message_id.clone())
        .collect();
    let mut found = summaries(conn, &ids.iter().collect::<Vec<_>>())?;
    for message in &mut messages {
        message.reactions = found.remove(&message.message_id).unwrap_or_default();
    }
    Ok(())
}

/// `GET /v1/reactions` for a caller allowed to read the room: the room's
/// changes after `after` in `change_seq` order, fetching `limit + 1` to
/// learn `has_more`; without `after`, no items and the room's cursor.
pub fn feed(conn: &Connection, query: &FeedQuery) -> Result<ReactionFeedPage, StoreError> {
    let room = query.room.to_string();
    let Some(after) = query.after else {
        let latest: Option<i64> = conn.query_row(
            "SELECT MAX(e.change_seq) FROM reaction_events e
             JOIN messages m ON m.message_id = e.message_id
             WHERE m.room_id = ?1",
            params![room],
            |row| row.get(0),
        )?;
        return Ok(ReactionFeedPage {
            items: Vec::new(),
            next_after_change_seq: stored_seq(latest.unwrap_or(0), "reaction_events.change_seq")?,
            has_more: false,
        });
    };
    let fetch = i64::from(query.limit) + 1;
    let mut statement = conn.prepare(
        "SELECT e.change_seq, e.message_id, e.agent_id, e.kind, e.removed, e.created_at
         FROM reaction_events e JOIN messages m ON m.message_id = e.message_id
         WHERE m.room_id = ?1 AND e.change_seq > ?2
         ORDER BY e.change_seq ASC
         LIMIT ?3",
    )?;
    let rows = statement.query_map(params![room, after.get(), fetch], StoredChange::from_row)?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?.into_change()?);
    }
    let has_more = items.len() > query.limit as usize;
    items.truncate(query.limit as usize);
    let next_after_change_seq = items.last().map_or(after, |change| change.change_seq);
    Ok(ReactionFeedPage {
        items,
        next_after_change_seq,
        has_more,
    })
}

/// Steps 8–10: the owner's identity is registered, the message exists and
/// the reactor reads it (the owner always does), and its room is not
/// archived.
fn target(
    conn: &Connection,
    reactor: &Reactor,
    message_id: &MessageId,
) -> Result<Result<Message, ReactRejection>, StoreError> {
    if let Reactor::Owner(sender) = reactor
        && !agent_exists(conn, sender)?
    {
        return Ok(Err(ReactRejection::UnknownSender));
    }
    let Some(message) = message_queries::find_message(conn, message_id)? else {
        return Ok(Err(ReactRejection::MessageNotFound));
    };
    if !room_queries::can_read_message(conn, &reactor.viewer(), &message)? {
        return Ok(Err(ReactRejection::MessageNotFound));
    }
    let archived = room_queries::find(conn, &message.room_id)?
        .as_ref()
        .is_some_and(room_queries::RoomRow::is_archived);
    if archived {
        return Ok(Err(ReactRejection::RoomArchived));
    }
    Ok(Ok(message))
}

/// The active reaction's last change and when it was added.
struct Active {
    change_seq: Seq,
    created_at: String,
}

fn active(
    conn: &Connection,
    message_id: &MessageId,
    agent_id: &AgentId,
    kind: &ReactionKind,
) -> Result<Option<Active>, StoreError> {
    let found: Option<(i64, String)> = conn
        .query_row(
            "SELECT change_seq, created_at FROM reactions
             WHERE message_id = ?1 AND agent_id = ?2 AND kind = ?3 AND removed = 0",
            params![message_id.as_str(), agent_id.as_str(), kind.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((change_seq, created_at)) = found else {
        return Ok(None);
    };
    Ok(Some(Active {
        change_seq: stored_seq(change_seq, "reactions.change_seq")?,
        created_at,
    }))
}

/// The response body: the reaction and the message's summary now.
fn respond(
    conn: &Connection,
    command: &ReactCommand,
    change_seq: Seq,
    created_at: String,
) -> Result<ReactResponse, StoreError> {
    let reactions = summaries(conn, &[&command.message_id])?
        .remove(&command.message_id)
        .unwrap_or_default();
    Ok(ReactResponse {
        message_id: command.message_id.clone(),
        agent_id: command.reactor.agent_id().clone(),
        kind: command.kind.clone(),
        change_seq,
        created_at,
        reactions,
    })
}

/// Issues the next `change_seq` inside the caller's transaction, so a
/// rollback leaves no hole.
fn next_change_seq(tx: &Transaction) -> Result<Seq, StoreError> {
    let value: i64 = tx.query_row(
        "UPDATE sequences SET value = value + 1 WHERE name = 'change_seq' RETURNING value",
        [],
        |row| row.get(0),
    )?;
    stored_seq(value, "sequences.change_seq")
}

/// Appends the addition (with the request's `wake`) or removal (`wake = 0`).
fn record_event(
    tx: &Transaction,
    change_seq: Seq,
    command: &ReactCommand,
    removed: bool,
) -> Result<(), StoreError> {
    let wake = !removed && command.wake;
    tx.execute(
        "INSERT INTO reaction_events (change_seq, message_id, agent_id, kind, removed, wake, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            change_seq.get(),
            command.message_id.as_str(),
            command.reactor.agent_id().as_str(),
            command.kind.as_str(),
            removed,
            wake,
            command.created_at
        ],
    )?;
    Ok(())
}

/// The author's `reaction` inbox row (`woke` = the request's `wake`)
/// unless the author reacted itself or is not registered, and its wake:
/// W when `wake` or the author's `wake_policy = always` (`IF-DLV-001` S16).
/// Returns whether a row was written and whether it is in W.
fn notify_author(
    tx: &Transaction,
    command: &ReactCommand,
    message: &Message,
    change_seq: Seq,
) -> Result<(bool, bool), StoreError> {
    let author = &message.sender_id;
    if author == command.reactor.agent_id() || !agent_exists(tx, author)? {
        return Ok((false, false));
    }
    let inbox_seq: i64 = tx.query_row(
        "UPDATE agents SET inbox_head = inbox_head + 1 WHERE agent_id = ?1 RETURNING inbox_head",
        params![author.as_str()],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO inbox_rows (agent_id, inbox_seq, message_id, kind, wake, woke, change_seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
        params![
            author.as_str(),
            inbox_seq,
            message.message_id.as_str(),
            REACTION_EVENT,
            command.wake,
            change_seq.get()
        ],
    )?;
    let always = message_queries::wake_policy(tx, author)? == WakePolicy::Always;
    let woken = command.wake || always;
    if woken {
        delivery_queries::record_new_inbox_row(tx, author, SystemTime::now())?;
    }
    Ok((true, woken))
}

/// A `reaction_events` row before validation.
struct StoredChange {
    change_seq: i64,
    message_id: String,
    agent_id: String,
    kind: String,
    removed: bool,
    created_at: String,
}

impl StoredChange {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            change_seq: row.get(0)?,
            message_id: row.get(1)?,
            agent_id: row.get(2)?,
            kind: row.get(3)?,
            removed: row.get(4)?,
            created_at: row.get(5)?,
        })
    }

    fn into_change(self) -> Result<ReactionChange, StoreError> {
        Ok(ReactionChange {
            change_seq: stored_seq(self.change_seq, "reaction_events.change_seq")?,
            message_id: stored_message_id(self.message_id)?,
            agent_id: stored_agent_id(self.agent_id)?,
            kind: stored_kind(self.kind)?,
            removed: self.removed,
            created_at: self.created_at,
        })
    }
}
