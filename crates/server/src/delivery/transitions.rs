//! The `IF-DLV-001` transition table (S4 slice spec) as pure functions over
//! one `notification_pending` row. No I/O and no clock reads: callers pass
//! `now` and the jitter sample, and `store::queries::delivery` loads and
//! saves the row inside the caller's transaction.
//!
//! Every transition ends in the same invariant: with a receiver set, a
//! `read_marker` at the head means `idle` (`IF-DLV-001`, PLATFORM spec
//! "delivery 상태의 우선순위").

use std::time::{Duration, SystemTime};

use platform_wire::{DeliveryState, Seq};

use super::classify::AttemptOutcome;
use crate::clock;
use crate::config::DeliveryConfig;

/// Stored `notification_pending.state`. `no_receiver` is never stored; it is
/// derived from a null `receiver_url`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredState {
    Idle,
    Pending,
    Blocked,
}

impl StoredState {
    pub fn as_str(self) -> &'static str {
        match self {
            StoredState::Idle => "idle",
            StoredState::Pending => "pending",
            StoredState::Blocked => "blocked",
        }
    }

    pub fn parse(stored: &str) -> Option<Self> {
        match stored {
            "idle" => Some(StoredState::Idle),
            "pending" => Some(StoredState::Pending),
            "blocked" => Some(StoredState::Blocked),
            _ => None,
        }
    }
}

/// The displayed state: `no_receiver` whenever the URL is null, otherwise
/// the stored state.
pub fn display_state(receiver_url_present: bool, stored: StoredState) -> DeliveryState {
    if !receiver_url_present {
        return DeliveryState::NoReceiver;
    }
    match stored {
        StoredState::Idle => DeliveryState::Idle,
        StoredState::Pending => DeliveryState::Pending,
        StoredState::Blocked => DeliveryState::Blocked,
    }
}

/// An agent's inbox head and read marker at evaluation time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InboxCursor {
    pub inbox_head: Seq,
    pub read_marker: Seq,
}

impl InboxCursor {
    pub fn has_unread(self) -> bool {
        self.read_marker < self.inbox_head
    }

    /// `pending_seq` as displayed: the head while anything is unread.
    pub fn pending_seq(self) -> Option<Seq> {
        self.has_unread().then_some(self.inbox_head)
    }
}

/// One `notification_pending` row. Timestamps keep the stored
/// `clock::format_rfc3339_ms` text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub route_version: i64,
    pub state: StoredState,
    pub target_seq: Seq,
    pub last_accepted_seq: Option<Seq>,
    pub attempt: u32,
    pub next_due_at: Option<String>,
    pub redrive_due_at: Option<String>,
    pub last_attempt_at: Option<String>,
    pub last_error: Option<String>,
}

/// The route and sequence an attempt was started with (its snapshot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Posted {
    pub route_version: i64,
    pub posted_seq: Seq,
}

/// Whether a response changed the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// The response belongs to an older route; nothing was changed.
    Ignored,
    /// The response was recorded; the row is now in this state.
    Recorded(StoredState),
}

/// Backoff and redrive tuning from `[delivery]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    pub redrive_after: Duration,
}

/// Width of the jitter band: the sample is drawn from `[-JITTER, JITTER]`.
pub const JITTER: f64 = 0.5;

impl RetryPolicy {
    pub fn from_config(config: &DeliveryConfig) -> Self {
        Self {
            backoff_min: Duration::from_secs(config.backoff_min_secs),
            backoff_max: Duration::from_secs(config.backoff_max_secs),
            redrive_after: Duration::from_secs(config.redrive_after_secs),
        }
    }

    /// Delay before retry number `attempt` (1-based):
    /// `min(max, min · 2^(attempt-1))` scaled by `1 + jitter`, clamped to
    /// `[backoff_min, backoff_max]`. A valid `Retry-After` replaces the
    /// exponential value and is clamped to the same range, so a receiver's
    /// `Retry-After: 0` cannot make the worker spin (§9: 1~300 s).
    pub fn delay(&self, attempt: u32, retry_after: Option<Duration>, jitter: f64) -> Duration {
        if let Some(requested) = retry_after {
            return requested.clamp(self.backoff_min, self.backoff_max);
        }
        let doublings = attempt.saturating_sub(1).min(31);
        let exponential = self
            .backoff_min
            .saturating_mul(1u32 << doublings)
            .min(self.backoff_max);
        let jitter = jitter.clamp(-JITTER, JITTER);
        exponential
            .mul_f64(1.0 + jitter)
            .clamp(self.backoff_min, self.backoff_max)
    }
}

fn stamp(time: SystemTime) -> String {
    clock::format_rfc3339_ms(time)
}

impl Notification {
    /// When the worker should next attempt this row, if ever on its own:
    /// `next_due_at`, else `redrive_due_at`, only while `pending`. A stored
    /// timestamp that does not parse counts as due now; the attempt that
    /// follows rewrites it.
    pub fn due_at(&self) -> Option<SystemTime> {
        if self.state != StoredState::Pending {
            return None;
        }
        due_time(self.next_due_at.as_deref(), self.redrive_due_at.as_deref())
    }

    /// Row "새 inbox row": coalesce into the head; schedule now unless a
    /// deadline already exists (a backoff deadline is never reset).
    pub fn on_new_inbox_row(&mut self, cursor: InboxCursor, now: SystemTime) {
        self.target_seq = cursor.inbox_head;
        if self.state == StoredState::Blocked {
            return;
        }
        self.state = StoredState::Pending;
        if self.next_due_at.is_none() {
            self.next_due_at = Some(stamp(now));
        }
    }

    /// Row "read marker PUT": a marker at the head means nothing is left to
    /// announce, even when blocked.
    pub fn on_read_marker(&mut self, cursor: InboxCursor) {
        if !cursor.has_unread() {
            self.become_idle();
        }
    }

    /// Row "receiver PATCH로 URL 변경 / rotate": adopt the new route, forget
    /// the old route's failures and, with a URL and unread rows, schedule
    /// now. A blocked route is released.
    pub fn on_route_change(
        &mut self,
        route_version: i64,
        receiver_url_present: bool,
        cursor: InboxCursor,
        now: SystemTime,
    ) {
        self.route_version = route_version;
        self.attempt = 0;
        self.last_error = None;
        if !receiver_url_present {
            self.next_due_at = None;
            return;
        }
        if !cursor.has_unread() {
            self.become_idle();
            return;
        }
        self.state = StoredState::Pending;
        self.target_seq = cursor.inbox_head;
        self.next_due_at = Some(stamp(now));
    }

    /// Rows "`POST delivery/retry`". Returns the displayed state afterwards.
    pub fn manual_retry(
        &mut self,
        receiver_url_present: bool,
        cursor: InboxCursor,
        now: SystemTime,
    ) -> DeliveryState {
        if !receiver_url_present {
            return DeliveryState::NoReceiver;
        }
        if !cursor.has_unread() {
            self.become_idle();
            return DeliveryState::Idle;
        }
        self.target_seq = cursor.inbox_head;
        if self.state != StoredState::Pending {
            // Blocked (or an idle row that somehow has unread rows): release
            // and schedule now. A pending row keeps its due time.
            self.state = StoredState::Pending;
            self.attempt = 0;
            self.next_due_at = Some(stamp(now));
        }
        DeliveryState::Pending
    }

    /// Rows "POST 시작" and "`redrive_due_at` 도달". The caller has checked
    /// that a URL is set and that no attempt is in flight. Returns the
    /// `posted_seq` when an attempt should start now.
    pub fn begin_attempt(&mut self, cursor: InboxCursor, now: SystemTime) -> Option<Seq> {
        if self.state != StoredState::Pending {
            return None;
        }
        if !cursor.has_unread() {
            self.become_idle();
            return None;
        }
        if self.due_at()? > now {
            return None;
        }
        if self.next_due_at.is_none() {
            // Only `redrive_due_at` made this due.
            self.target_seq = cursor.inbox_head;
            self.next_due_at = Some(stamp(now));
        }
        self.last_attempt_at = Some(stamp(now));
        Some(self.target_seq)
    }

    /// Rows "응답 도착": the outcome of the attempt described by `posted`.
    pub fn apply_outcome(
        &mut self,
        posted: Posted,
        outcome: &AttemptOutcome,
        cursor: InboxCursor,
        now: SystemTime,
        policy: &RetryPolicy,
        jitter: f64,
    ) -> Applied {
        if self.route_version != posted.route_version {
            return Applied::Ignored;
        }
        let posted_seq = posted.posted_seq;
        match outcome {
            AttemptOutcome::Accepted => {
                self.last_accepted_seq = Some(posted_seq);
                self.attempt = 0;
                self.last_error = None;
                self.redrive_due_at = Some(stamp(now + policy.redrive_after));
                self.state = StoredState::Pending;
                if cursor.inbox_head > posted_seq {
                    self.target_seq = cursor.inbox_head;
                    self.next_due_at = Some(stamp(now));
                } else {
                    self.next_due_at = None;
                }
            }
            AttemptOutcome::Transient { error, retry_after } => {
                self.attempt = self.attempt.saturating_add(1);
                let delay = policy.delay(self.attempt, *retry_after, jitter);
                self.next_due_at = Some(stamp(now + delay));
                self.last_error = Some(error.clone());
                self.state = StoredState::Pending;
            }
            AttemptOutcome::Blocked { error } => {
                self.state = StoredState::Blocked;
                self.next_due_at = None;
                self.redrive_due_at = None;
                self.last_error = Some(error.clone());
            }
        }
        if !cursor.has_unread() {
            self.become_idle();
        }
        Applied::Recorded(self.state)
    }

    fn become_idle(&mut self) {
        self.state = StoredState::Idle;
        self.next_due_at = None;
        self.redrive_due_at = None;
        self.attempt = 0;
    }
}

/// The due time of a pending row from its two stored deadlines.
pub fn due_time(next_due_at: Option<&str>, redrive_due_at: Option<&str>) -> Option<SystemTime> {
    let deadline = next_due_at.or(redrive_due_at)?;
    Some(clock::parse_rfc3339_ms(deadline).unwrap_or(SystemTime::UNIX_EPOCH))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    fn seq(value: i64) -> Seq {
        Seq::new(value).unwrap()
    }

    fn cursor(inbox_head: i64, read_marker: i64) -> InboxCursor {
        InboxCursor {
            inbox_head: seq(inbox_head),
            read_marker: seq(read_marker),
        }
    }

    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + secs)
    }

    fn idle() -> Notification {
        Notification {
            route_version: 1,
            state: StoredState::Idle,
            target_seq: Seq::ZERO,
            last_accepted_seq: None,
            attempt: 0,
            next_due_at: None,
            redrive_due_at: None,
            last_attempt_at: None,
            last_error: None,
        }
    }

    fn posted(route_version: i64, posted_seq: i64) -> Posted {
        Posted {
            route_version,
            posted_seq: seq(posted_seq),
        }
    }

    fn policy() -> RetryPolicy {
        RetryPolicy {
            backoff_min: SECOND,
            backoff_max: 8 * SECOND,
            redrive_after: 30 * SECOND,
        }
    }

    fn transient(error: &str) -> AttemptOutcome {
        AttemptOutcome::Transient {
            error: error.into(),
            retry_after: None,
        }
    }

    fn blocked(error: &str) -> AttemptOutcome {
        AttemptOutcome::Blocked {
            error: error.into(),
        }
    }

    /// A pending row with one unread row, attempted at `t(0)`.
    fn in_flight(posted: i64) -> Notification {
        let mut row = idle();
        row.on_new_inbox_row(cursor(posted, 0), t(0));
        assert_eq!(
            row.begin_attempt(cursor(posted, 0), t(0)),
            Some(seq(posted))
        );
        row
    }

    #[test]
    fn new_row_schedules_now_and_targets_the_head() {
        let mut row = idle();
        row.on_new_inbox_row(cursor(3, 0), t(5));
        assert_eq!(row.state, StoredState::Pending);
        assert_eq!(row.target_seq, seq(3));
        assert_eq!(row.next_due_at, Some(stamp(t(5))));
        assert_eq!(row.due_at(), Some(t(5)));
    }

    #[test]
    fn new_row_keeps_an_existing_deadline() {
        let mut row = in_flight(1);
        row.apply_outcome(
            posted(1, 1),
            &transient("http_503"),
            cursor(1, 0),
            t(0),
            &policy(),
            0.0,
        );
        let deadline = row.next_due_at.clone();
        assert_eq!(deadline, Some(stamp(t(1))));

        row.on_new_inbox_row(cursor(2, 0), t(0));
        assert_eq!(row.next_due_at, deadline, "backoff deadline is not reset");
        assert_eq!(row.target_seq, seq(2), "target coalesces into the head");
    }

    #[test]
    fn new_row_while_blocked_only_moves_the_target() {
        let mut row = in_flight(1);
        row.apply_outcome(
            posted(1, 1),
            &blocked("http_404"),
            cursor(1, 0),
            t(0),
            &policy(),
            0.0,
        );
        let before = row.clone();

        row.on_new_inbox_row(cursor(2, 0), t(1));
        assert_eq!(row.state, StoredState::Blocked);
        assert_eq!(row.target_seq, seq(2));
        assert_eq!(
            Notification {
                target_seq: before.target_seq,
                ..row.clone()
            },
            before
        );
        assert_eq!(row.due_at(), None);
    }

    #[test]
    fn begin_attempt_records_the_time_and_posts_the_target() {
        let mut row = idle();
        row.on_new_inbox_row(cursor(2, 0), t(0));
        assert_eq!(row.begin_attempt(cursor(2, 0), t(0)), Some(seq(2)));
        assert_eq!(row.last_attempt_at, Some(stamp(t(0))));
    }

    #[test]
    fn begin_attempt_waits_for_the_deadline() {
        let mut row = in_flight(1);
        row.apply_outcome(
            posted(1, 1),
            &transient("timeout"),
            cursor(1, 0),
            t(0),
            &policy(),
            0.0,
        );
        let before = row.clone();
        assert_eq!(row.begin_attempt(cursor(1, 0), t(0)), None);
        assert_eq!(row, before);
        assert_eq!(row.begin_attempt(cursor(1, 0), t(1)), Some(seq(1)));
    }

    #[test]
    fn begin_attempt_idles_a_row_whose_marker_reached_the_head() {
        let mut row = idle();
        row.on_new_inbox_row(cursor(1, 0), t(0));
        assert_eq!(row.begin_attempt(cursor(1, 1), t(0)), None);
        assert_eq!(row.state, StoredState::Idle);
        assert_eq!(row.next_due_at, None);
    }

    #[test]
    fn accepted_at_head_waits_for_redrive() {
        let mut row = in_flight(1);
        let applied = row.apply_outcome(
            posted(1, 1),
            &AttemptOutcome::Accepted,
            cursor(1, 0),
            t(2),
            &policy(),
            0.0,
        );
        assert_eq!(applied, Applied::Recorded(StoredState::Pending));
        assert_eq!(row.last_accepted_seq, Some(seq(1)));
        assert_eq!(row.attempt, 0);
        assert_eq!(row.last_error, None);
        assert_eq!(row.next_due_at, None);
        assert_eq!(row.redrive_due_at, Some(stamp(t(32))));
        assert_eq!(row.due_at(), Some(t(32)));
    }

    #[test]
    fn accepted_behind_the_head_posts_the_head_next() {
        let mut row = in_flight(42);
        row.on_new_inbox_row(cursor(43, 0), t(1));
        row.apply_outcome(
            posted(1, 42),
            &AttemptOutcome::Accepted,
            cursor(43, 0),
            t(2),
            &policy(),
            0.0,
        );
        assert_eq!(row.last_accepted_seq, Some(seq(42)), "42's 202 is not 43's");
        assert_eq!(row.target_seq, seq(43));
        assert_eq!(row.next_due_at, Some(stamp(t(2))));
        assert_eq!(row.begin_attempt(cursor(43, 0), t(2)), Some(seq(43)));
    }

    #[test]
    fn accepted_after_the_marker_caught_up_is_idle() {
        let mut row = in_flight(1);
        row.apply_outcome(
            posted(1, 1),
            &AttemptOutcome::Accepted,
            cursor(1, 1),
            t(1),
            &policy(),
            0.0,
        );
        assert_eq!(row.state, StoredState::Idle);
        assert_eq!(row.last_accepted_seq, Some(seq(1)));
        assert_eq!(
            (row.next_due_at.clone(), row.redrive_due_at.clone()),
            (None, None)
        );
    }

    #[test]
    fn redrive_reposts_the_current_head() {
        let mut row = in_flight(1);
        row.apply_outcome(
            posted(1, 1),
            &AttemptOutcome::Accepted,
            cursor(1, 0),
            t(0),
            &policy(),
            0.0,
        );
        assert_eq!(row.begin_attempt(cursor(1, 0), t(29)), None);
        assert_eq!(row.begin_attempt(cursor(1, 0), t(30)), Some(seq(1)));
        assert_eq!(row.next_due_at, Some(stamp(t(30))));
    }

    #[test]
    fn transient_failures_back_off_exponentially_with_codes() {
        let mut row = in_flight(1);
        let mut deadlines = Vec::new();
        for (attempt, code) in [
            (1, "connect"),
            (2, "timeout"),
            (3, "http_503"),
            (4, "http_429"),
            (5, "network"),
        ] {
            let now = t(100 * attempt);
            row.apply_outcome(
                posted(1, 1),
                &transient(code),
                cursor(1, 0),
                now,
                &policy(),
                0.0,
            );
            assert_eq!(row.state, StoredState::Pending);
            assert_eq!(row.attempt, attempt as u32);
            assert_eq!(row.last_error.as_deref(), Some(code));
            deadlines.push(row.due_at().unwrap().duration_since(now).unwrap());
        }
        assert_eq!(deadlines, [1, 2, 4, 8, 8].map(|s| s * SECOND));
    }

    #[test]
    fn retry_after_replaces_the_exponential_delay_within_bounds() {
        let mut row = in_flight(1);
        let outcome = AttemptOutcome::Transient {
            error: "http_429".into(),
            retry_after: Some(5 * SECOND),
        };
        row.apply_outcome(posted(1, 1), &outcome, cursor(1, 0), t(0), &policy(), 0.5);
        assert_eq!(row.due_at(), Some(t(5)));

        let huge = AttemptOutcome::Transient {
            error: "http_503".into(),
            retry_after: Some(3600 * SECOND),
        };
        row.apply_outcome(posted(1, 1), &huge, cursor(1, 0), t(0), &policy(), 0.0);
        assert_eq!(row.due_at(), Some(t(8)), "capped at backoff_max");
    }

    #[test]
    fn blocked_stops_attempts() {
        for code in ["http_404", "http_301", "http_200", "protocol_mismatch"] {
            let mut row = in_flight(1);
            let applied = row.apply_outcome(
                posted(1, 1),
                &blocked(code),
                cursor(1, 0),
                t(0),
                &policy(),
                0.0,
            );
            assert_eq!(applied, Applied::Recorded(StoredState::Blocked));
            assert_eq!(row.last_error.as_deref(), Some(code));
            assert_eq!(
                (row.next_due_at.clone(), row.redrive_due_at.clone()),
                (None, None)
            );
            assert_eq!(row.due_at(), None);
            assert_eq!(row.begin_attempt(cursor(1, 0), t(1000)), None);
        }
    }

    #[test]
    fn responses_for_an_older_route_are_ignored() {
        for outcome in [
            AttemptOutcome::Accepted,
            blocked("http_401"),
            transient("timeout"),
        ] {
            let mut row = in_flight(1);
            row.on_route_change(2, true, cursor(1, 0), t(1));
            let before = row.clone();
            let applied =
                row.apply_outcome(posted(1, 1), &outcome, cursor(1, 0), t(2), &policy(), 0.0);
            assert_eq!(applied, Applied::Ignored, "{outcome:?}");
            assert_eq!(row, before);
        }
    }

    #[test]
    fn read_marker_at_head_idles_even_a_blocked_row() {
        let mut row = in_flight(2);
        row.apply_outcome(
            posted(1, 2),
            &blocked("http_404"),
            cursor(2, 0),
            t(0),
            &policy(),
            0.0,
        );

        row.on_read_marker(cursor(2, 1));
        assert_eq!(
            row.state,
            StoredState::Blocked,
            "below the head nothing changes"
        );

        row.on_read_marker(cursor(2, 2));
        assert_eq!(row.state, StoredState::Idle);
        assert_eq!(row.attempt, 0);
        assert_eq!(
            (row.next_due_at.clone(), row.redrive_due_at.clone()),
            (None, None)
        );
    }

    #[test]
    fn route_change_releases_a_blocked_route_and_schedules_now() {
        let mut row = in_flight(1);
        row.apply_outcome(
            posted(1, 1),
            &blocked("http_401"),
            cursor(1, 0),
            t(0),
            &policy(),
            0.0,
        );
        row.on_new_inbox_row(cursor(3, 0), t(1));

        row.on_route_change(2, true, cursor(3, 0), t(2));
        assert_eq!(row.route_version, 2);
        assert_eq!(row.state, StoredState::Pending);
        assert_eq!(row.target_seq, seq(3));
        assert_eq!(row.attempt, 0);
        assert_eq!(row.last_error, None);
        assert_eq!(row.due_at(), Some(t(2)));
    }

    #[test]
    fn route_change_to_null_unschedules_but_keeps_the_state() {
        let mut row = in_flight(1);
        row.apply_outcome(
            posted(1, 1),
            &transient("http_503"),
            cursor(1, 0),
            t(0),
            &policy(),
            0.0,
        );

        row.on_route_change(2, false, cursor(1, 0), t(1));
        assert_eq!(row.state, StoredState::Pending);
        assert_eq!(row.next_due_at, None);
        assert_eq!(display_state(false, row.state), DeliveryState::NoReceiver);
    }

    #[test]
    fn route_change_with_nothing_unread_is_idle() {
        let mut row = idle();
        row.on_route_change(2, true, cursor(0, 0), t(0));
        assert_eq!(row.state, StoredState::Idle);
        assert_eq!(row.due_at(), None);
    }

    #[test]
    fn manual_retry_follows_the_table() {
        let mut none = idle();
        none.on_new_inbox_row(cursor(1, 0), t(0));
        let before = none.clone();
        assert_eq!(
            none.manual_retry(false, cursor(1, 0), t(1)),
            DeliveryState::NoReceiver
        );
        assert_eq!(none, before, "no receiver: no change");

        let mut caught_up = in_flight(1);
        assert_eq!(
            caught_up.manual_retry(true, cursor(1, 1), t(1)),
            DeliveryState::Idle
        );
        assert_eq!(caught_up.state, StoredState::Idle);

        let mut stuck = in_flight(1);
        stuck.apply_outcome(
            posted(1, 1),
            &blocked("http_404"),
            cursor(1, 0),
            t(0),
            &policy(),
            0.0,
        );
        assert_eq!(
            stuck.manual_retry(true, cursor(2, 0), t(9)),
            DeliveryState::Pending
        );
        assert_eq!(stuck.state, StoredState::Pending);
        assert_eq!(stuck.attempt, 0);
        assert_eq!(stuck.target_seq, seq(2));
        assert_eq!(stuck.due_at(), Some(t(9)), "released and due now");

        let mut backing_off = in_flight(1);
        backing_off.apply_outcome(
            posted(1, 1),
            &transient("http_503"),
            cursor(1, 0),
            t(0),
            &policy(),
            0.0,
        );
        let due = backing_off.due_at();
        assert_eq!(
            backing_off.manual_retry(true, cursor(2, 0), t(0)),
            DeliveryState::Pending
        );
        assert_eq!(backing_off.due_at(), due, "pending keeps its due time");
        assert_eq!(backing_off.attempt, 1);
        assert_eq!(backing_off.target_seq, seq(2));
    }

    #[test]
    fn jittered_backoff_stays_within_bounds() {
        let policy = policy();
        for attempt in [1, 2, 3, 4, 10, 40, u32::MAX] {
            for jitter in [-0.5, -0.25, 0.0, 0.25, 0.5, -9.0, 9.0] {
                let delay = policy.delay(attempt, None, jitter);
                assert!(
                    (policy.backoff_min..=policy.backoff_max).contains(&delay),
                    "attempt {attempt} jitter {jitter}: {delay:?}"
                );
            }
        }
        assert_eq!(policy.delay(2, None, 0.5), 3 * SECOND);
        assert_eq!(policy.delay(2, None, -0.5), SECOND);
        assert_eq!(
            policy.delay(1, Some(Duration::ZERO), 0.0),
            SECOND,
            "Retry-After: 0 is floored"
        );
    }

    #[test]
    fn stored_state_round_trips() {
        for state in [
            StoredState::Idle,
            StoredState::Pending,
            StoredState::Blocked,
        ] {
            assert_eq!(StoredState::parse(state.as_str()), Some(state));
        }
        assert_eq!(StoredState::parse("no_receiver"), None);
    }

    #[test]
    fn due_time_prefers_next_due_and_treats_garbage_as_due() {
        assert_eq!(due_time(None, None), None);
        assert_eq!(due_time(Some(&stamp(t(1))), Some(&stamp(t(9)))), Some(t(1)));
        assert_eq!(due_time(None, Some(&stamp(t(9)))), Some(t(9)));
        assert_eq!(
            due_time(Some("garbage"), None),
            Some(SystemTime::UNIX_EPOCH)
        );
    }
}
