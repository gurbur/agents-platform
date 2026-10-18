//! `notification_pending` (`IF-DB-001`) under the `IF-DLV-001` transitions.
//!
//! Each entry point loads one agent's row together with its inbox cursor and
//! route, applies a [`transitions`] function and writes the row back, all in
//! the caller's transaction. S1 (receiver PATCH, rotate), S2 (send) and S3
//! (read marker) call the hooks from their own transactions so the state
//! change commits atomically with the event that caused it.

use std::fmt;
use std::time::SystemTime;

use platform_wire::{AgentId, DeliveryRetryResponse, DeliveryStatus, Seq};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use super::agents::stored_agent_id;
use super::messages::stored_seq;
use crate::delivery::classify::AttemptOutcome;
use crate::delivery::transitions::{
    self, Applied, InboxCursor, Notification, Posted, RetryPolicy, StoredState,
};
use crate::store::StoreError;

/// One agent's delivery row with everything a transition reads.
struct DeliveryRecord {
    notification: Notification,
    cursor: InboxCursor,
    receiver_url: Option<String>,
    receiver_secret: String,
    /// `agents.route_version`, the route's current version.
    route_version: i64,
}

/// A pending agent as the worker schedules it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scheduled {
    pub agent_id: AgentId,
    /// `None`: nothing to do until a signal (e.g. waiting for a commit).
    pub due_at: Option<SystemTime>,
}

/// The snapshot an attempt POSTs with.
#[derive(Clone)]
pub struct Attempt {
    pub agent_id: AgentId,
    pub receiver_url: String,
    pub receiver_secret: String,
    pub posted: Posted,
}

impl fmt::Debug for Attempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The URL may carry a token in its query; neither it nor the secret
        // belongs in debug output.
        f.debug_struct("Attempt")
            .field("agent_id", &self.agent_id)
            .field("posted", &self.posted)
            .finish_non_exhaustive()
    }
}

const RECORD_SQL: &str =
    "SELECT n.route_version, n.state, n.target_seq, n.last_accepted_seq, n.attempt,
            n.next_due_at, n.redrive_due_at, n.last_attempt_at, n.last_error,
            a.inbox_head, s.read_marker, a.receiver_url, a.receiver_secret, a.route_version
     FROM notification_pending n
     JOIN agents a ON a.agent_id = n.agent_id
     JOIN read_state s ON s.agent_id = n.agent_id
     WHERE n.agent_id = ?1";

/// `GET /v1/agents/{agent_id}/delivery`. `None` when the agent does not exist.
pub fn status(conn: &Connection, agent_id: &AgentId) -> Result<Option<DeliveryStatus>, StoreError> {
    let Some(record) = load(conn, agent_id)? else {
        return Ok(None);
    };
    let present = record.receiver_url.is_some();
    let notification = record.notification;
    Ok(Some(DeliveryStatus {
        read_marker: record.cursor.read_marker,
        inbox_head_seq: record.cursor.inbox_head,
        pending_seq: record.cursor.pending_seq(),
        last_accepted_seq: notification.last_accepted_seq,
        route_version: record.route_version,
        receiver_url_present: present,
        state: transitions::display_state(present, notification.state),
        last_attempt_at: notification.last_attempt_at,
        last_error: notification.last_error,
    }))
}

/// `POST /v1/agents/{agent_id}/delivery/retry`. `None` when the agent does
/// not exist.
pub fn retry(
    tx: &Transaction,
    agent_id: &AgentId,
    now: SystemTime,
) -> Result<Option<DeliveryRetryResponse>, StoreError> {
    let Some(mut record) = load(tx, agent_id)? else {
        return Ok(None);
    };
    let state = update(tx, agent_id, &mut record, |n, view| {
        n.manual_retry(view.receiver_url_present, view.cursor, now)
    })?;
    Ok(Some(DeliveryRetryResponse {
        state,
        route_version: record.route_version,
        pending_seq: record.cursor.pending_seq(),
        last_accepted_seq: record.notification.last_accepted_seq,
    }))
}

/// S2 hook: `agent_id` just received an inbox row (its head moved).
pub fn record_new_inbox_row(
    tx: &Transaction,
    agent_id: &AgentId,
    now: SystemTime,
) -> Result<(), StoreError> {
    let mut record = require(tx, agent_id)?;
    update(tx, agent_id, &mut record, |n, view| {
        n.on_new_inbox_row(view.cursor, now);
    })
}

/// S3 hook: `agent_id`'s read marker moved up.
pub fn record_read_marker(tx: &Transaction, agent_id: &AgentId) -> Result<(), StoreError> {
    let mut record = require(tx, agent_id)?;
    update(tx, agent_id, &mut record, |n, view| {
        n.on_read_marker(view.cursor)
    })
}

/// S1 hook: `agents.route_version` of `agent_id` was just incremented
/// (receiver URL change or credential rotation).
pub fn record_route_change(
    tx: &Transaction,
    agent_id: &AgentId,
    now: SystemTime,
) -> Result<(), StoreError> {
    let mut record = require(tx, agent_id)?;
    update(tx, agent_id, &mut record, |n, view| {
        n.on_route_change(
            view.route_version,
            view.receiver_url_present,
            view.cursor,
            now,
        );
    })
}

/// Pending agents with a receiver URL and their due times, for the worker.
pub fn schedule(conn: &Connection) -> Result<Vec<Scheduled>, StoreError> {
    let mut statement = conn.prepare(
        "SELECT n.agent_id, n.next_due_at, n.redrive_due_at
         FROM notification_pending n JOIN agents a ON a.agent_id = n.agent_id
         WHERE n.state = ?1 AND a.receiver_url IS NOT NULL
         ORDER BY n.agent_id",
    )?;
    let rows = statement.query_map(params![StoredState::Pending.as_str()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut scheduled = Vec::new();
    for row in rows {
        let (agent_id, next_due_at, redrive_due_at) = row?;
        scheduled.push(Scheduled {
            agent_id: stored_agent_id(agent_id)?,
            due_at: transitions::due_time(next_due_at.as_deref(), redrive_due_at.as_deref()),
        });
    }
    Ok(scheduled)
}

/// Starts an attempt if `agent_id` is due at `now` and has a receiver URL:
/// records `last_attempt_at` and returns the snapshot to POST with.
pub fn begin_attempt(
    tx: &Transaction,
    agent_id: &AgentId,
    now: SystemTime,
) -> Result<Option<Attempt>, StoreError> {
    let Some(mut record) = load(tx, agent_id)? else {
        return Ok(None);
    };
    let Some(receiver_url) = record.receiver_url.clone() else {
        return Ok(None);
    };
    let posted_seq = update(tx, agent_id, &mut record, |n, view| {
        n.begin_attempt(view.cursor, now)
    })?;
    let Some(posted_seq) = posted_seq else {
        return Ok(None);
    };
    Ok(Some(Attempt {
        agent_id: agent_id.clone(),
        receiver_url,
        receiver_secret: record.receiver_secret,
        posted: Posted {
            route_version: record.notification.route_version,
            posted_seq,
        },
    }))
}

/// Records the outcome of an attempt, unless the route moved on meanwhile.
/// `jitter` is a sample from `[-JITTER, JITTER]` for the backoff.
///
/// The agent may have been deleted while the POST was in flight, and its id
/// registered again: a successor starts at `route_version` 1 like the old
/// agent, so the receiver secret tells them apart (`IF-DLV-001` "삭제·재등록").
/// Either way the response belongs to no current route.
pub fn apply_outcome(
    tx: &Transaction,
    attempt: &Attempt,
    outcome: &AttemptOutcome,
    now: SystemTime,
    policy: &RetryPolicy,
    jitter: f64,
) -> Result<Applied, StoreError> {
    let Some(mut record) = load(tx, &attempt.agent_id)? else {
        return Ok(Applied::Ignored);
    };
    if record.receiver_secret != attempt.receiver_secret {
        return Ok(Applied::Ignored);
    }
    update(tx, &attempt.agent_id, &mut record, |n, view| {
        n.apply_outcome(attempt.posted, outcome, view.cursor, now, policy, jitter)
    })
}

fn load(conn: &Connection, agent_id: &AgentId) -> Result<Option<DeliveryRecord>, StoreError> {
    let raw = conn
        .query_row(RECORD_SQL, params![agent_id.as_str()], RawRecord::from_row)
        .optional()?;
    raw.map(RawRecord::into_record).transpose()
}

/// Like [`load`] for an agent that must exist: every hook runs right after
/// its caller touched the agent's rows in the same transaction.
fn require(conn: &Connection, agent_id: &AgentId) -> Result<DeliveryRecord, StoreError> {
    load(conn, agent_id)?
        .ok_or_else(|| StoreError::Invariant("agent has no notification_pending row".to_owned()))
}

/// Applies `transition` to the loaded row and writes it back if it changed.
fn update<T>(
    tx: &Transaction,
    agent_id: &AgentId,
    record: &mut DeliveryRecord,
    transition: impl FnOnce(&mut Notification, &RecordView) -> T,
) -> Result<T, StoreError> {
    let before = record.notification.clone();
    let view = RecordView {
        cursor: record.cursor,
        receiver_url_present: record.receiver_url.is_some(),
        route_version: record.route_version,
    };
    let value = transition(&mut record.notification, &view);
    if record.notification != before {
        save(tx, agent_id, &record.notification)?;
    }
    Ok(value)
}

/// The read-only part of a [`DeliveryRecord`] a transition looks at.
struct RecordView {
    cursor: InboxCursor,
    receiver_url_present: bool,
    route_version: i64,
}

fn save(tx: &Transaction, agent_id: &AgentId, row: &Notification) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE notification_pending SET route_version = ?2, state = ?3, target_seq = ?4,
                last_accepted_seq = ?5, attempt = ?6, next_due_at = ?7, redrive_due_at = ?8,
                last_attempt_at = ?9, last_error = ?10
         WHERE agent_id = ?1",
        params![
            agent_id.as_str(),
            row.route_version,
            row.state.as_str(),
            row.target_seq.get(),
            row.last_accepted_seq.map(Seq::get),
            row.attempt,
            row.next_due_at,
            row.redrive_due_at,
            row.last_attempt_at,
            row.last_error,
        ],
    )?;
    Ok(())
}

/// [`RECORD_SQL`] columns before validation.
struct RawRecord {
    route_version: i64,
    state: String,
    target_seq: i64,
    last_accepted_seq: Option<i64>,
    attempt: i64,
    next_due_at: Option<String>,
    redrive_due_at: Option<String>,
    last_attempt_at: Option<String>,
    last_error: Option<String>,
    inbox_head: i64,
    read_marker: i64,
    receiver_url: Option<String>,
    receiver_secret: String,
    agent_route_version: i64,
}

impl RawRecord {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            route_version: row.get(0)?,
            state: row.get(1)?,
            target_seq: row.get(2)?,
            last_accepted_seq: row.get(3)?,
            attempt: row.get(4)?,
            next_due_at: row.get(5)?,
            redrive_due_at: row.get(6)?,
            last_attempt_at: row.get(7)?,
            last_error: row.get(8)?,
            inbox_head: row.get(9)?,
            read_marker: row.get(10)?,
            receiver_url: row.get(11)?,
            receiver_secret: row.get(12)?,
            agent_route_version: row.get(13)?,
        })
    }

    fn into_record(self) -> Result<DeliveryRecord, StoreError> {
        let state = StoredState::parse(&self.state).ok_or_else(|| {
            StoreError::Invariant(format!(
                "unknown notification_pending.state '{}'",
                self.state
            ))
        })?;
        let attempt = u32::try_from(self.attempt).map_err(|_| {
            StoreError::Invariant("notification_pending.attempt is out of range".to_owned())
        })?;
        let last_accepted_seq = self
            .last_accepted_seq
            .map(|value| stored_seq(value, "notification_pending.last_accepted_seq"))
            .transpose()?;
        Ok(DeliveryRecord {
            notification: Notification {
                route_version: self.route_version,
                state,
                target_seq: stored_seq(self.target_seq, "notification_pending.target_seq")?,
                last_accepted_seq,
                attempt,
                next_due_at: self.next_due_at,
                redrive_due_at: self.redrive_due_at,
                last_attempt_at: self.last_attempt_at,
                last_error: self.last_error,
            },
            cursor: InboxCursor {
                inbox_head: stored_seq(self.inbox_head, "agents.inbox_head")?,
                read_marker: stored_seq(self.read_marker, "read_state.read_marker")?,
            },
            receiver_url: self.receiver_url,
            receiver_secret: self.receiver_secret,
            route_version: self.agent_route_version,
        })
    }
}
