//! The one send (`IF-WIRE-010` steps 12–18 with the S15 additions:
//! durable commit with idempotency, room resolution, permission, the thread
//! a reply lands in, corrections, participant registration, inbox fanout
//! and wake set) and history reads (`IF-DB-001` tables `messages`,
//! `message_mentions`, `thread_participants`, `inbox_rows`, `idempotency`,
//! `sequences`, `rooms`, `memberships`).

use std::collections::BTreeSet;
use std::time::SystemTime;

use platform_wire::{AgentId, JoinedVia, Message, MessageId, RoomId, Seq, WakePolicy};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use sha2::{Digest, Sha256};

use super::agents::{agent_exists, stored_agent_id};
use super::delivery as delivery_queries;
use super::inbox::MESSAGE_EVENT;
use super::reactions as reaction_queries;
use super::rooms::{self as room_queries, RoomRow, stored_room_id};
use super::threads as thread_queries;
use crate::store::StoreError;

/// The columns [`StoredMessage::from_row`] reads, for a `messages` table
/// aliased `m`. Mentions come as one comma-joined text (an `agent_id` holds
/// no comma) and are sorted on reading.
pub(crate) const MESSAGE_COLUMNS: &str = "m.message_id, m.event_seq, m.room_id, m.sender_id, \
     m.body, m.reply_to, m.thread_id, m.corrects, m.wake, m.created_at, \
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
    /// The room of the message replied to, inside its thread.
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
    /// The sender's own message this one corrects (`IF-WIRE-015`).
    pub corrects: Option<MessageId>,
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
    /// `corrects` names no message (17a).
    CorrectsNotFound,
    /// `corrects` names a message in another room (17b).
    CorrectsRoomMismatch,
    /// `corrects` names another sender's message (17c).
    CorrectsNotOwn,
}

/// Which messages of a room a history page holds (`IF-WIRE-005` S15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadFilter {
    /// Every message (the default).
    All,
    /// Messages outside any thread (`thread_id IS NULL`, roots included).
    TopLevel,
    /// The root and the replies of one thread.
    Thread(MessageId),
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
    pub filter: ThreadFilter,
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
/// `,` (none is empty). No trimming or normalisation. S15 appends
/// `0x00 corrects` only when `corrects` is given, so every hash without it
/// is the S13 one.
pub fn canonical_request_hash(
    room: &str,
    body: &str,
    reply_to: Option<&str>,
    wake: bool,
    mentions: &[AgentId],
    corrects: Option<&str>,
) -> String {
    let mut sorted: Vec<&str> = mentions.iter().map(AgentId::as_str).collect();
    sorted.sort_unstable();
    let joined = sorted.join(",");
    let mut parts = vec![
        room,
        body,
        reply_to.unwrap_or_default(),
        if wake { "1" } else { "0" },
        &joined,
    ];
    parts.extend(corrects);
    let mut hasher = Sha256::new();
    for (index, part) in parts.into_iter().enumerate() {
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

/// The `POST /v1/messages` transaction, `IF-WIRE-010` steps 12–18 in order
/// (S15: 13 also finds the thread, 17a–c check `corrects`). Every
/// rejection happens before the first write.
pub fn commit_send(tx: &Transaction, command: &SendCommand) -> Result<SendOutcome, StoreError> {
    // 12
    if command.principal == SendAs::Owner && !agent_exists(tx, &command.sender)? {
        return Ok(SendOutcome::Rejected(SendRejection::UnknownSender));
    }
    // 13
    let place = match resolve_place(tx, command)? {
        Ok(place) => place,
        Err(rejection) => return Ok(SendOutcome::Rejected(rejection)),
    };
    // 14
    let reply_to = match &command.destination {
        Destination::ReplyTo(message_id) => Some(message_id),
        Destination::Room(_) | Destination::Direct(_) => None,
    };
    let request_hash = canonical_request_hash(
        &place.room.to_string(),
        &command.body,
        reply_to.map(MessageId::as_str),
        command.wake,
        &command.mentions,
        command.corrects.as_ref().map(MessageId::as_str),
    );
    if let Some(replayed) = replay(tx, &command.sender, &command.client_key, &request_hash)? {
        return Ok(replayed);
    }
    // 15
    let target = match authorize(tx, command, &place.room)? {
        Ok(target) => target,
        Err(rejection) => return Ok(SendOutcome::Rejected(rejection)),
    };
    // 16
    if target.row.as_ref().is_some_and(RoomRow::is_archived) {
        return Ok(SendOutcome::Rejected(SendRejection::RoomArchived));
    }
    // 17
    let outsiders = match mentioned_outsiders(tx, command, &target.members)? {
        Ok(outsiders) => outsiders,
        Err(rejection) => return Ok(SendOutcome::Rejected(rejection)),
    };
    // 17a–c
    if let Some(corrects) = &command.corrects
        && let Err(rejection) = check_corrects(tx, command, corrects, &place.room)?
    {
        return Ok(SendOutcome::Rejected(rejection));
    }
    // 18
    let draft = Draft {
        command,
        place,
        target,
        outsiders,
        reply_to,
        request_hash: &request_hash,
    };
    commit_new(tx, draft)
}

/// Where a new message goes: its room and, for a reply, the root of the
/// thread it lands in.
struct Place {
    room: RoomId,
    thread: Option<MessageId>,
}

/// Step 13: the room a destination names. `reply_to` takes the room of
/// the original, which never changes (messages are never deleted), and its
/// thread: the original's own thread, or the original as the root.
fn resolve_place(
    conn: &Connection,
    command: &SendCommand,
) -> Result<Result<Place, SendRejection>, StoreError> {
    let place = match &command.destination {
        Destination::Room(room) => Place {
            room: room.clone(),
            thread: None,
        },
        Destination::ReplyTo(message_id) => match find_reply_target(conn, message_id)? {
            Some(place) => place,
            None => return Ok(Err(SendRejection::ReplyToNotFound)),
        },
        Destination::Direct(recipient) => {
            match RoomId::dm(command.sender.clone(), recipient.clone()) {
                Some(room) => Place { room, thread: None },
                None => return Ok(Err(SendRejection::SelfDm)),
            }
        }
    };
    Ok(Ok(place))
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

/// Step 17: every mention is a joined member or a registered `post_as`
/// identity. Returns the mentioned identities outside the room, which the
/// S13 R of a message outside any thread adds.
fn mentioned_outsiders(
    conn: &Connection,
    command: &SendCommand,
    members: &[AgentId],
) -> Result<Result<Vec<AgentId>, SendRejection>, StoreError> {
    let mut outsiders = Vec::new();
    for mentioned in &command.mentions {
        if members.contains(mentioned) {
            continue;
        }
        let allowlisted = command.post_as.contains(mentioned) && agent_exists(conn, mentioned)?;
        if !allowlisted {
            return Ok(Err(SendRejection::MentionNotMember(mentioned.clone())));
        }
        outsiders.push(mentioned.clone());
    }
    Ok(Ok(outsiders))
}

/// Steps 17a–c (`IF-WIRE-015`): the corrected message exists, is in the
/// resolved room and was sent by this message's sender (for an owner post,
/// its `sender_id`).
fn check_corrects(
    conn: &Connection,
    command: &SendCommand,
    corrects: &MessageId,
    room: &RoomId,
) -> Result<Result<(), SendRejection>, StoreError> {
    let found: Option<(String, String)> = conn
        .query_row(
            "SELECT room_id, sender_id FROM messages WHERE message_id = ?1",
            params![corrects.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((corrected_room, corrected_sender)) = found else {
        return Ok(Err(SendRejection::CorrectsNotFound));
    };
    if corrected_room != room.to_string() {
        return Ok(Err(SendRejection::CorrectsRoomMismatch));
    }
    if corrected_sender != command.sender.as_str() {
        return Ok(Err(SendRejection::CorrectsNotOwn));
    }
    Ok(Ok(()))
}

/// One inbox recipient: `woke` is the sender's rule for it (stored on the
/// row and shown in the inbox item), `woken` the actual wake set W, which
/// adds the recipient's own `wake_policy = always`.
struct Recipient {
    agent_id: AgentId,
    woke: bool,
    woken: bool,
}

/// R and W of a message outside any thread (`IF-WIRE-010`): R is every
/// joined member plus the mentioned `post_as` identities outside the room.
/// `woke` is R minus the sender by `wake` or a mention.
fn room_recipients(
    conn: &Connection,
    command: &SendCommand,
    members: &[AgentId],
    outsiders: &[AgentId],
) -> Result<Vec<Recipient>, StoreError> {
    let receivers: BTreeSet<&AgentId> = members.iter().chain(outsiders).collect();
    let mut recipients = Vec::with_capacity(receivers.len());
    for agent_id in receivers {
        let woke =
            *agent_id != command.sender && (command.wake || command.mentions.contains(agent_id));
        recipients.push(recipient(conn, agent_id, woke)?);
    }
    Ok(recipients)
}

/// R and W of a thread message (`IF-WIRE-014`), read after the
/// participants were registered: R is every joined member plus every
/// participant; `woke` is R minus the sender that is an unmuted participant
/// of a waking message, or is mentioned whatever its mute says.
fn thread_recipients(
    conn: &Connection,
    command: &SendCommand,
    members: &[AgentId],
    root: &MessageId,
) -> Result<Vec<Recipient>, StoreError> {
    let participants = thread_queries::mutes(conn, root)?;
    let receivers: BTreeSet<&AgentId> = members
        .iter()
        .chain(participants.iter().map(|participant| &participant.agent_id))
        .collect();
    let mut recipients = Vec::with_capacity(receivers.len());
    for agent_id in receivers {
        let listening = participants
            .iter()
            .any(|participant| participant.agent_id == *agent_id && !participant.muted);
        let woke = *agent_id != command.sender
            && ((listening && command.wake) || command.mentions.contains(agent_id));
        recipients.push(recipient(conn, agent_id, woke)?);
    }
    Ok(recipients)
}

/// W is `woke` or the recipient's own `wake_policy = always`, so the
/// sender's own row wakes it only under that policy.
fn recipient(conn: &Connection, agent_id: &AgentId, woke: bool) -> Result<Recipient, StoreError> {
    let always = wake_policy(conn, agent_id)? == WakePolicy::Always;
    Ok(Recipient {
        agent_id: agent_id.clone(),
        woke,
        woken: woke || always,
    })
}

pub(crate) fn wake_policy(conn: &Connection, agent_id: &AgentId) -> Result<WakePolicy, StoreError> {
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
    place: Place,
    target: Target,
    /// Mentioned `post_as` identities outside the room (step 17).
    outsiders: Vec<AgentId>,
    reply_to: Option<&'a MessageId>,
    request_hash: &'a str,
}

/// Step 18's writes in the caller's transaction: (a) a new DM's room and
/// memberships, (b) the message, (c) its mentions, (c') the thread's
/// participants, (d) one inbox row per recipient, (e) the wake transition
/// for W and (f) the idempotency key. R and W of a thread message are
/// read after (c'), so the first reply wakes the root's author.
fn commit_new(tx: &Transaction, draft: Draft<'_>) -> Result<SendOutcome, StoreError> {
    let command = draft.command;
    if draft.target.row.is_none() {
        room_queries::insert_dm(tx, &draft.place.room, &command.sender, &command.created_at)?;
    }
    let opens_thread = match &draft.place.thread {
        Some(root) => !thread_queries::has_replies(tx, root)?,
        None => false,
    };
    let event_seq = next_event_seq(tx)?;
    let message = Message {
        message_id: command.message_id.clone(),
        event_seq,
        room_kind: draft.place.room.kind(),
        room_id: draft.place.room.clone(),
        sender_id: command.sender.clone(),
        body: command.body.clone(),
        reply_to: draft.reply_to.cloned(),
        thread_id: draft.place.thread.clone(),
        corrects: command.corrects.clone(),
        wake: command.wake,
        mentions: command.mentions.clone(),
        thread_summary: None,
        reactions: Vec::new(),
        created_at: command.created_at.clone(),
    };
    insert_message(tx, &message)?;
    for mentioned in &message.mentions {
        tx.execute(
            "INSERT INTO message_mentions (message_id, agent_id) VALUES (?1, ?2)",
            params![message.message_id.as_str(), mentioned.as_str()],
        )?;
    }
    let recipients = match &message.thread_id {
        Some(root) => {
            register_participants(tx, &draft, root, opens_thread)?;
            thread_recipients(tx, command, &draft.target.members, root)?
        }
        None => room_recipients(tx, command, &draft.target.members, &draft.outsiders)?,
    };
    for recipient in &recipients {
        append_inbox_row(tx, recipient, &message)?;
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

/// Write (c'), `IF-WIRE-014`: the root's author when this reply opens the
/// thread, the sender (`opener`, `poster`, or `barged` for an owner post by
/// an identity that is no joined member) and every mentioned agent. Rows
/// that exist stay as they are; unregistered agents get none.
fn register_participants(
    tx: &Transaction,
    draft: &Draft<'_>,
    root: &MessageId,
    opens_thread: bool,
) -> Result<(), StoreError> {
    let command = draft.command;
    let joined_at = command.created_at.as_str();
    if opens_thread {
        let root_sender: String = tx.query_row(
            "SELECT sender_id FROM messages WHERE message_id = ?1",
            params![root.as_str()],
            |row| row.get(0),
        )?;
        let root_sender = stored_agent_id(root_sender)?;
        thread_queries::join(tx, root, &root_sender, JoinedVia::Root, joined_at)?;
    }
    let barged =
        command.principal == SendAs::Owner && !draft.target.members.contains(&command.sender);
    let sender_via = match (barged, opens_thread) {
        (true, _) => JoinedVia::Barged,
        (false, true) => JoinedVia::Opener,
        (false, false) => JoinedVia::Poster,
    };
    thread_queries::join(tx, root, &command.sender, sender_via, joined_at)?;
    for mentioned in &command.mentions {
        thread_queries::join(tx, root, mentioned, JoinedVia::Mentioned, joined_at)?;
    }
    Ok(())
}

/// One page of a room's history, fetching `limit + 1` rows to learn
/// `has_more`, with the summary of every root on the page and every
/// message's reactions. Permission and the thread's existence are the
/// caller's ([`room_queries::history_access`]).
pub fn history(conn: &Connection, query: &HistoryQuery) -> Result<HistorySlice, StoreError> {
    let (comparison, order, bound) = match query.direction {
        HistoryDirection::Before(bound) => ("<", "DESC", bound.map(Seq::get)),
        HistoryDirection::After(bound) => (">", "ASC", Some(bound.get())),
    };
    let (top_level, thread) = match &query.filter {
        ThreadFilter::All => (false, None),
        ThreadFilter::TopLevel => (true, None),
        ThreadFilter::Thread(root) => (false, Some(root.as_str())),
    };
    let sql = format!(
        "SELECT {MESSAGE_COLUMNS} FROM messages m
         WHERE m.room_id = ?1
           AND (?2 IS NULL OR m.event_seq {comparison} ?2)
           AND (?3 IS NULL OR instr(m.body, ?3) > 0)
           AND (?5 = 0 OR m.thread_id IS NULL)
           AND (?6 IS NULL OR m.message_id = ?6 OR m.thread_id = ?6)
         ORDER BY m.event_seq {order}
         LIMIT ?4"
    );
    let fetch = i64::from(query.limit) + 1;
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(
        params![
            query.room.to_string(),
            bound,
            query.contains,
            fetch,
            top_level,
            thread
        ],
        |row| StoredMessage::from_row(row, 0),
    )?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?.into_message()?);
    }
    let has_more = items.len() > query.limit as usize;
    items.truncate(query.limit as usize);
    thread_queries::attach_summaries(conn, &mut items)?;
    reaction_queries::attach_summaries(conn, &mut items)?;
    Ok(HistorySlice { items, has_more })
}

/// A stored message without its `thread_summary` and `reactions` (the
/// send response's shape); [`thread_queries::attach_summaries`] and
/// [`reaction_queries::attach_summaries`] add them for reads.
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

/// The room of the message replied to and the root of the thread a reply
/// to it lands in: its own thread's root, or the message itself.
fn find_reply_target(
    conn: &Connection,
    message_id: &MessageId,
) -> Result<Option<Place>, StoreError> {
    let found: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT room_id, thread_id FROM messages WHERE message_id = ?1",
            params![message_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((room, thread)) = found else {
        return Ok(None);
    };
    let root = match thread {
        Some(root) => stored_message_id(root)?,
        None => message_id.clone(),
    };
    Ok(Some(Place {
        room: stored_room_id(&room)?,
        thread: Some(root),
    }))
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
        "INSERT INTO messages
           (message_id, event_seq, room_id, sender_id, body, reply_to, thread_id, corrects, wake, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            message.message_id.as_str(),
            message.event_seq.get(),
            message.room_id.to_string(),
            message.sender_id.as_str(),
            message.body,
            message.reply_to.as_ref().map(MessageId::as_str),
            message.thread_id.as_ref().map(MessageId::as_str),
            message.corrects.as_ref().map(MessageId::as_str),
            message.wake,
            message.created_at,
        ],
    )?;
    Ok(())
}

/// Issues the recipient's next `inbox_seq` (its new `inbox_head`) and
/// writes the message event row in the caller's transaction.
fn append_inbox_row(
    tx: &Transaction,
    recipient: &Recipient,
    message: &Message,
) -> Result<(), StoreError> {
    let agent_id = &recipient.agent_id;
    let inbox_seq: i64 = tx.query_row(
        "UPDATE agents SET inbox_head = inbox_head + 1 WHERE agent_id = ?1 RETURNING inbox_head",
        params![agent_id.as_str()],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO inbox_rows (agent_id, inbox_seq, message_id, kind, wake, woke)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            agent_id.as_str(),
            inbox_seq,
            message.message_id.as_str(),
            MESSAGE_EVENT,
            message.wake,
            recipient.woke
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
    thread_id: Option<String>,
    corrects: Option<String>,
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
            thread_id: row.get(first + 6)?,
            corrects: row.get(first + 7)?,
            wake: row.get(first + 8)?,
            created_at: row.get(first + 9)?,
            mentions: row.get(first + 10)?,
        })
    }

    /// The message with `thread_summary` unset and no `reactions`.
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
            thread_id: self.thread_id.map(stored_message_id).transpose()?,
            corrects: self.corrects.map(stored_message_id).transpose()?,
            wake: self.wake,
            mentions,
            thread_summary: None,
            reactions: Vec::new(),
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
            canonical_request_hash("all", "hello", None, true, &[], None),
            "3b0f8a785c40039a8395fd65813b608e1ad2f4e86a7b86ebbddcbe59214362c7"
        );
        assert_eq!(
            canonical_request_hash(
                "dm:a:b",
                "hi",
                Some("msg-01J00000000000000000000000"),
                false,
                &agents(&["human", "b"]),
                None
            ),
            "7c9212bdc6e013fc72ed20146ff54c553eb7d5a0022c5423fb5952ed4731fb03"
        );
    }

    #[test]
    fn corrects_is_appended_only_when_given() {
        // `printf 'all\0hello\0\0001\0\0msg-01J00000000000000000000000' | shasum -a 256`
        assert_eq!(
            canonical_request_hash(
                "all",
                "hello",
                None,
                true,
                &[],
                Some("msg-01J00000000000000000000000")
            ),
            "42b508a4b3757be96d0c3b307d0c41acc7d592994ab4b22c66753ed08a23cc9d"
        );
        assert_ne!(
            canonical_request_hash("all", "x", None, true, &[], Some("")),
            canonical_request_hash("all", "x", None, true, &[], None),
            "an empty corrects still adds a separator"
        );
    }

    #[test]
    fn hash_separates_fields_and_treats_mentions_as_a_set() {
        assert_ne!(
            canonical_request_hash("all", "ab", None, true, &[], None),
            canonical_request_hash("all", "a", Some("b"), true, &[], None)
        );
        assert_eq!(
            canonical_request_hash("all", "x", None, true, &[], None),
            canonical_request_hash("all", "x", Some(""), true, &[], None)
        );
        assert_ne!(
            canonical_request_hash("all", "x ", None, true, &[], None),
            canonical_request_hash("all", "x", None, true, &[], None),
            "no trimming"
        );
        assert_ne!(
            canonical_request_hash("all", "x", None, true, &[], None),
            canonical_request_hash("all", "x", None, false, &[], None)
        );
        assert_eq!(
            canonical_request_hash("all", "x", None, true, &agents(&["b", "a"]), None),
            canonical_request_hash("all", "x", None, true, &agents(&["a", "b"]), None)
        );
        assert_ne!(
            canonical_request_hash("all", "x", None, true, &agents(&["a"]), None),
            canonical_request_hash("all", "x", None, true, &[], None)
        );
    }
}
