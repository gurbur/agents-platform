//! Agents, their `all` membership, receiver route and credentials
//! (`IF-DB-001` tables `agents`, `memberships`, `read_state`,
//! `notification_pending`, `sequences`).

use std::time::SystemTime;

use platform_wire::{AgentId, DeliveryState};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::delivery as delivery_queries;
use crate::delivery::transitions::{self, StoredState};
use crate::store::StoreError;

/// The only room with memberships in v1.
pub const ALL_ROOM: &str = "all";

/// `route_version` and `membership_epoch` of a freshly registered agent.
const INITIAL_ROUTE_VERSION: i64 = 1;
const INITIAL_MEMBERSHIP_EPOCH: i64 = 1;

/// Everything needed to insert an agent. Credentials are already generated;
/// only the API key hash and the receiver secret are stored.
pub struct NewAgent {
    pub agent_id: AgentId,
    pub receiver_url: Option<String>,
    pub api_key_hash: String,
    pub receiver_secret: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredAgent {
    pub agent_id: AgentId,
    pub joined_all: bool,
    pub receiver_url: Option<String>,
    pub route_version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    Created(RegisteredAgent),
    Taken,
}

/// One row of the directory, with the displayed delivery state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRecord {
    pub agent_id: AgentId,
    pub joined_all: bool,
    pub receiver_url: Option<String>,
    pub route_version: i64,
    pub created_at: String,
    pub delivery_state: DeliveryState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiverRoute {
    pub receiver_url: Option<String>,
    pub route_version: i64,
    pub delivery_state: DeliveryState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllMembership {
    pub joined: bool,
    pub epoch: i64,
    pub effective_event_seq: i64,
}

/// Inserts the agent with its `all` membership (joined, effective at the
/// current `event_seq`), a zero read marker and an idle notification row,
/// all in the caller's transaction.
pub fn register(tx: &Transaction, agent: &NewAgent) -> Result<Registration, StoreError> {
    // The IMMEDIATE transaction holds the write lock, so check-then-insert
    // cannot race another registration.
    if agent_exists(tx, &agent.agent_id)? {
        return Ok(Registration::Taken);
    }
    let event_seq = current_event_seq(tx)?;
    let id = agent.agent_id.as_str();

    tx.execute(
        "INSERT INTO agents (agent_id, api_key_hash, receiver_secret, receiver_url, route_version, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id,
            agent.api_key_hash,
            agent.receiver_secret,
            agent.receiver_url,
            INITIAL_ROUTE_VERSION,
            agent.created_at,
        ],
    )?;
    tx.execute(
        "INSERT INTO memberships (room_id, agent_id, joined, epoch, effective_event_seq)
         VALUES (?1, ?2, 1, ?3, ?4)",
        params![ALL_ROOM, id, INITIAL_MEMBERSHIP_EPOCH, event_seq],
    )?;
    tx.execute(
        "INSERT INTO read_state (agent_id, read_marker) VALUES (?1, 0)",
        params![id],
    )?;
    tx.execute(
        "INSERT INTO notification_pending (agent_id, route_version, state, target_seq)
         VALUES (?1, ?2, ?3, 0)",
        params![id, INITIAL_ROUTE_VERSION, StoredState::Idle.as_str()],
    )?;

    Ok(Registration::Created(RegisteredAgent {
        agent_id: agent.agent_id.clone(),
        joined_all: true,
        receiver_url: agent.receiver_url.clone(),
        route_version: INITIAL_ROUTE_VERSION,
    }))
}

/// Every agent in ASCII `agent_id` order (SQLite BINARY collation).
pub fn list(conn: &Connection) -> Result<Vec<AgentRecord>, StoreError> {
    let mut statement = conn.prepare(
        "SELECT a.agent_id, m.joined, a.receiver_url, a.route_version, a.created_at, n.state
         FROM agents a
         JOIN memberships m ON m.agent_id = a.agent_id AND m.room_id = ?1
         JOIN notification_pending n ON n.agent_id = a.agent_id
         ORDER BY a.agent_id",
    )?;
    let rows = statement.query_map(params![ALL_ROOM], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, bool>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;
    let mut records = Vec::new();
    for row in rows {
        let (agent_id, joined_all, receiver_url, route_version, created_at, state) = row?;
        let delivery_state = display_state(receiver_url.as_deref(), &state)?;
        records.push(AgentRecord {
            agent_id: stored_agent_id(agent_id)?,
            joined_all,
            receiver_url,
            route_version,
            created_at,
            delivery_state,
        });
    }
    Ok(records)
}

/// The agent whose API key hashes to `api_key_hash`.
pub fn find_by_api_key_hash(
    conn: &Connection,
    api_key_hash: &str,
) -> Result<Option<AgentId>, StoreError> {
    let found: Option<String> = conn
        .query_row(
            "SELECT agent_id FROM agents WHERE api_key_hash = ?1",
            params![api_key_hash],
            |row| row.get(0),
        )
        .optional()?;
    found.map(stored_agent_id).transpose()
}

/// Sets the receiver URL. `route_version` grows only when the value changes,
/// including null ↔ URL, and a change applies the route-change delivery
/// transition in the same transaction. `None` when the agent does not exist.
pub fn update_receiver(
    tx: &Transaction,
    agent_id: &AgentId,
    receiver_url: Option<String>,
) -> Result<Option<ReceiverRoute>, StoreError> {
    let current: Option<(Option<String>, i64)> = tx
        .query_row(
            "SELECT receiver_url, route_version FROM agents WHERE agent_id = ?1",
            params![agent_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((stored_url, stored_version)) = current else {
        return Ok(None);
    };

    let route_version = if stored_url == receiver_url {
        stored_version
    } else {
        let bumped = tx.query_row(
            "UPDATE agents SET receiver_url = ?1, route_version = route_version + 1
             WHERE agent_id = ?2 RETURNING route_version",
            params![receiver_url, agent_id.as_str()],
            |row| row.get(0),
        )?;
        delivery_queries::record_route_change(tx, agent_id, SystemTime::now())?;
        bumped
    };

    let state = stored_notification_state(tx, agent_id)?;
    let delivery_state = display_state(receiver_url.as_deref(), &state)?;
    Ok(Some(ReceiverRoute {
        receiver_url,
        route_version,
        delivery_state,
    }))
}

/// Joins or leaves `all`. A change bumps `epoch` and makes the new state
/// effective at the current `event_seq`; the same value is a no-op.
/// `None` when the agent does not exist.
pub fn set_all_membership(
    tx: &Transaction,
    agent_id: &AgentId,
    joined: bool,
) -> Result<Option<AllMembership>, StoreError> {
    let Some(current) = all_membership(tx, agent_id)? else {
        return Ok(None);
    };
    if current.joined == joined {
        return Ok(Some(current));
    }
    let event_seq = current_event_seq(tx)?;
    let epoch = tx.query_row(
        "UPDATE memberships SET joined = ?1, epoch = epoch + 1, effective_event_seq = ?2
         WHERE room_id = ?3 AND agent_id = ?4 RETURNING epoch",
        params![joined, event_seq, ALL_ROOM, agent_id.as_str()],
        |row| row.get(0),
    )?;
    Ok(Some(AllMembership {
        joined,
        epoch,
        effective_event_seq: event_seq,
    }))
}

/// Replaces both credentials, bumps `route_version` and applies the
/// route-change delivery transition. Returns the new `route_version`, or
/// `None` when the agent does not exist.
pub fn rotate_credentials(
    tx: &Transaction,
    agent_id: &AgentId,
    api_key_hash: &str,
    receiver_secret: &str,
) -> Result<Option<i64>, StoreError> {
    let route_version = tx
        .query_row(
            "UPDATE agents SET api_key_hash = ?1, receiver_secret = ?2, route_version = route_version + 1
             WHERE agent_id = ?3 RETURNING route_version",
            params![api_key_hash, receiver_secret, agent_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    if route_version.is_some() {
        delivery_queries::record_route_change(tx, agent_id, SystemTime::now())?;
    }
    Ok(route_version)
}

/// The last issued `event_seq` (`sequences('event_seq')`).
pub fn current_event_seq(conn: &Connection) -> Result<i64, StoreError> {
    let value = conn.query_row(
        "SELECT value FROM sequences WHERE name = 'event_seq'",
        [],
        |row| row.get(0),
    )?;
    Ok(value)
}

pub(crate) fn agent_exists(conn: &Connection, agent_id: &AgentId) -> Result<bool, StoreError> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM agents WHERE agent_id = ?1",
            params![agent_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

fn all_membership(
    conn: &Connection,
    agent_id: &AgentId,
) -> Result<Option<AllMembership>, StoreError> {
    let membership = conn
        .query_row(
            "SELECT joined, epoch, effective_event_seq FROM memberships
             WHERE room_id = ?1 AND agent_id = ?2",
            params![ALL_ROOM, agent_id.as_str()],
            |row| {
                Ok(AllMembership {
                    joined: row.get(0)?,
                    epoch: row.get(1)?,
                    effective_event_seq: row.get(2)?,
                })
            },
        )
        .optional()?;
    Ok(membership)
}

fn stored_notification_state(conn: &Connection, agent_id: &AgentId) -> Result<String, StoreError> {
    let state = conn.query_row(
        "SELECT state FROM notification_pending WHERE agent_id = ?1",
        params![agent_id.as_str()],
        |row| row.get(0),
    )?;
    Ok(state)
}

/// `IF-DLV-001`: a null `receiver_url` displays as `no_receiver`; otherwise
/// the stored `notification_pending.state`.
fn display_state(receiver_url: Option<&str>, stored: &str) -> Result<DeliveryState, StoreError> {
    let state = StoredState::parse(stored).ok_or_else(|| {
        StoreError::Invariant(format!("unknown notification_pending.state '{stored}'"))
    })?;
    Ok(transitions::display_state(receiver_url.is_some(), state))
}

pub(crate) fn stored_agent_id(raw: String) -> Result<AgentId, StoreError> {
    AgentId::try_from(raw)
        .map_err(|err| StoreError::Invariant(format!("stored agent_id is invalid: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_receiver_displays_no_receiver_whatever_is_stored() {
        for stored in ["idle", "pending", "blocked"] {
            assert_eq!(
                display_state(None, stored).unwrap(),
                DeliveryState::NoReceiver
            );
        }
    }

    #[test]
    fn set_receiver_displays_the_stored_state() {
        let url = Some("http://receiver.test/notify");
        assert_eq!(display_state(url, "idle").unwrap(), DeliveryState::Idle);
        assert_eq!(
            display_state(url, "pending").unwrap(),
            DeliveryState::Pending
        );
        assert_eq!(
            display_state(url, "blocked").unwrap(),
            DeliveryState::Blocked
        );
        assert!(display_state(url, "bogus").is_err());
    }
}
