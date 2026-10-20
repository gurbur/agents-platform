//! Agent presence in memory (`IF-WIRE-013`, ADR-0012): the declared
//! entries with their TTL and the `last_seen_at` throttle. Nothing here
//! touches the database; a restart empties both maps (everyone offline, no
//! throttle).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use platform_wire::{AgentId, Presence, PresenceState};

use crate::clock::format_rfc3339_ms;

/// One declaration. `updated_at` and `expires_at` are whole milliseconds
/// (`Clock::now`), so the stored instants equal their RFC 3339 text.
#[derive(Debug, Clone)]
struct Entry {
    state: PresenceState,
    note: Option<String>,
    updated_at: SystemTime,
    expires_at: SystemTime,
}

#[derive(Debug, Default)]
struct Maps {
    entries: HashMap<AgentId, Entry>,
    /// When `agents.last_seen_at` was last sent for each agent.
    last_seen_written: HashMap<AgentId, Instant>,
}

/// The shared presence state of one server. Cheap to clone. The lock is
/// held only for map operations: never across an await or any I/O.
#[derive(Debug, Clone, Default)]
pub struct PresenceBoard {
    maps: Arc<Mutex<Maps>>,
}

impl PresenceBoard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every operation leaves both maps consistent, so a panic elsewhere
    /// while the lock was held cannot have broken them: keep using them.
    fn lock(&self) -> MutexGuard<'_, Maps> {
        self.maps.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Replaces the agent's whole entry (an omitted note removes the old
    /// one) and returns what the directory now shows.
    pub fn set(
        &self,
        agent_id: &AgentId,
        state: PresenceState,
        note: Option<String>,
        ttl: Duration,
        now: SystemTime,
    ) -> Presence {
        let entry = Entry {
            state,
            note,
            updated_at: now,
            expires_at: now + ttl,
        };
        let presence = shown(&entry);
        self.lock().entries.insert(agent_id.clone(), entry);
        presence
    }

    /// Withdraws the declaration; nothing to do when there is none.
    pub fn clear(&self, agent_id: &AgentId) {
        self.lock().entries.remove(agent_id);
    }

    /// The agent's presence at `now`: `offline` without an entry or once
    /// `now >= expires_at` (the expired entry is dropped here).
    pub fn view(&self, agent_id: &AgentId, now: SystemTime) -> Presence {
        let mut maps = self.lock();
        let Some(entry) = maps.entries.get(agent_id) else {
            return Presence::offline();
        };
        if now >= entry.expires_at {
            maps.entries.remove(agent_id);
            return Presence::offline();
        }
        shown(entry)
    }

    /// Forgets the agent in both maps: deletion and (re)registration call
    /// it so a new agent with an old id inherits neither (`IF-WIRE-009`).
    pub fn forget(&self, agent_id: &AgentId) {
        let mut maps = self.lock();
        maps.entries.remove(agent_id);
        maps.last_seen_written.remove(agent_id);
    }

    /// Whether `agents.last_seen_at` should be written for this request:
    /// `true` when nothing was recorded or `interval` has passed since, and
    /// then `now` is recorded right away.
    pub fn should_touch(&self, agent_id: &AgentId, now: Instant, interval: Duration) -> bool {
        let mut maps = self.lock();
        let due = maps
            .last_seen_written
            .get(agent_id)
            .is_none_or(|&written| now.saturating_duration_since(written) >= interval);
        if due {
            maps.last_seen_written.insert(agent_id.clone(), now);
        }
        due
    }
}

fn shown(entry: &Entry) -> Presence {
    Presence {
        state: entry.state,
        note: entry.note.clone(),
        updated_at: Some(format_rfc3339_ms(entry.updated_at)),
        expires_at: Some(format_rfc3339_ms(entry.expires_at)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    const TTL: Duration = Duration::from_secs(300);
    const INTERVAL: Duration = Duration::from_secs(60);

    fn agent(id: &str) -> AgentId {
        id.parse().unwrap()
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_800_000_000 + secs)
    }

    #[test]
    fn an_agent_without_an_entry_is_offline() {
        let board = PresenceBoard::new();
        assert_eq!(board.view(&agent("agent-a"), at(0)), Presence::offline());
    }

    #[test]
    fn a_declaration_shows_until_it_expires() {
        let board = PresenceBoard::new();
        let a = agent("agent-a");
        let shown = board.set(&a, PresenceState::Busy, Some("note".into()), TTL, at(0));
        assert_eq!(shown.state, PresenceState::Busy);
        assert_eq!(shown.note.as_deref(), Some("note"));
        assert_eq!(shown.updated_at, Some(format_rfc3339_ms(at(0))));
        assert_eq!(shown.expires_at, Some(format_rfc3339_ms(at(0) + TTL)));
        assert_eq!(board.view(&a, at(0)), shown);
        assert_eq!(
            board.view(&a, at(0) + TTL - Duration::from_millis(1)),
            shown
        );
        assert_eq!(
            board.view(&a, at(0) + TTL),
            Presence::offline(),
            "now >= expires_at"
        );
        assert!(
            board.lock().entries.is_empty(),
            "the expired entry is dropped"
        );
        assert_eq!(board.view(&agent("agent-b"), at(0)), Presence::offline());
    }

    #[test]
    fn a_new_declaration_replaces_the_whole_entry() {
        let board = PresenceBoard::new();
        let a = agent("agent-a");
        board.set(&a, PresenceState::Busy, Some("note".into()), TTL, at(0));
        let again = board.set(&a, PresenceState::Online, None, TTL, at(10));
        assert_eq!(again.state, PresenceState::Online);
        assert_eq!(again.note, None, "an omitted note removes the old one");
        assert_eq!(again.updated_at, Some(format_rfc3339_ms(at(10))));
        assert_eq!(again.expires_at, Some(format_rfc3339_ms(at(10) + TTL)));
        assert_eq!(board.view(&a, at(10)), again);
    }

    #[test]
    fn clear_withdraws_at_once_and_is_idempotent() {
        let board = PresenceBoard::new();
        let a = agent("agent-a");
        board.clear(&a);
        board.set(&a, PresenceState::Online, None, TTL, at(0));
        board.clear(&a);
        assert_eq!(board.view(&a, at(0)), Presence::offline());
        board.clear(&a);
    }

    #[test]
    fn the_throttle_writes_once_per_interval_and_per_agent() {
        let board = PresenceBoard::new();
        let (a, b) = (agent("agent-a"), agent("agent-b"));
        let start = Instant::now();
        assert!(
            board.should_touch(&a, start, INTERVAL),
            "first request writes"
        );
        assert!(!board.should_touch(&a, start, INTERVAL));
        assert!(!board.should_touch(&a, start + INTERVAL - Duration::from_millis(1), INTERVAL));
        assert!(board.should_touch(&b, start, INTERVAL), "per agent");
        assert!(
            board.should_touch(&a, start + INTERVAL, INTERVAL),
            "after the interval"
        );
        assert!(!board.should_touch(&a, start + INTERVAL, INTERVAL));
    }

    #[test]
    fn forget_empties_both_maps_for_that_agent_only() {
        let board = PresenceBoard::new();
        let (a, b) = (agent("agent-a"), agent("agent-b"));
        let start = Instant::now();
        board.set(&a, PresenceState::Busy, None, TTL, at(0));
        board.set(&b, PresenceState::Online, None, TTL, at(0));
        assert!(board.should_touch(&a, start, INTERVAL));
        assert!(board.should_touch(&b, start, INTERVAL));

        board.forget(&a);

        assert_eq!(board.view(&a, at(0)), Presence::offline());
        assert!(
            board.should_touch(&a, start, INTERVAL),
            "no throttle inherited"
        );
        assert_eq!(board.view(&b, at(0)).state, PresenceState::Online);
        assert!(!board.should_touch(&b, start, INTERVAL));
    }
}
