//! Single DB thread behind a bounded command queue (`IF-DB-002`).
//!
//! One OS thread owns the only `rusqlite::Connection`. [`Store`] handles send
//! closures to it; closures must not perform network I/O or await.

pub mod backup;
pub mod migrations;
pub mod queries;

use std::path::Path;
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::{Connection, Transaction, TransactionBehavior};
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info};

/// Command queue capacity. A constant rather than configuration (`IF-DB-002`).
pub const QUEUE_CAPACITY: usize = 256;

const THREAD_NAME: &str = "platformd-db";

type Job = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database thread is not running")]
    Closed,
    #[error("cannot start database thread: {0}")]
    Spawn(std::io::Error),
    #[error("journal_mode is '{0}', expected 'wal'")]
    JournalMode(String),
    #[error("PRAGMA {name} is {actual}, expected {expected}")]
    Pragma {
        name: &'static str,
        expected: i64,
        actual: i64,
    },
    #[error("database schema version {found} is newer than the supported version {supported}")]
    SchemaTooNew { found: u32, supported: u32 },
    #[error("schema migration {version} left {violations} foreign key violations; rolled back")]
    ForeignKeyViolations { version: u32, violations: usize },
    #[error("schema migration {version} failed its post-check ({detail}); rolled back")]
    PostCheck { version: u32, detail: String },
    #[error("schema_migrations is not contiguous from 1 (found {count} rows, max version {max})")]
    SchemaGap { count: u32, max: u32 },
    #[error("database thread did not stop within {0:?}")]
    ShutdownTimeout(Duration),
    #[error("database thread panicked")]
    ThreadPanicked,
    #[error("stored data violates an invariant: {0}")]
    Invariant(String),
}

/// Handle to the DB thread. Cheap to clone; the thread stops once every clone
/// is dropped and the queued commands are drained.
#[derive(Clone)]
pub struct Store {
    jobs: mpsc::Sender<Job>,
}

/// Owner of the DB thread's lifetime. Kept separate from [`Store`] so that
/// handlers can hold `Store` clones while exactly one place joins the thread.
pub struct StoreThread {
    handle: JoinHandle<()>,
    finished: oneshot::Receiver<()>,
}

impl Store {
    /// Opens `path` with the `IF-DB-001` PRAGMAs and starts the DB thread.
    /// Migrations are not applied here; run [`migrations::apply`] next.
    pub fn open(path: &Path) -> Result<(Store, StoreThread), StoreError> {
        let conn = migrations::open_connection(path)?;
        Self::spawn(conn)
    }

    fn spawn(conn: Connection) -> Result<(Store, StoreThread), StoreError> {
        let (jobs, queue) = mpsc::channel::<Job>(QUEUE_CAPACITY);
        let (finished_tx, finished) = oneshot::channel();
        let handle = std::thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn(move || {
                db_thread_main(conn, queue);
                // The receiver may already be gone if shutdown timed out.
                let _ = finished_tx.send(());
            })
            .map_err(StoreError::Spawn)?;
        Ok((Store { jobs }, StoreThread { handle, finished }))
    }

    /// Runs `f` on the DB thread and returns its result. Waits while the
    /// queue is full.
    pub async fn run<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        let job: Job = Box::new(move |conn| {
            // The caller may have been cancelled; nothing to do then.
            let _ = reply_tx.send(f(conn));
        });
        self.jobs.send(job).await.map_err(|_| StoreError::Closed)?;
        reply_rx.await.map_err(|_| StoreError::Closed)?
    }

    /// Like [`Store::run`] but wrapped in `BEGIN IMMEDIATE … COMMIT`.
    /// An error from `f` rolls the transaction back.
    pub async fn transaction<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        self.run(move |conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Dropping `tx` on the error path rolls back.
            let value = f(&tx)?;
            tx.commit()?;
            Ok(value)
        })
        .await
    }
}

impl StoreThread {
    /// Waits up to `limit` for the DB thread to drain its queue and close the
    /// connection. All [`Store`] clones must be dropped first.
    pub async fn shutdown(self, limit: Duration) -> Result<(), StoreError> {
        let StoreThread { handle, finished } = self;
        if tokio::time::timeout(limit, finished).await.is_err() {
            error!(?limit, "database thread did not stop in time");
            return Err(StoreError::ShutdownTimeout(limit));
        }
        // The thread has signalled completion (or unwound), so join returns promptly.
        handle.join().map_err(|_| StoreError::ThreadPanicked)?;
        info!("database closed");
        Ok(())
    }
}

fn db_thread_main(mut conn: Connection, mut queue: mpsc::Receiver<Job>) {
    while let Some(job) = queue.blocking_recv() {
        job(&mut conn);
    }
    if let Err((_, err)) = conn.close() {
        error!(error = %err, "closing the database connection failed");
    }
}
