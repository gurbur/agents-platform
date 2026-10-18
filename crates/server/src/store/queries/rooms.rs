//! `GET /v1/rooms` (`IF-WIRE-007`): rooms derived from `messages`, grouped
//! by `room_id`. The `messages_room_event(room_id, event_seq)` index covers
//! the count and the newest `event_seq`; the newest message is read back
//! through `UNIQUE(event_seq)`. No table of its own (`IF-DB-001`, S10).

use platform_wire::{AgentId, RoomId, RoomKind, RoomSummary};
use rusqlite::{Connection, OptionalExtension, params};

use super::agents::{ALL_ROOM, stored_agent_id};
use super::messages::{joined_all_members, stored_seq};
use crate::store::StoreError;

/// Every DM room with a message, newest last message first, at most
/// `cap - 1` of them, with `all` (always present) at its place in that
/// order; an `all` without messages comes last.
pub fn list_rooms(conn: &Connection, cap: usize) -> Result<Vec<RoomSummary>, StoreError> {
    let dm_limit = i64::try_from(cap.saturating_sub(1)).unwrap_or(i64::MAX);
    let mut statement = conn.prepare(
        "SELECT g.room_id, g.message_count, m.event_seq, m.created_at, m.sender_id
         FROM (SELECT room_id, COUNT(*) AS message_count, MAX(event_seq) AS last_seq
               FROM messages WHERE room_id <> ?1 GROUP BY room_id) AS g
         JOIN messages AS m ON m.event_seq = g.last_seq
         ORDER BY m.event_seq DESC
         LIMIT ?2",
    )?;
    let rows = statement.query_map(params![ALL_ROOM, dm_limit], |row| {
        Ok(StoredRoom {
            room_id: row.get(0)?,
            message_count: row.get(1)?,
            last: Some(StoredLast {
                event_seq: row.get(2)?,
                created_at: row.get(3)?,
                sender_id: row.get(4)?,
            }),
        })
    })?;
    let mut rooms = Vec::new();
    for row in rows {
        rooms.push(row?.into_summary(Vec::new())?);
    }

    let all = all_room(conn)?;
    // DM summaries are newest first; `all` goes before the first older one.
    let position = match all.last_event_seq {
        None => rooms.len(),
        Some(all_seq) => rooms
            .iter()
            .position(|room| room.last_event_seq.is_some_and(|seq| seq < all_seq))
            .unwrap_or(rooms.len()),
    };
    rooms.insert(position, all);
    Ok(rooms)
}

/// The `all` summary, with its current members.
fn all_room(conn: &Connection) -> Result<RoomSummary, StoreError> {
    let (message_count, last_seq): (i64, Option<i64>) = conn.query_row(
        "SELECT COUNT(*), MAX(event_seq) FROM messages WHERE room_id = ?1",
        params![ALL_ROOM],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let last = match last_seq {
        None => None,
        Some(event_seq) => conn
            .query_row(
                "SELECT created_at, sender_id FROM messages WHERE event_seq = ?1",
                params![event_seq],
                |row| {
                    Ok(StoredLast {
                        event_seq,
                        created_at: row.get(0)?,
                        sender_id: row.get(1)?,
                    })
                },
            )
            .optional()?,
    };
    let stored = StoredRoom {
        room_id: ALL_ROOM.to_owned(),
        message_count,
        last,
    };
    stored.into_summary(joined_all_members(conn)?)
}

struct StoredLast {
    event_seq: i64,
    created_at: String,
    sender_id: String,
}

/// One grouped `messages` row before its text columns are validated.
struct StoredRoom {
    room_id: String,
    message_count: i64,
    last: Option<StoredLast>,
}

impl StoredRoom {
    /// `all_members` is used for `all` only; a DM's members are its ids.
    fn into_summary(self, all_members: Vec<AgentId>) -> Result<RoomSummary, StoreError> {
        let room_id: RoomId = self
            .room_id
            .parse()
            .map_err(|err| StoreError::Invariant(format!("stored room_id is invalid: {err}")))?;
        let members = match &room_id {
            RoomId::All => all_members,
            RoomId::Dm(low, high) => vec![low.clone(), high.clone()],
        };
        let message_count = u64::try_from(self.message_count)
            .map_err(|_| StoreError::Invariant("negative message count".to_owned()))?;
        let (last_event_seq, last_message_at, last_sender_id) = match self.last {
            None => (None, None, None),
            Some(last) => (
                Some(stored_seq(last.event_seq, "messages.event_seq")?),
                Some(last.created_at),
                Some(stored_agent_id(last.sender_id)?),
            ),
        };
        Ok(RoomSummary {
            kind: RoomKind::of(&room_id),
            room_id,
            members,
            last_event_seq,
            last_message_at,
            last_sender_id,
            message_count,
        })
    }
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
    fn the_cap_counts_all_and_keeps_the_newest_dm_rooms() {
        let conn = database(&["a", "b", "c", "d"]);
        insert(&conn, 1, "dm:a:b", "a");
        insert(&conn, 2, "dm:a:c", "a");
        insert(&conn, 3, "all", "b");
        insert(&conn, 4, "dm:a:d", "d");
        insert(&conn, 5, "dm:b:c", "c");

        let capped = list_rooms(&conn, 3).unwrap();
        assert_eq!(ids(&capped), ["dm:b:c", "dm:a:d", "all"]);
        assert_eq!(
            ids(&list_rooms(&conn, 100).unwrap()),
            ["dm:b:c", "dm:a:d", "all", "dm:a:c", "dm:a:b"]
        );
        assert_eq!(ids(&list_rooms(&conn, 1).unwrap()), ["all"], "all always");
    }

    #[test]
    fn all_without_messages_comes_last_with_its_members() {
        let conn = database(&["b", "a"]);
        insert(&conn, 1, "dm:a:b", "b");
        let rooms = list_rooms(&conn, 10).unwrap();
        assert_eq!(ids(&rooms), ["dm:a:b", "all"]);
        let all = &rooms[1];
        assert_eq!((all.message_count, all.last_event_seq), (0, None));
        // Registration through SQL above does not join `all`.
        assert!(all.members.is_empty());
        assert_eq!(
            rooms[0].last_sender_id.as_ref().map(AgentId::as_str),
            Some("b")
        );
        assert_eq!(
            rooms[0].last_message_at.as_deref(),
            Some("2026-10-18T00:00:01.000Z")
        );
    }
}
