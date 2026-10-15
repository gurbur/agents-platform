//! Message send (durable commit with idempotency and fanout) and history
//! reads (`IF-DB-001` tables `messages`, `inbox_rows`, `idempotency`,
//! `sequences`, `memberships`, `agents`).

use platform_wire::{AgentId, Message, MessageId, Recipient, RoomId, Seq};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use sha2::{Digest, Sha256};

use super::agents::{ALL_ROOM, agent_exists, stored_agent_id};
use crate::store::StoreError;

const MESSAGE_COLUMNS: &str =
    "message_id, event_seq, room_id, sender_id, body, reply_to, created_at";

/// A validated send request. `message_id` and `created_at` are used only if
/// the request is new.
pub struct SendCommand {
    pub sender: AgentId,
    pub client_key: String,
    pub to: Recipient,
    pub body: String,
    pub reply_to: Option<MessageId>,
    pub message_id: MessageId,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    /// Committed now. `recipients` is the number of inbox rows written.
    Created {
        message: Message,
        recipients: usize,
    },
    /// The same canonical request was already committed under this key.
    Deduplicated(Message),
    Rejected(SendRejection),
}

/// Reasons a send commits nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendRejection {
    /// The key is bound to a different `(to, body, reply_to)`.
    IdempotencyConflict,
    NotAllMember,
    SelfDm,
    UnknownRecipient,
    ReplyToNotFound,
    ReplyToRoomMismatch,
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

/// SHA-256 hex of `to 0x00 body 0x00 reply_to` with an absent `reply_to` as
/// the empty string, without trimming or normalisation (`IF-DB-001`).
pub fn canonical_request_hash(to: &str, body: &str, reply_to: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(to.as_bytes());
    hasher.update([0u8]);
    hasher.update(body.as_bytes());
    hasher.update([0u8]);
    hasher.update(reply_to.unwrap_or_default().as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The `POST /v1/messages` transaction (S2 "구현 방안" 1): idempotency
/// lookup first; only a new key is checked against membership, recipient and
/// `reply_to`, then the message, its inbox rows and the key are written.
/// Every rejection happens before the first write.
pub fn commit_send(tx: &Transaction, command: &SendCommand) -> Result<SendOutcome, StoreError> {
    let to_text = command.to.to_string();
    let request_hash = canonical_request_hash(
        &to_text,
        &command.body,
        command.reply_to.as_ref().map(MessageId::as_str),
    );

    if let Some((stored_hash, message_id)) =
        find_idempotency(tx, &command.sender, &command.client_key)?
    {
        if stored_hash != request_hash {
            return Ok(SendOutcome::Rejected(SendRejection::IdempotencyConflict));
        }
        let original = find_message(tx, &message_id)?.ok_or_else(|| {
            StoreError::Invariant("idempotency row points at a missing message".to_owned())
        })?;
        return Ok(SendOutcome::Deduplicated(original));
    }

    let (room, recipients) = match resolve_recipients(tx, &command.sender, &command.to)? {
        Ok(resolved) => resolved,
        Err(rejection) => return Ok(SendOutcome::Rejected(rejection)),
    };
    if let Some(reply_to) = &command.reply_to {
        match find_room_of(tx, reply_to)? {
            None => return Ok(SendOutcome::Rejected(SendRejection::ReplyToNotFound)),
            Some(reply_room) if reply_room != room.to_string() => {
                return Ok(SendOutcome::Rejected(SendRejection::ReplyToRoomMismatch));
            }
            Some(_) => {}
        }
    }

    let event_seq = next_event_seq(tx)?;
    let message = Message {
        message_id: command.message_id.clone(),
        event_seq,
        room_id: room,
        sender_id: command.sender.clone(),
        body: command.body.clone(),
        reply_to: command.reply_to.clone(),
        created_at: command.created_at.clone(),
    };
    insert_message(tx, &message)?;
    for recipient in &recipients {
        append_inbox_row(tx, recipient, &message.message_id)?;
    }
    tx.execute(
        "INSERT INTO idempotency (sender_id, client_key, request_hash, message_id)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            command.sender.as_str(),
            command.client_key,
            request_hash,
            message.message_id.as_str()
        ],
    )?;

    Ok(SendOutcome::Created {
        message,
        recipients: recipients.len(),
    })
}

/// One page of a room's history, fetching `limit + 1` rows to learn
/// `has_more`.
pub fn history(conn: &Connection, query: &HistoryQuery) -> Result<HistorySlice, StoreError> {
    let (comparison, order, bound) = match query.direction {
        HistoryDirection::Before(bound) => ("<", "DESC", bound.map(Seq::get)),
        HistoryDirection::After(bound) => (">", "ASC", Some(bound.get())),
    };
    let sql = format!(
        "SELECT {MESSAGE_COLUMNS} FROM messages
         WHERE room_id = ?1
           AND (?2 IS NULL OR event_seq {comparison} ?2)
           AND (?3 IS NULL OR instr(body, ?3) > 0)
         ORDER BY event_seq {order}
         LIMIT ?4"
    );
    let fetch = i64::from(query.limit) + 1;
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(
        params![query.room.to_string(), bound, query.contains, fetch],
        StoredMessage::from_row,
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
            &format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE message_id = ?1"),
            params![message_id.as_str()],
            StoredMessage::from_row,
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

/// The room and inbox recipients of a new message: every joined `all`
/// member (the sender included), or the one DM recipient.
fn resolve_recipients(
    conn: &Connection,
    sender: &AgentId,
    to: &Recipient,
) -> Result<Result<(RoomId, Vec<AgentId>), SendRejection>, StoreError> {
    match to {
        Recipient::All => {
            let members = joined_all_members(conn)?;
            if !members.contains(sender) {
                return Ok(Err(SendRejection::NotAllMember));
            }
            Ok(Ok((RoomId::All, members)))
        }
        Recipient::Agent(recipient) => {
            let Some(room) = RoomId::dm(sender.clone(), recipient.clone()) else {
                return Ok(Err(SendRejection::SelfDm));
            };
            if !agent_exists(conn, recipient)? {
                return Ok(Err(SendRejection::UnknownRecipient));
            }
            Ok(Ok((room, vec![recipient.clone()])))
        }
    }
}

fn joined_all_members(conn: &Connection) -> Result<Vec<AgentId>, StoreError> {
    let mut statement = conn.prepare(
        "SELECT agent_id FROM memberships WHERE room_id = ?1 AND joined = 1 ORDER BY agent_id",
    )?;
    let rows = statement.query_map(params![ALL_ROOM], |row| row.get::<_, String>(0))?;
    let mut members = Vec::new();
    for row in rows {
        members.push(stored_agent_id(row?)?);
    }
    Ok(members)
}

fn find_room_of(conn: &Connection, message_id: &MessageId) -> Result<Option<String>, StoreError> {
    let room = conn
        .query_row(
            "SELECT room_id FROM messages WHERE message_id = ?1",
            params![message_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(room)
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
        "INSERT INTO messages (message_id, event_seq, room_id, sender_id, body, reply_to, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            message.message_id.as_str(),
            message.event_seq.get(),
            message.room_id.to_string(),
            message.sender_id.as_str(),
            message.body,
            message.reply_to.as_ref().map(MessageId::as_str),
            message.created_at,
        ],
    )?;
    Ok(())
}

/// Issues the agent's next `inbox_seq` (its new `inbox_head`) and writes the
/// inbox row in the caller's transaction.
fn append_inbox_row(
    tx: &Transaction,
    agent_id: &AgentId,
    message_id: &MessageId,
) -> Result<(), StoreError> {
    let inbox_seq: i64 = tx.query_row(
        "UPDATE agents SET inbox_head = inbox_head + 1 WHERE agent_id = ?1 RETURNING inbox_head",
        params![agent_id.as_str()],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO inbox_rows (agent_id, inbox_seq, message_id) VALUES (?1, ?2, ?3)",
        params![agent_id.as_str(), inbox_seq, message_id.as_str()],
    )?;
    Ok(())
}

/// A `messages` row before its text columns are validated.
struct StoredMessage {
    message_id: String,
    event_seq: i64,
    room_id: String,
    sender_id: String,
    body: String,
    reply_to: Option<String>,
    created_at: String,
}

impl StoredMessage {
    /// Reads the columns in [`MESSAGE_COLUMNS`] order.
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            message_id: row.get(0)?,
            event_seq: row.get(1)?,
            room_id: row.get(2)?,
            sender_id: row.get(3)?,
            body: row.get(4)?,
            reply_to: row.get(5)?,
            created_at: row.get(6)?,
        })
    }

    fn into_message(self) -> Result<Message, StoreError> {
        let room_id = self
            .room_id
            .parse::<RoomId>()
            .map_err(|err| StoreError::Invariant(format!("stored room_id is invalid: {err}")))?;
        Ok(Message {
            message_id: stored_message_id(self.message_id)?,
            event_seq: stored_seq(self.event_seq, "messages.event_seq")?,
            room_id,
            sender_id: stored_agent_id(self.sender_id)?,
            body: self.body,
            reply_to: self.reply_to.map(stored_message_id).transpose()?,
            created_at: self.created_at,
        })
    }
}

fn stored_message_id(raw: String) -> Result<MessageId, StoreError> {
    MessageId::try_from(raw)
        .map_err(|err| StoreError::Invariant(format!("stored message_id is invalid: {err}")))
}

fn stored_seq(value: i64, column: &str) -> Result<Seq, StoreError> {
    Seq::new(value).map_err(|_| StoreError::Invariant(format!("{column} is negative")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_matches_the_canonical_byte_layout() {
        // Reference values from `printf '<bytes>' | shasum -a 256`.
        assert_eq!(
            canonical_request_hash("all", "hello", None),
            "c1e107d755312e12202a55712d4404d71c98ec4144b922056f3e9a315bf95ddb"
        );
        assert_eq!(
            canonical_request_hash("agent-b", "hi", Some("msg-01J00000000000000000000000")),
            "818a46aa7204d7c23e9976131dd69c5e70308ccf49309ba7fd280c8cde4d3cd6"
        );
    }

    #[test]
    fn hash_separates_fields_and_treats_null_reply_as_empty() {
        assert_ne!(
            canonical_request_hash("all", "ab", None),
            canonical_request_hash("all", "a", Some("b"))
        );
        assert_eq!(
            canonical_request_hash("all", "x", None),
            canonical_request_hash("all", "x", Some(""))
        );
        assert_ne!(
            canonical_request_hash("all", "x ", None),
            canonical_request_hash("all", "x", None),
            "no trimming"
        );
    }
}
