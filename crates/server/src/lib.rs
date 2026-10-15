//! `platformd`: HTTP API over a single SQLite DB thread.
//!
//! The binary (`main.rs`) parses arguments, loads [`config::Config`] and calls
//! [`start`] then [`Server::serve`]. Integration tests drive the same path
//! in-process.

pub mod api;
pub mod clock;
pub mod config;
pub mod store;

use std::future::{Future, IntoFuture};
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tracing::{info, warn};

use crate::config::Config;
use crate::store::{Store, StoreError, StoreThread, migrations};

/// Upper bound for in-flight requests to finish after a shutdown signal (`IF-OPS-001`).
pub const DRAIN_LIMIT: Duration = Duration::from_secs(10);

/// Upper bound for the DB thread to finish queued commands and close the connection.
pub const STORE_JOIN_LIMIT: Duration = Duration::from_secs(5);

/// The `--version` line: package version and git short hash (`IF-OPS-001`).
pub const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("PLATFORMD_GIT_SHA"),
    ")"
);

#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("cannot create database directory {path}: {source}")]
    DatabaseDir { path: String, source: io::Error },
    #[error("database: {0}")]
    Store(#[from] StoreError),
    #[error("cannot listen on {addr}: {source}")]
    Bind { addr: SocketAddr, source: io::Error },
}

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("http server: {0}")]
    Http(#[from] io::Error),
    #[error("database shutdown: {0}")]
    Store(#[from] StoreError),
}

/// A server whose database is open and migrated and whose listener is bound.
/// Readiness is listen: nothing is bound until migrations succeed.
pub struct Server {
    listener: TcpListener,
    router: axum::Router,
    store_thread: StoreThread,
}

/// Opens the database, applies migrations, then binds the listener.
pub async fn start(config: &Config) -> Result<Server, StartupError> {
    ensure_parent_dir(&config.db_path)?;

    let (store, store_thread) = Store::open(&config.db_path)?;
    let schema_version = store.run(migrations::apply).await?;
    info!(
        db_path = %config.db_path.display(),
        schema_version,
        "database ready"
    );

    let listener = TcpListener::bind(config.listen)
        .await
        .map_err(|source| StartupError::Bind {
            addr: config.listen,
            source,
        })?;
    let router = api::router(api::AppState::new(store));

    Ok(Server {
        listener,
        router,
        store_thread,
    })
}

impl Server {
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Serves until `shutdown` resolves, then closes the listener, waits up to
    /// [`DRAIN_LIMIT`] for in-flight requests and joins the DB thread.
    pub async fn serve<S>(self, shutdown: S) -> Result<(), ServeError>
    where
        S: Future<Output = ()> + Send + 'static,
    {
        let Server {
            listener,
            router,
            store_thread,
        } = self;
        if let Ok(addr) = listener.local_addr() {
            info!(%addr, "listening");
        }

        // The router owns the last `Store` clones; it must be dropped (with the
        // serve future) before the DB thread can observe the closed queue.
        let http_result = serve_http(listener, router, shutdown).await;

        let store_result = store_thread.shutdown(STORE_JOIN_LIMIT).await;
        http_result?;
        store_result?;
        info!("shutdown complete");
        Ok(())
    }
}

async fn serve_http<S>(listener: TcpListener, router: axum::Router, shutdown: S) -> io::Result<()>
where
    S: Future<Output = ()> + Send + 'static,
{
    let (drain_tx, drain_rx) = oneshot::channel::<()>();
    let serve = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            // A dropped sender also means "stop": either way we drain.
            let _ = drain_rx.await;
        })
        .into_future();
    tokio::pin!(serve);

    let finished_early = tokio::select! {
        result = &mut serve => Some(result),
        () = shutdown => None,
    };
    if let Some(result) = finished_early {
        return result;
    }

    info!("shutdown signal received; listener closed, draining in-flight requests");
    let _ = drain_tx.send(());
    match tokio::time::timeout(DRAIN_LIMIT, &mut serve).await {
        Ok(result) => result,
        Err(_) => {
            warn!(limit = ?DRAIN_LIMIT, "in-flight requests did not finish in time; dropping them");
            Ok(())
        }
    }
}

fn ensure_parent_dir(db_path: &std::path::Path) -> Result<(), StartupError> {
    let Some(parent) = db_path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(parent).map_err(|source| StartupError::DatabaseDir {
        path: parent.display().to_string(),
        source,
    })
}
