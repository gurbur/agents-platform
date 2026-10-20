//! The one send (`IF-WIRE-010` steps 12–18: durable commit with
//! idempotency, room resolution, permission, inbox fanout and wake set) and
//! history reads (`IF-DB-001` tables `messages`, `message_mentions`,
//! `inbox_rows`, `idempotency`, `sequences`, `rooms`, `memberships`).

use std::time::SystemTime;

use platform_wire::{AgentId, Message, MessageId, RoomId, Seq, WakePolicy};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use sha2::{Digest, Sha256};

use super::agents::{agent_exists, stored_agent_id};
use super::delivery as delivery_queries;
use super::inbox::MESSAGE_EVENT;
use super::rooms::{self as room_queries, RoomRow, stored_room_id};
use crate::store::StoreError;

/// The columns [`StoredMessage::from_row`] reads, for a `messages` table
/// aliased `m`. Mentions come as one comma-joined text (an `agent_id` holds
/// no comma) and are sorted on reading.
pub(crate) const MESSAGE_COLUMNS: &str = "m.message_id, m.event_seq, m.room_id, m.sender_id, \
     m.body, m.reply_to, m.wake, m.created_at, \
     (SELECT group_concat(agent_id, ',') FROM message_mentions mm WHERE mm.message_id = m.message_id)";

/// Who calls `POST /v1/messages`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendAs {
    /// The sender is the calling agent.
    Agent,
    /// The owner posts as an allowlisted `sender_id` (ADR-0007).
    Owner,
}

/// The one destination of a send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    Room(RoomId),
    /// The room of the message replied to.
    ReplyTo(MessageId),
    /// The DM between the sender and this agent.
    Direct(AgentId),
}

/// A send that passed every check not needing the database (`IF-WIRE-010`
/// 1–11). `message_id` and `created_at` are used only if the request is new.
pub struct SendCommand {
    pub principal: SendAs,
    pub sender: AgentId,
    pub client_key: String,
    pub destination: Destination,
    pub body: String,
    pub wake: bool,
    /// ASCII ascending, without duplicates.
    pub mentions: Vec<AgentId>,
    /// `[owner] post_as`: identities that may be mentioned in any room.
    pub post_as: Vec<AgentId>,
    pub message_id: MessageId,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    /// Committed now: `recipients` inbox rows (R), `woken` of them with the
    /// new-wake-row delivery transition (W).
    Created {
        message: Message,
        recipients: usize,
        woken: usize,
    },
    /// The same canonical request was already committed under this key.
    Deduplicated(Message),
    Rejected(SendRejection),
}

/// Reasons a send commits nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendRejection {
    /// The key is bound to a different canonical request.
    IdempotencyConflict,
    /// `to` names the sender. The handler refuses it first; this is the
    /// store's own guard.
    SelfDm,
    UnknownRecipient,
    /// The owner named a room that does not exist, or a DM member is not
    /// registered.
    UnknownRoom,
    /// Owner only: the allowlisted `sender_id` is not registered.
    UnknownSender,
    /// An agent without a joined membership of the room (or no room).
    NotRoomMember,
    RoomArchived,
    ReplyToNotFound,
    /// The first mention that is neither a joined member nor a registered
    /// `post_as` identity.
    MentionNotMember(AgentId),
}

/// History direction and exclusive bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryDirection {
    /// Newest first, `event_seq < bound` when a bound is given.
    Before(Option<Seq>),
    /// Oldest first, `event_seq > bound`.
    After(Seq),
}

pub struct HistoryQuery {
    pub room: RoomId,
    pub direction: HistoryDirection,
    pub limit: u32,
    /// Literal substring of `body`; no wildcard interpretation.
    pub contains: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistorySlice {
    pub items: Vec<Message>,
    pub has_more: bool,
}

/// SHA-256 hex of `room 0x00 body 0x00 reply_to 0x00 wake 0x00 mentions`
/// (`IF-DB-001` S13): the resolved room id, the body and `reply_to` as sent
/// (absent is empty), `1`/`0`, and the mentions in ASCII order joined by
/// `,` (none is empty). No trimming or normalisation.
pub fn canonical_request_hash(
    room: &str,
    body: &str,
    reply_to: Option<&str>,
    wake: bool,
    mentions: &[AgentId],
) -> String {
    let mut sorted: Vec<&str> = mentions.iter().map(AgentId::as_str).collect();
    sorted.sort_unstable();
    let mut hasher = Sha256::new();
    for (index, part) in [
        room,
        body,
        reply_to.unwrap_or_default(),
        if wake { "1" } else { "0" },
        &sorted.join(","),
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            hasher.update([0u8]);
        }
        hasher.update(part.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The `POST /v1/messages` transaction, `IF-WIRE-010` steps 12–18 in order.
/// Every rejection happens before the first write.
pub fn commit_send(tx: &Transaction, command: &SendCommand) -> Result<SendOutcome, StoreError> {
    // 12
    if command.principal == SendAs::Owner && !agent_exists(tx, &command.sender)? {
        return Ok(SendOutcome::Rejected(SendRejection::UnknownSender));
    }
    // 13
    let room = match resolve_room(tx, command)? {
        Ok(room) => room,
        Err(rejection) => return Ok(SendOutcome::Rejected(rejection)),
    };
    // 14
    let reply_to = match &command.destination {
        Destination::ReplyTo(message_id) => Some(message_id),
        Destination::Room(_) | Destination::Direct(_) => None,
    };
    let request_hash = canonical_request_hash(
        &room.to_string(),
        &command.body,
        reply_to.map(MessageId::as_str),
        command.wake,
        &command.mentions,
    );
    if let Some(replayed) = replay(tx, &command.sender, &command.client_key, &request_hash)? {
        return Ok(replayed);
    }
    // 15
    let target = match authorize(tx, command, &room)? {
        Ok(target) => target,
        Err(rejection) => return Ok(SendOutcome::Rejected(rejection)),
    };
    // 16
    if target.row.as_ref().is_some_and(RoomRow::is_archived) {
        return Ok(SendOutcome::Rejected(SendRejection::RoomArchived));
    }
    // 17
    let recipients = match recipients(tx, command, &target.members)? {
        Ok(recipients) => recipients,
        Err(rejection) => return Ok(SendOutcome::Rejected(rejection)),
    };
    // 18
    let draft = Draft {
        command,
        room,
        room_exists: target.row.is_some(),
        reply_to,
        request_hash: &request_hash,
    };
    commit_new(tx, draft, &recipients)
}

/// Step 13: the room a destination names. `reply_to` takes the room of
/// the original, which never changes (messages are never deleted).
fn resolve_room(
    conn: &Connection,
    command: &SendCommand,
) -> Result<Result<RoomId, SendRejection>, StoreError> {
    let room = match &command.destination {
        Destination::Room(room) => room.clone(),
        Destination::ReplyTo(message_id) => match find_room_of(conn, message_id)? {
            Some(room) => room,
            None => return Ok(Err(SendRejection::ReplyToNotFound)),
        },
        Destination::Direct(recipient) => {
            match RoomId::dm(command.sender.clone(), recipient.clone()) {
                Some(room) => room,
                None => return Ok(Err(SendRejection::SelfDm)),
            }
        }
    };
    Ok(Ok(room))
}

/// The resolved room after step 15: its row (`None` for a DM no one has
/// written to yet) and its joined members.
struct Target {
    row: Option<RoomRow>,
    members: Vec<AgentId>,
}

/// Step 15: the room exists (or is a DM between registered agents) and
/// the sender may write to it. The owner needs no membership of its sender.
fn authorize(
    conn: &Connection,
    command: &SendCommand,
    room: &RoomId,
) -> Result<Result<Target, SendRejection>, StoreError> {
    let is_agent = command.principal == SendAs::Agent;
    if let Destination::Direct(recipient) = &command.destination
        && !agent_exists(conn, recipient)?
    {
        return Ok(Err(SendRejection::UnknownRecipient));
    }
    if let Some((low, high)) = room.dm_members() {
        let sender_named = command.sender == *low || command.sender == *high;
        if is_agent && !sender_named {
            return Ok(Err(SendRejection::NotRoomMember));
        }
        for member in [low, high] {
            if !agent_exists(conn, member)? {
                return Ok(Err(SendRejection::UnknownRoom));
            }
        }
        let row = room_queries::find(conn, room)?;
        return Ok(Ok(Target {
            row,
            members: vec![low.clone(), high.clone()],
        }));
    }

    let access = room_queries::access(conn, room, &command.sender)?;
    let Some(row) = access.room.clone() else {
        let rejection = if is_agent {
            SendRejection::NotRoomMember
        } else {
            SendRejection::UnknownRoom
        };
        return Ok(Err(rejection));
    };
    if is_agent && !access.can_write() {
        return Ok(Err(SendRejection::NotRoomMember));
    }
    Ok(Ok(Target {
        members: room_queries::joined_members(conn, room)?,
        row: Some(row),
    }))
}

/// One inbox recipient and whether it is woken.
struct Recipient {
    agent_id: AgentId,
    woken: bool,
}

/// Step 17 and the sets R and W (`IF-WIRE-010`): R is every joined member
/// plus mentioned registered `post_as` identities outside the room; W is R
/// minus the sender, by `wake`, a mention or `wake_policy = always`. The
/// sender's own row wakes it only under its own `wake_policy = always`.
/// R is in ASCII order.
fn recipients(
    conn: &Connection,
    command: &SendCommand,
    members: &[AgentId],
) -> Result<Result<Vec<Recipient>, SendRejection>, StoreError> {
    let mut receivers: Vec<AgentId> = members.to_vec();
    for mentioned in &command.mentions {
        if members.contains(mentioned) {
            continue;
        }
        let allowlisted = command.post_as.contains(mentioned) && agent_exists(conn, mentioned)?;
        if !allowlisted {
            return Ok(Err(SendRejection::MentionNotMember(mentioned.clone())));
        }
        receivers.push(mentioned.clone());
    }
    receivers.sort_unstable();

    let mut recipients = Vec::with_capacity(receivers.len());
    for agent_id in receivers {
        let always = wake_policy(conn, &agent_id)? == WakePolicy::Always;
        let woken = if agent_id == command.sender {
            always
        } else {
            command.wake || command.mentions.contains(&agent_id) || always
        };
        recipients.push(Recipient { agent_id, woken });
    }
    Ok(Ok(recipients))
}

fn wake_policy(conn: &Connection, agent_id: &AgentId) -> Result<WakePolicy, StoreError> {
    let stored: String = conn.query_row(
        "SELECT wake_policy FROM agents WHERE agent_id = ?1",
        params![agent_id.as_str()],
        |row| row.get(0),
    )?;
    stored
        .parse()
        .map_err(|_| StoreError::Invariant(format!("agents.wake_policy '{stored}' is unknown")))
}

/// The idempotency lookup. `Some` ends the request: the original message
/// for the same canonical request, a conflict for a different one.
fn replay(
    conn: &Connection,
    sender: &AgentId,
    client_key: &str,
    request_hash: &str,
) -> Result<Option<SendOutcome>, StoreError> {
    let Some((stored_hash, message_id)) = find_idempotency(conn, sender, client_key)? else {
        return Ok(None);
    };
    if stored_hash != request_hash {
        return Ok(Some(SendOutcome::Rejected(
            SendRejection::IdempotencyConflict,
        )));
    }
    let original = find_message(conn, &message_id)?.ok_or_else(|| {
        StoreError::Invariant("idempotency row points at a missing message".to_owned())
    })?;
    Ok(Some(SendOutcome::Deduplicated(original)))
}

/// A new message whose checks have all passed.
struct Draft<'a> {
    command: &'a SendCommand,
    room: RoomId,
    room_exists: bool,
    reply_to: Option<&'a MessageId>,
    request_hash: &'a str,
}

/// Step 18's writes (a)–(f) in the caller's transaction: a new DM's room
/// and memberships, the message, its mentions, one inbox row per
/// recipient, the wake transition for W and the idempotency key.
fn commit_new(
    tx: &Transaction,
    draft: Draft<'_>,
    recipients: &[Recipient],
) -> Result<SendOutcome, StoreError> {
    let command = draft.command;
    if !draft.room_exists {
        room_queries::insert_dm(tx, &draft.room, &command.sender, &command.created_at)?;
    }
    let event_seq = next_event_seq(tx)?;
    let message = Message {
        message_id: command.message_id.clone(),
        event_seq,
        room_kind: draft.room.kind(),
        room_id: draft.room,
        sender_id: command.sender.clone(),
        body: command.body.clone(),
        reply_to: draft.reply_to.cloned(),
        wake: command.wake,
        mentions: command.mentions.clone(),
        created_at: command.created_at.clone(),
    };
    insert_message(tx, &message)?;
    for mentioned in &message.mentions {
        tx.execute(
            "INSERT INTO message_mentions (message_id, agent_id) VALUES (?1, ?2)",
            params![message.message_id.as_str(), mentioned.as_str()],
        )?;
    }
    for recipient in recipients {
        append_inbox_row(tx, &recipient.agent_id, &message)?;
    }
    let now = SystemTime::now();
    let mut woken = 0;
    for recipient in recipients.iter().filter(|recipient| recipient.woken) {
        delivery_queries::record_new_inbox_row(tx, &recipient.agent_id, now)?;
        woken += 1;
    }
    tx.execute(
        "INSERT INTO idempotency (sender_id, client_key, request_hash, message_id)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            command.sender.as_str(),
            command.client_key,
            draft.request_hash,
            message.message_id.as_str()
        ],
    )?;

    Ok(SendOutcome::Created {
        message,
        recipients: recipients.len(),
        woken,
    })
}

/// One page of a room's history, fetching `limit + 1` rows to learn
/// `has_more`. Permission is the caller's ([`room_queries::read_access`]).
pub fn history(conn: &Connection, query: &HistoryQuery) -> Result<HistorySlice, StoreError> {
    let (comparison, order, bound) = match query.direction {
        HistoryDirection::Before(bound) => ("<", "DESC", bound.map(Seq::get)),
        HistoryDirection::After(bound) => (">", "ASC", Some(bound.get())),
    };
    let sql = format!(
        "SELECT {MESSAGE_COLUMNS} FROM messages m
         WHERE m.room_id = ?1
           AND (?2 IS NULL OR m.event_seq {comparison} ?2)
           AND (?3 IS NULL OR instr(m.body, ?3) > 0)
         ORDER BY m.event_seq {order}
         LIMIT ?4"
    );
    let fetch = i64::from(query.limit) + 1;
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(
        params![query.room.to_string(), bound, query.contains, fetch],
        |row| StoredMessage::from_row(row, 0),
    )?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?.into_message()?);
    }
    let has_more = items.len() > query.limit as usize;
    items.truncate(query.limit as usize);
    Ok(HistorySlice { items, has_more })
}

pub fn find_message(
    conn: &Connection,
    message_id: &MessageId,
) -> Result<Option<Message>, StoreError> {
    let stored = conn
        .query_row(
            &format!("SELECT {MESSAGE_COLUMNS} FROM messages m WHERE m.message_id = ?1"),
            params![message_id.as_str()],
            |row| StoredMessage::from_row(row, 0),
        )
        .optional()?;
    stored.map(StoredMessage::into_message).transpose()
}

fn find_idempotency(
    conn: &Connection,
    sender: &AgentId,
    client_key: &str,
) -> Result<Option<(String, MessageId)>, StoreError> {
    let found: Option<(String, String)> = conn
        .query_row(
            "SELECT request_hash, message_id FROM idempotency
             WHERE sender_id = ?1 AND client_key = ?2",
            params![sender.as_str(), client_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((hash, message_id)) = found else {
        return Ok(None);
    };
    Ok(Some((hash, stored_message_id(message_id)?)))
}

fn find_room_of(conn: &Connection, message_id: &MessageId) -> Result<Option<RoomId>, StoreError> {
    let room: Option<String> = conn
        .query_row(
            "SELECT room_id FROM messages WHERE message_id = ?1",
            params![message_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    room.map(|room| stored_room_id(&room)).transpose()
}

/// Issues the next global `event_seq` inside the caller's transaction, so a
/// rollback leaves no hole.
fn next_event_seq(tx: &Transaction) -> Result<Seq, StoreError> {
    let value: i64 = tx.query_row(
        "UPDATE sequences SET value = value + 1 WHERE name = 'event_seq' RETURNING value",
        [],
        |row| row.get(0),
    )?;
    stored_seq(value, "sequences.event_seq")
}

fn insert_message(tx: &Transaction, message: &Message) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO messages (message_id, event_seq, room_id, sender_id, body, reply_to, wake, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            message.message_id.as_str(),
            message.event_seq.get(),
            message.room_id.to_string(),
            message.sender_id.as_str(),
            message.body,
            message.reply_to.as_ref().map(MessageId::as_str),
            message.wake,
            message.created_at,
        ],
    )?;
    Ok(())
}

/// Issues the agent's next `inbox_seq` (its new `inbox_head`) and writes
/// the message event row in the caller's transaction.
fn append_inbox_row(
    tx: &Transaction,
    agent_id: &AgentId,
    message: &Message,
) -> Result<(), StoreError> {
    let inbox_seq: i64 = tx.query_row(
        "UPDATE agents SET inbox_head = inbox_head + 1 WHERE agent_id = ?1 RETURNING inbox_head",
        params![agent_id.as_str()],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO inbox_rows (agent_id, inbox_seq, message_id, kind, wake)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            agent_id.as_str(),
            inbox_seq,
            message.message_id.as_str(),
            MESSAGE_EVENT,
            message.wake
        ],
    )?;
    Ok(())
}

/// A [`MESSAGE_COLUMNS`] row before its text columns are validated.
pub(crate) struct StoredMessage {
    message_id: String,
    event_seq: i64,
    room_id: String,
    sender_id: String,
    body: String,
    reply_to: Option<String>,
    wake: bool,
    created_at: String,
    mentions: Option<String>,
}

impl StoredMessage {
    /// Reads the [`MESSAGE_COLUMNS`] starting at column `first`.
    pub(crate) fn from_row(row: &Row<'_>, first: usize) -> rusqlite::Result<Self> {
        Ok(Self {
            message_id: row.get(first)?,
            event_seq: row.get(first + 1)?,
            room_id: row.get(first + 2)?,
            sender_id: row.get(first + 3)?,
            body: row.get(first + 4)?,
            reply_to: row.get(first + 5)?,
            wake: row.get(first + 6)?,
            created_at: row.get(first + 7)?,
            mentions: row.get(first + 8)?,
        })
    }

    pub(crate) fn into_message(self) -> Result<Message, StoreError> {
        let room_id = stored_room_id(&self.room_id)?;
        let mut mentions = self
            .mentions
            .as_deref()
            .map(|joined| joined.split(',').map(|id| stored_agent_id(id.to_owned())))
            .into_iter()
            .flatten()
            .collect::<Result<Vec<AgentId>, StoreError>>()?;
        mentions.sort_unstable();
        Ok(Message {
            message_id: stored_message_id(self.message_id)?,
            event_seq: stored_seq(self.event_seq, "messages.event_seq")?,
            room_kind: room_id.kind(),
            room_id,
            sender_id: stored_agent_id(self.sender_id)?,
            body: self.body,
            reply_to: self.reply_to.map(stored_message_id).transpose()?,
            wake: self.wake,
            mentions,
            created_at: self.created_at,
        })
    }
}

pub(crate) fn stored_message_id(raw: String) -> Result<MessageId, StoreError> {
    MessageId::try_from(raw)
        .map_err(|err| StoreError::Invariant(format!("stored message_id is invalid: {err}")))
}

pub(crate) fn stored_seq(value: i64, column: &str) -> Result<Seq, StoreError> {
    Seq::new(value).map_err(|_| StoreError::Invariant(format!("{column} is negative")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agents(ids: &[&str]) -> Vec<AgentId> {
        ids.iter().map(|id| id.parse().unwrap()).collect()
    }

    #[test]
    fn hash_matches_the_canonical_byte_layout() {
        // Reference values from `printf '<bytes>' | shasum -a 256`:
        // `all\0hello\0\01\0` and `dm:a:b\0hi\0msg-01J00000000000000000000000\00\0b,human`.
        assert_eq!(
            canonical_request_hash("all", "hello", None, true, &[]),
            "3b0f8a785c40039a8395fd65813b608e1ad2f4e86a7b86ebbddcbe59214362c7"
        );
        assert_eq!(
            canonical_request_hash(
                "dm:a:b",
                "hi",
                Some("msg-01J00000000000000000000000"),
                false,
                &agents(&["human", "b"])
            ),
            "7c9212bdc6e013fc72ed20146ff54c553eb7d5a0022c5423fb5952ed4731fb03"
        );
    }

    #[test]
    fn hash_separates_fields_and_treats_mentions_as_a_set() {
        assert_ne!(
            canonical_request_hash("all", "ab", None, true, &[]),
            canonical_request_hash("all", "a", Some("b"), true, &[])
        );
        assert_eq!(
            canonical_request_hash("all", "x", None, true, &[]),
            canonical_request_hash("all", "x", Some(""), true, &[])
        );
        assert_ne!(
            canonical_request_hash("all", "x ", None, true, &[]),
            canonical_request_hash("all", "x", None, true, &[]),
            "no trimming"
        );
        assert_ne!(
            canonical_request_hash("all", "x", None, true, &[]),
            canonical_request_hash("all", "x", None, false, &[])
        );
        assert_eq!(
            canonical_request_hash("all", "x", None, true, &agents(&["b", "a"])),
            canonical_request_hash("all", "x", None, true, &agents(&["a", "b"]))
        );
        assert_ne!(
            canonical_request_hash("all", "x", None, true, &agents(&["a"])),
            canonical_request_hash("all", "x", None, true, &[])
        );
    }
}
