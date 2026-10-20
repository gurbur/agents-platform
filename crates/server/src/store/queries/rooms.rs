//! Rooms and room permission (`IF-DB-001` v5 tables `rooms`,
//! `memberships`; `IF-WIRE-011`, ADR-0010).
//!
//! Permission is one `memberships` row whatever the room kind: a row reads
//! the room (its whole history), `joined = 1` writes to it and receives its
//! events. The owner reads every room. Topic room management is the owner's
//! and lives here too; deleting a topic membership row is the only
//! `memberships` DELETE outside the agent deletion cascade.

use platform_wire::{AgentId, OWNER_CREATOR, RoomId, RoomKind, RoomSummary};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use super::agents::{agent_exists, current_event_seq, stored_agent_id};
use super::messages::stored_seq;
use crate::store::StoreError;

/// `epoch` of a membership row a send, a registration or an invitation
/// creates.
const INITIAL_EPOCH: i64 = 1;

/// A `rooms` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomRow {
    pub room_id: RoomId,
    pub kind: RoomKind,
    pub title: Option<String>,
    pub created_by: String,
    pub created_at: String,
    pub archived_at: Option<String>,
}

impl RoomRow {
    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }
}

/// A membership row's `joined` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Joined(pub bool);

/// Everything a permission decision about one agent and one room needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomAccess {
    pub room: Option<RoomRow>,
    pub membership: Option<Joined>,
}

impl RoomAccess {
    /// A membership row, joined or not, reads the room.
    pub fn can_read(&self) -> bool {
        self.membership.is_some()
    }

    /// Writing and receiving need `joined = 1`.
    pub fn can_write(&self) -> bool {
        self.membership == Some(Joined(true))
    }
}

/// Who is looking at rooms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Viewer {
    Owner,
    Agent(AgentId),
}

/// Whether a history or single-message read may go ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadAccess {
    Allowed,
    /// The owner asked for a room that does not exist.
    UnknownRoom,
    /// An agent without a membership row, whether or not the room exists.
    Forbidden,
}

/// Why a topic room operation changed nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomRejection {
    UnknownRoom,
    NotTopic,
    RoomArchived,
    UnknownAgent,
    RoomIdTaken,
}

const ROOM_COLUMNS: &str = "room_id, kind, title, created_by, created_at, archived_at";

/// The room and `agent_id`'s membership of it.
pub fn access(
    conn: &Connection,
    room_id: &RoomId,
    agent_id: &AgentId,
) -> Result<RoomAccess, StoreError> {
    let room = find(conn, room_id)?;
    let membership = conn
        .query_row(
            "SELECT joined FROM memberships WHERE room_id = ?1 AND agent_id = ?2",
            params![room_id.to_string(), agent_id.as_str()],
            |row| row.get::<_, bool>(0),
        )
        .optional()?
        .map(Joined);
    Ok(RoomAccess { room, membership })
}

/// History permission (`IF-WIRE-005` S13): the owner needs the room to
/// exist, an agent needs a membership row.
pub fn read_access(
    conn: &Connection,
    viewer: &Viewer,
    room_id: &RoomId,
) -> Result<ReadAccess, StoreError> {
    let decision = match viewer {
        Viewer::Owner if find(conn, room_id)?.is_some() => ReadAccess::Allowed,
        Viewer::Owner => ReadAccess::UnknownRoom,
        Viewer::Agent(agent_id) if access(conn, room_id, agent_id)?.can_read() => {
            ReadAccess::Allowed
        }
        Viewer::Agent(_) => ReadAccess::Forbidden,
    };
    Ok(decision)
}

pub fn find(conn: &Connection, room_id: &RoomId) -> Result<Option<RoomRow>, StoreError> {
    let stored = conn
        .query_row(
            &format!("SELECT {ROOM_COLUMNS} FROM rooms WHERE room_id = ?1"),
            params![room_id.to_string()],
            StoredRoom::from_row,
        )
        .optional()?;
    stored.map(StoredRoom::into_row).transpose()
}

/// The agents with `joined = 1` in `room_id`, in ASCII order.
pub fn joined_members(conn: &Connection, room_id: &RoomId) -> Result<Vec<AgentId>, StoreError> {
    let mut statement = conn.prepare(
        "SELECT agent_id FROM memberships WHERE room_id = ?1 AND joined = 1 ORDER BY agent_id",
    )?;
    let rows = statement.query_map(params![room_id.to_string()], |row| row.get::<_, String>(0))?;
    let mut members = Vec::new();
    for row in rows {
        members.push(stored_agent_id(row?)?);
    }
    Ok(members)
}

/// Writes a DM room's row and both members' joined memberships, the first
/// send's job (`IF-WIRE-010` write (a)).
pub fn insert_dm(
    tx: &Transaction,
    room_id: &RoomId,
    created_by: &AgentId,
    created_at: &str,
) -> Result<(), StoreError> {
    let Some((low, high)) = room_id.dm_members() else {
        return Err(StoreError::Invariant(
            "insert_dm needs a DM room".to_owned(),
        ));
    };
    tx.execute(
        "INSERT INTO rooms (room_id, kind, title, created_by, created_at, archived_at)
         VALUES (?1, ?2, NULL, ?3, ?4, NULL)",
        params![
            room_id.to_string(),
            RoomKind::Dm.as_str(),
            created_by.as_str(),
            created_at
        ],
    )?;
    for member in [low, high] {
        insert_joined(tx, room_id, member)?;
    }
    Ok(())
}

/// A joined membership effective at the current `event_seq`.
fn insert_joined(tx: &Transaction, room_id: &RoomId, agent_id: &AgentId) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO memberships (room_id, agent_id, joined, epoch, effective_event_seq)
         VALUES (?1, ?2, 1, ?3, ?4)",
        params![
            room_id.to_string(),
            agent_id.as_str(),
            INITIAL_EPOCH,
            current_event_seq(tx)?
        ],
    )?;
    Ok(())
}

/// `GET /v1/rooms`: every room for the owner, the rooms with a membership
/// row for an agent. Rooms with messages come first by `last_event_seq`
/// descending, then the rest by `created_at` descending and `room_id`.
/// `all` is always listed; other rooms stop at `cap - 1`.
pub fn list(
    conn: &Connection,
    viewer: &Viewer,
    cap: usize,
) -> Result<Vec<RoomSummary>, StoreError> {
    let agent = match viewer {
        Viewer::Owner => None,
        Viewer::Agent(agent_id) => Some(agent_id.as_str()),
    };
    let sql = format!(
        "{SUMMARY_SELECT}
         WHERE ?1 IS NULL
            OR EXISTS (SELECT 1 FROM memberships x WHERE x.room_id = r.room_id AND x.agent_id = ?1)
         ORDER BY m.event_seq IS NULL, m.event_seq DESC, r.created_at DESC, r.room_id ASC"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params![agent], StoredSummary::from_row)?;
    let mut summaries = Vec::new();
    let mut others = 0;
    let other_limit = cap.saturating_sub(1);
    for row in rows {
        let stored = row?;
        let is_all = stored.room.room_id == platform_wire::messages::ALL_ROOM_ID;
        if !is_all {
            if others == other_limit {
                continue;
            }
            others += 1;
        }
        summaries.push(stored.into_summary(conn)?);
    }
    Ok(summaries)
}

/// One room's summary, `None` when it does not exist.
pub fn summary(conn: &Connection, room_id: &RoomId) -> Result<Option<RoomSummary>, StoreError> {
    let stored = conn
        .query_row(
            &format!("{SUMMARY_SELECT} WHERE r.room_id = ?1"),
            params![room_id.to_string()],
            StoredSummary::from_row,
        )
        .optional()?;
    stored.map(|stored| stored.into_summary(conn)).transpose()
}

/// `POST /v1/rooms`: a new topic room created by the owner, no members.
pub fn create_topic(
    tx: &Transaction,
    room_id: &RoomId,
    title: Option<&str>,
    now: &str,
) -> Result<Result<RoomSummary, RoomRejection>, StoreError> {
    if room_id.kind() != RoomKind::Topic {
        return Ok(Err(RoomRejection::NotTopic));
    }
    if find(tx, room_id)?.is_some() {
        return Ok(Err(RoomRejection::RoomIdTaken));
    }
    tx.execute(
        "INSERT INTO rooms (room_id, kind, title, created_by, created_at, archived_at)
         VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
        params![
            room_id.to_string(),
            RoomKind::Topic.as_str(),
            title,
            OWNER_CREATOR,
            now
        ],
    )?;
    require_summary(tx, room_id).map(Ok)
}

/// `PUT /v1/rooms/{room_id}/members/{agent_id}`. An existing row is left
/// as it is.
pub fn invite(
    tx: &Transaction,
    room_id: &RoomId,
    agent_id: &AgentId,
) -> Result<Result<RoomSummary, RoomRejection>, StoreError> {
    if let Err(rejection) = check_membership_change(tx, room_id, agent_id)? {
        return Ok(Err(rejection));
    }
    if access(tx, room_id, agent_id)?.membership.is_none() {
        insert_joined(tx, room_id, agent_id)?;
    }
    require_summary(tx, room_id).map(Ok)
}

/// `DELETE /v1/rooms/{room_id}/members/{agent_id}`. No row is a no-op.
pub fn remove(
    tx: &Transaction,
    room_id: &RoomId,
    agent_id: &AgentId,
) -> Result<Result<(), RoomRejection>, StoreError> {
    if let Err(rejection) = check_membership_change(tx, room_id, agent_id)? {
        return Ok(Err(rejection));
    }
    tx.execute(
        "DELETE FROM memberships WHERE room_id = ?1 AND agent_id = ?2",
        params![room_id.to_string(), agent_id.as_str()],
    )?;
    Ok(Ok(()))
}

/// `POST /v1/rooms/{room_id}/archive`. A second call keeps the first
/// `archived_at`.
pub fn archive(
    tx: &Transaction,
    room_id: &RoomId,
    now: &str,
) -> Result<Result<RoomSummary, RoomRejection>, StoreError> {
    let Some(room) = find(tx, room_id)? else {
        return Ok(Err(RoomRejection::UnknownRoom));
    };
    if room.kind != RoomKind::Topic {
        return Ok(Err(RoomRejection::NotTopic));
    }
    if !room.is_archived() {
        tx.execute(
            "UPDATE rooms SET archived_at = ?2 WHERE room_id = ?1",
            params![room_id.to_string(), now],
        )?;
    }
    require_summary(tx, room_id).map(Ok)
}

/// The store half of the `IF-WIRE-011` member order: the room exists, is a
/// topic, is not archived, and the agent is registered.
fn check_membership_change(
    conn: &Connection,
    room_id: &RoomId,
    agent_id: &AgentId,
) -> Result<Result<(), RoomRejection>, StoreError> {
    let Some(room) = find(conn, room_id)? else {
        return Ok(Err(RoomRejection::UnknownRoom));
    };
    if room.kind != RoomKind::Topic {
        return Ok(Err(RoomRejection::NotTopic));
    }
    if room.is_archived() {
        return Ok(Err(RoomRejection::RoomArchived));
    }
    if !agent_exists(conn, agent_id)? {
        return Ok(Err(RoomRejection::UnknownAgent));
    }
    Ok(Ok(()))
}

fn require_summary(conn: &Connection, room_id: &RoomId) -> Result<RoomSummary, StoreError> {
    summary(conn, room_id)?
        .ok_or_else(|| StoreError::Invariant("a room just written has no row".to_owned()))
}

/// A room with its message count and newest message. The grouped
/// subquery is covered by `messages_room_event(room_id, event_seq)`; the
/// newest message is read back through `UNIQUE(event_seq)`.
const SUMMARY_SELECT: &str = "
SELECT r.room_id, r.kind, r.title, r.created_by, r.created_at, r.archived_at,
       COALESCE(s.message_count, 0), m.event_seq, m.created_at, m.sender_id
FROM rooms r
LEFT JOIN (SELECT room_id, COUNT(*) AS message_count, MAX(event_seq) AS last_seq
           FROM messages GROUP BY room_id) s ON s.room_id = r.room_id
LEFT JOIN messages m ON m.event_seq = s.last_seq";

/// A `rooms` row before its text columns are validated.
struct StoredRoom {
    room_id: String,
    kind: String,
    title: Option<String>,
    created_by: String,
    created_at: String,
    archived_at: Option<String>,
}

impl StoredRoom {
    /// Reads [`ROOM_COLUMNS`] order.
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            room_id: row.get(0)?,
            kind: row.get(1)?,
            title: row.get(2)?,
            created_by: row.get(3)?,
            created_at: row.get(4)?,
            archived_at: row.get(5)?,
        })
    }

    fn into_row(self) -> Result<RoomRow, StoreError> {
        let room_id = stored_room_id(&self.room_id)?;
        let kind = RoomKind::parse(&self.kind).ok_or_else(|| {
            StoreError::Invariant(format!("rooms.kind '{}' is unknown", self.kind))
        })?;
        if kind != room_id.kind() {
            return Err(StoreError::Invariant(format!(
                "rooms.kind of '{}' does not match its id",
                self.room_id
            )));
        }
        Ok(RoomRow {
            room_id,
            kind,
            title: self.title,
            created_by: self.created_by,
            created_at: self.created_at,
            archived_at: self.archived_at,
        })
    }
}

/// A [`SUMMARY_SELECT`] row before validation.
struct StoredSummary {
    room: StoredRoom,
    message_count: i64,
    last_event_seq: Option<i64>,
    last_message_at: Option<String>,
    last_sender_id: Option<String>,
}

impl StoredSummary {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            room: StoredRoom::from_row(row)?,
            message_count: row.get(6)?,
            last_event_seq: row.get(7)?,
            last_message_at: row.get(8)?,
            last_sender_id: row.get(9)?,
        })
    }

    /// Members are the two ids of a DM's room id (a deleted agent's
    /// included) and the joined members of `all` and topic rooms.
    fn into_summary(self, conn: &Connection) -> Result<RoomSummary, StoreError> {
        let room = self.room.into_row()?;
        let members = match room.room_id.dm_members() {
            Some((low, high)) => vec![low.clone(), high.clone()],
            None => joined_members(conn, &room.room_id)?,
        };
        let message_count = u64::try_from(self.message_count)
            .map_err(|_| StoreError::Invariant("negative message count".to_owned()))?;
        Ok(RoomSummary {
            room_id: room.room_id,
            kind: room.kind,
            title: room.title,
            members,
            created_by: room.created_by,
            created_at: room.created_at,
            archived_at: room.archived_at,
            last_event_seq: self
                .last_event_seq
                .map(|seq| stored_seq(seq, "messages.event_seq"))
                .transpose()?,
            last_message_at: self.last_message_at,
            last_sender_id: self.last_sender_id.map(stored_agent_id).transpose()?,
            message_count,
        })
    }
}

pub(crate) fn stored_room_id(raw: &str) -> Result<RoomId, StoreError> {
    raw.parse()
        .map_err(|err| StoreError::Invariant(format!("stored room_id is invalid: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::migrations;

    fn database(agents: &[&str]) -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::apply(&mut conn).unwrap();
        for agent in agents {
            conn.execute(
                "INSERT INTO agents (agent_id, api_key_hash, receiver_secret, created_at)
                 VALUES (?1, ?1, 'secret', '2026-10-18T00:00:00.000Z')",
                params![agent],
            )
            .unwrap();
        }
        conn
    }

    fn insert(conn: &Connection, event_seq: i64, room_id: &str, sender_id: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO rooms (room_id, kind, created_by, created_at)
             VALUES (?1, 'dm', ?2, '2026-10-18T00:00:00.000Z')",
            params![room_id, sender_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (message_id, event_seq, room_id, sender_id, body, created_at)
             VALUES (?1, ?2, ?3, ?4, 'x', ?5)",
            params![
                format!("msg-01J{event_seq:023}"),
                event_seq,
                room_id,
                sender_id,
                format!("2026-10-18T00:00:{event_seq:02}.000Z")
            ],
        )
        .unwrap();
    }

    fn ids(rooms: &[RoomSummary]) -> Vec<String> {
        rooms.iter().map(|room| room.room_id.to_string()).collect()
    }

    #[test]
    fn the_cap_counts_all_and_keeps_the_newest_rooms() {
        let conn = database(&["a", "b", "c", "d"]);
        insert(&conn, 1, "dm:a:b", "a");
        insert(&conn, 2, "dm:a:c", "a");
        insert(&conn, 3, "all", "b");
        insert(&conn, 4, "dm:a:d", "d");
        insert(&conn, 5, "dm:b:c", "c");

        let capped = list(&conn, &Viewer::Owner, 3).unwrap();
        assert_eq!(ids(&capped), ["dm:b:c", "dm:a:d", "all"]);
        assert_eq!(
            ids(&list(&conn, &Viewer::Owner, 100).unwrap()),
            ["dm:b:c", "dm:a:d", "all", "dm:a:c", "dm:a:b"]
        );
        assert_eq!(
            ids(&list(&conn, &Viewer::Owner, 1).unwrap()),
            ["all"],
            "all always"
        );
    }

    #[test]
    fn all_without_messages_comes_after_rooms_with_messages() {
        let conn = database(&["b", "a"]);
        insert(&conn, 1, "dm:a:b", "b");
        let rooms = list(&conn, &Viewer::Owner, 10).unwrap();
        assert_eq!(ids(&rooms), ["dm:a:b", "all"]);
        let all = &rooms[1];
        assert_eq!((all.message_count, all.last_event_seq), (0, None));
        assert!(all.members.is_empty(), "SQL registration does not join all");
        assert_eq!(
            rooms[0].last_sender_id.as_ref().map(AgentId::as_str),
            Some("b")
        );
    }

    #[test]
    fn access_reads_with_any_row_and_writes_only_when_joined() {
        let left = RoomAccess {
            room: None,
            membership: Some(Joined(false)),
        };
        assert!(left.can_read() && !left.can_write());
        let none = RoomAccess {
            room: None,
            membership: None,
        };
        assert!(!none.can_read() && !none.can_write());
    }
}
