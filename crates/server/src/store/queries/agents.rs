//! Agents, their directory profile, `all` membership, receiver route and
//! credentials (`IF-DB-001` tables `agents`, `memberships`, `read_state`,
//! `notification_pending`, `sequences`).

use std::str::FromStr;
use std::time::SystemTime;

use platform_wire::{
    AgeBand, AgentId, AgentProfile, DeliveryState, GenderForm, ProfileField, ProfileUpdate,
};
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
    /// Already validated and trimmed (`platform_wire::validate_name`).
    pub name: String,
    pub receiver_url: Option<String>,
    pub api_key_hash: String,
    pub receiver_secret: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredAgent {
    pub agent_id: AgentId,
    pub name: String,
    pub joined_all: bool,
    pub receiver_url: Option<String>,
    pub route_version: i64,
    /// Nothing declared yet; `updated_at` is the registration time.
    pub profile: AgentProfile,
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
    /// `None` for an agent registered before S9 until the owner names it.
    pub name: Option<String>,
    pub profile: AgentProfile,
    pub joined_all: bool,
    pub receiver_url: Option<String>,
    pub route_version: i64,
    pub created_at: String,
    pub delivery_state: DeliveryState,
}

/// An agent's display name and profile as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRecord {
    pub name: Option<String>,
    pub profile: AgentProfile,
}

/// Result of [`update_profile`]: the stored values afterwards and the
/// fields whose value actually changed, in `IF-WIRE-006` field order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileChange {
    pub record: ProfileRecord,
    pub changed: Vec<ProfileField>,
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
        "INSERT INTO agents (agent_id, api_key_hash, receiver_secret, receiver_url, route_version,
                             created_at, name, profile_updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?6)",
        params![
            id,
            agent.api_key_hash,
            agent.receiver_secret,
            agent.receiver_url,
            INITIAL_ROUTE_VERSION,
            agent.created_at,
            agent.name,
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
        name: agent.name.clone(),
        joined_all: true,
        receiver_url: agent.receiver_url.clone(),
        route_version: INITIAL_ROUTE_VERSION,
        profile: AgentProfile {
            updated_at: Some(agent.created_at.clone()),
            ..AgentProfile::default()
        },
    }))
}

/// Every agent in ASCII `agent_id` order (SQLite BINARY collation).
pub fn list(conn: &Connection) -> Result<Vec<AgentRecord>, StoreError> {
    let mut statement = conn.prepare(
        "SELECT a.agent_id, m.joined, a.receiver_url, a.route_version, a.created_at, n.state,
                a.name, a.gender_form, a.age_band, a.role, a.intro, a.profile_updated_at
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
            StoredProfile::from_row(row, 6)?,
        ))
    })?;
    let mut records = Vec::new();
    for row in rows {
        let (agent_id, joined_all, receiver_url, route_version, created_at, state, stored) = row?;
        let delivery_state = display_state(receiver_url.as_deref(), &state)?;
        let ProfileRecord { name, profile } = stored.into_record()?;
        records.push(AgentRecord {
            agent_id: stored_agent_id(agent_id)?,
            name,
            profile,
            joined_all,
            receiver_url,
            route_version,
            created_at,
            delivery_state,
        });
    }
    Ok(records)
}

/// Applies a validated partial update. The row is written only when a
/// stored value actually changes, and then `profile_updated_at` becomes
/// `now`; otherwise nothing is written and `updated_at` stays (`IF-WIRE-006`).
/// `None` when the agent does not exist.
pub fn update_profile(
    tx: &Transaction,
    agent_id: &AgentId,
    update: &ProfileUpdate,
    now: &str,
) -> Result<Option<ProfileChange>, StoreError> {
    let current = tx
        .query_row(
            "SELECT name, gender_form, age_band, role, intro, profile_updated_at
             FROM agents WHERE agent_id = ?1",
            params![agent_id.as_str()],
            |row| StoredProfile::from_row(row, 0),
        )
        .optional()?;
    let Some(current) = current else {
        return Ok(None);
    };

    let (mut next, changed) = current.patched(update);
    if !changed.is_empty() {
        next.updated_at = Some(now.to_owned());
        tx.execute(
            "UPDATE agents SET name = ?1, gender_form = ?2, age_band = ?3, role = ?4, intro = ?5,
                               profile_updated_at = ?6
             WHERE agent_id = ?7",
            params![
                next.name,
                next.gender_form,
                next.age_band,
                next.role,
                next.intro,
                next.updated_at,
                agent_id.as_str(),
            ],
        )?;
    }
    Ok(Some(ProfileChange {
        record: next.into_record()?,
        changed,
    }))
}

/// The profile columns of one `agents` row, enums still as stored text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct StoredProfile {
    name: Option<String>,
    gender_form: Option<String>,
    age_band: Option<String>,
    role: Option<String>,
    intro: Option<String>,
    updated_at: Option<String>,
}

impl StoredProfile {
    /// Reads `name, gender_form, age_band, role, intro, profile_updated_at`
    /// starting at column `first`.
    fn from_row(row: &rusqlite::Row<'_>, first: usize) -> rusqlite::Result<Self> {
        Ok(Self {
            name: row.get(first)?,
            gender_form: row.get(first + 1)?,
            age_band: row.get(first + 2)?,
            role: row.get(first + 3)?,
            intro: row.get(first + 4)?,
            updated_at: row.get(first + 5)?,
        })
    }

    /// The values after `update` (without touching `updated_at`) and the
    /// fields that differ from `self`.
    fn patched(&self, update: &ProfileUpdate) -> (StoredProfile, Vec<ProfileField>) {
        let next = StoredProfile {
            name: update.name.clone().or_else(|| self.name.clone()),
            gender_form: update
                .gender_form
                .clone()
                .map(|value| value.as_str().to_owned())
                .apply_to(self.gender_form.clone()),
            age_band: update
                .age_band
                .clone()
                .map(|value| value.as_str().to_owned())
                .apply_to(self.age_band.clone()),
            role: update.role.clone().apply_to(self.role.clone()),
            intro: update.intro.clone().apply_to(self.intro.clone()),
            updated_at: self.updated_at.clone(),
        };
        let pairs = [
            (ProfileField::Name, &self.name, &next.name),
            (
                ProfileField::GenderForm,
                &self.gender_form,
                &next.gender_form,
            ),
            (ProfileField::AgeBand, &self.age_band, &next.age_band),
            (ProfileField::Role, &self.role, &next.role),
            (ProfileField::Intro, &self.intro, &next.intro),
        ];
        let changed = pairs
            .into_iter()
            .filter(|(_, before, after)| before != after)
            .map(|(field, _, _)| field)
            .collect();
        (next, changed)
    }

    fn into_record(self) -> Result<ProfileRecord, StoreError> {
        Ok(ProfileRecord {
            name: self.name,
            profile: AgentProfile {
                gender_form: stored_enum::<GenderForm>("gender_form", self.gender_form)?,
                age_band: stored_enum::<AgeBand>("age_band", self.age_band)?,
                role: self.role,
                intro: self.intro,
                updated_at: self.updated_at,
            },
        })
    }
}

/// Parses a stored enum column. Values are written only from validated
/// wire enums, so anything else is a broken invariant.
fn stored_enum<T: FromStr>(column: &str, stored: Option<String>) -> Result<Option<T>, StoreError> {
    stored
        .map(|text| {
            text.parse().map_err(|_| {
                StoreError::Invariant(format!("agents.{column} holds an unknown value"))
            })
        })
        .transpose()
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
    use platform_wire::{FieldPatch, UpdateProfileRequest};

    fn update(body: serde_json::Value) -> ProfileUpdate {
        serde_json::from_value::<UpdateProfileRequest>(body)
            .unwrap()
            .validate()
            .unwrap()
    }

    fn declared() -> StoredProfile {
        StoredProfile {
            name: Some("A".into()),
            gender_form: Some("none".into()),
            age_band: None,
            role: Some("ops".into()),
            intro: Some("hello".into()),
            updated_at: Some("2026-10-17T09:00:00.000Z".into()),
        }
    }

    #[test]
    fn patch_reports_only_fields_whose_value_changes() {
        let current = declared();
        let (next, changed) = current.patched(&update(serde_json::json!({
            "name": "A", "gender_form": "female", "role": null, "age_band": null
        })));
        assert_eq!(changed, [ProfileField::GenderForm, ProfileField::Role]);
        assert_eq!(next.gender_form.as_deref(), Some("female"));
        assert_eq!(next.role, None);
        assert_eq!(
            next.intro.as_deref(),
            Some("hello"),
            "omitted field is kept"
        );
        assert_eq!(
            next.updated_at, current.updated_at,
            "the caller sets updated_at"
        );
    }

    #[test]
    fn same_values_and_empty_patches_change_nothing() {
        let current = declared();
        for body in [
            serde_json::json!({}),
            serde_json::json!({"name": " A ", "gender_form": "none", "age_band": null, "intro": "hello"}),
        ] {
            let (next, changed) = current.patched(&update(body));
            assert!(changed.is_empty());
            assert_eq!(next, current);
        }
        assert_eq!(update(serde_json::json!({})).role, FieldPatch::Keep);
    }

    #[test]
    fn unknown_stored_enum_is_an_invariant_error() {
        let broken = StoredProfile {
            age_band: Some("ancient".into()),
            ..StoredProfile::default()
        };
        assert!(matches!(
            broken.into_record(),
            Err(StoreError::Invariant(_))
        ));
    }

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
