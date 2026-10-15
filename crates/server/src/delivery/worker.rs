//! The delivery worker (`IF-DLV-001`, `IF-RCV-RETRY`).
//!
//! One task waits for the commit signal, the earliest due time or an
//! attempt finishing, then reads the pending agents and starts an attempt
//! for every due agent that has none in flight (at most
//! [`MAX_CONCURRENT_ATTEMPTS`] at once). The in-flight set lives only in
//! memory; after a restart nothing is in flight and the due times come from
//! the database.
//!
//! An attempt is: `begin_attempt` transaction → POST (no transaction open) →
//! `apply_outcome` transaction. Its end wakes the loop, which re-evaluates.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use platform_wire::AgentId;
use rand::RngExt as _;
use tokio::sync::oneshot;
use tokio::task::{self, JoinError, JoinHandle, JoinSet};
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

use super::DeliverySignal;
use super::classify::AttemptOutcome;
use super::client::{CallbackClient, log_host};
use super::transitions::{Applied, JITTER, RetryPolicy};
use crate::config::DeliveryConfig;
use crate::store::queries::delivery::{self as delivery_queries, Attempt};
use crate::store::{Store, StoreError};

/// Upper bound of attempts in flight across all agents.
pub const MAX_CONCURRENT_ATTEMPTS: usize = 16;

/// Pause after a database error, so a failing store does not spin the loop
/// or re-POST an agent whose outcome could not be recorded.
const STORE_ERROR_PAUSE: Duration = Duration::from_secs(1);

/// The running worker. [`WorkerHandle::stop`] ends it; dropping the handle
/// aborts it.
pub struct WorkerHandle {
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl WorkerHandle {
    /// Stops the loop and aborts attempts in flight without waiting for
    /// their HTTP requests (`IF-OPS-001`): their state is in the database.
    pub async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let Some(task) = self.task.take() else {
            return;
        };
        if let Err(err) = task.await {
            error!(error = %err, "delivery worker ended abnormally");
        }
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

pub struct Worker {
    store: Store,
    signal: DeliverySignal,
    client: Arc<CallbackClient>,
    policy: RetryPolicy,
}

impl Worker {
    pub fn spawn(
        store: Store,
        signal: DeliverySignal,
        client: CallbackClient,
        config: &DeliveryConfig,
    ) -> WorkerHandle {
        let worker = Worker {
            store,
            signal,
            client: Arc::new(client),
            policy: RetryPolicy::from_config(config),
        };
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(worker.run(stopped));
        WorkerHandle {
            stop: Some(stop),
            task: Some(task),
        }
    }

    async fn run(self, stopped: oneshot::Receiver<()>) {
        info!("delivery worker started");
        let mut in_flight = InFlight::default();
        tokio::select! {
            () = self.serve(&mut in_flight) => {}
            // A dropped sender also means stop.
            _ = stopped => {}
        }
        in_flight.attempts.shutdown().await;
        info!("delivery worker stopped");
    }

    async fn serve(&self, in_flight: &mut InFlight) {
        loop {
            let wakeup = match self.dispatch(in_flight).await {
                Ok(wakeup) => wakeup,
                Err(err) => {
                    warn!(error = %err, "delivery worker cannot read the schedule");
                    Some(Instant::now() + STORE_ERROR_PAUSE)
                }
            };
            tokio::select! {
                Some(finished) = in_flight.attempts.join_next_with_id() => in_flight.finish(finished),
                () = self.signal.notified() => {}
                () = sleep_until(wakeup) => {}
            }
        }
    }

    /// Starts every due attempt that fits and returns when the earliest
    /// not-yet-due agent becomes due (`None`: wait for a signal).
    async fn dispatch(&self, in_flight: &mut InFlight) -> Result<Option<Instant>, StoreError> {
        let schedule = self
            .store
            .run(|conn| delivery_queries::schedule(conn))
            .await?;
        let now = SystemTime::now();
        let mut earliest: Option<SystemTime> = None;
        for entry in schedule {
            if in_flight.contains(&entry.agent_id) {
                continue;
            }
            let Some(due_at) = entry.due_at else {
                continue;
            };
            if due_at > now {
                earliest = Some(earliest.map_or(due_at, |current| current.min(due_at)));
                continue;
            }
            if in_flight.len() >= MAX_CONCURRENT_ATTEMPTS {
                // Picked up when a running attempt finishes.
                continue;
            }
            let attempt = run_attempt(
                self.store.clone(),
                Arc::clone(&self.client),
                self.policy,
                entry.agent_id.clone(),
            );
            in_flight.start(entry.agent_id, attempt);
        }
        Ok(earliest.map(instant_at))
    }
}

/// Attempts in flight, keyed by task so a finished (or panicked) task
/// releases its agent.
#[derive(Default)]
struct InFlight {
    attempts: JoinSet<()>,
    agents: HashMap<task::Id, AgentId>,
}

impl InFlight {
    fn contains(&self, agent_id: &AgentId) -> bool {
        self.agents.values().any(|running| running == agent_id)
    }

    fn len(&self) -> usize {
        self.agents.len()
    }

    fn start(&mut self, agent_id: AgentId, attempt: impl Future<Output = ()> + Send + 'static) {
        let handle = self.attempts.spawn(attempt);
        self.agents.insert(handle.id(), agent_id);
    }

    fn finish(&mut self, finished: Result<(task::Id, ()), JoinError>) {
        let id = match finished {
            Ok((id, ())) => id,
            Err(err) => {
                error!(error = %err, "delivery attempt task failed");
                err.id()
            }
        };
        self.agents.remove(&id);
    }
}

/// One attempt for `agent_id`, from the due check to the recorded outcome.
async fn run_attempt(
    store: Store,
    client: Arc<CallbackClient>,
    policy: RetryPolicy,
    agent_id: AgentId,
) {
    let now = SystemTime::now();
    let target = agent_id.clone();
    let begun = store
        .transaction(move |tx| delivery_queries::begin_attempt(tx, &target, now))
        .await;
    let attempt = match begun {
        Ok(Some(attempt)) => attempt,
        Ok(None) => return,
        Err(err) => {
            warn!(agent_id = %agent_id, error = %err, "cannot start a delivery attempt");
            tokio::time::sleep(STORE_ERROR_PAUSE).await;
            return;
        }
    };

    let outcome = client.notify(&attempt).await;
    let jitter = rand::rng().random_range(-JITTER..=JITTER);
    let now = SystemTime::now();
    let recorded = {
        let attempt = attempt.clone();
        let outcome = outcome.clone();
        store
            .transaction(move |tx| {
                delivery_queries::apply_outcome(tx, &attempt, &outcome, now, &policy, jitter)
            })
            .await
    };
    match recorded {
        Ok(applied) => log_outcome(&attempt, &outcome, applied),
        Err(err) => {
            warn!(agent_id = %agent_id, error = %err, "cannot record a delivery outcome");
            tokio::time::sleep(STORE_ERROR_PAUSE).await;
        }
    }
}

fn log_outcome(attempt: &Attempt, outcome: &AttemptOutcome, applied: Applied) {
    let agent_id = &attempt.agent_id;
    let host = log_host(&attempt.receiver_url);
    let inbox_seq = attempt.posted.posted_seq;
    let route_version = attempt.posted.route_version;
    let Applied::Recorded(state) = applied else {
        debug!(%agent_id, %host, %inbox_seq, route_version, error = outcome.error_code(),
            "response for an older route ignored");
        return;
    };
    let state = state.as_str();
    match outcome {
        AttemptOutcome::Accepted => {
            info!(%agent_id, %host, %inbox_seq, route_version, %state, "receiver accepted the wake");
        }
        AttemptOutcome::Transient { error, .. } => {
            warn!(%agent_id, %host, %inbox_seq, route_version, error = %error, %state,
                "wake failed; will retry");
        }
        AttemptOutcome::Blocked { error } => {
            warn!(%agent_id, %host, %inbox_seq, route_version, error = %error, %state,
                "wake rejected; route blocked until it changes or a manual retry");
        }
    }
}

fn instant_at(at: SystemTime) -> Instant {
    let wait = at
        .duration_since(SystemTime::now())
        .unwrap_or(Duration::ZERO);
    Instant::now() + wait
}

async fn sleep_until(wakeup: Option<Instant>) {
    match wakeup {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}
