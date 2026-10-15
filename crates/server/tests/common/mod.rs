//! Shared fixtures: a temp directory holding a config file and token file,
//! and an in-process server bound to an ephemeral port.

#![allow(dead_code)] // Each test binary uses a different subset.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use platformd::config::Config;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

pub const OWNER_TOKEN: &str = "test-owner-token-0123456789abcdef0123456789";

pub struct Fixture {
    pub dir: tempfile::TempDir,
}

impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("owner.token"), format!("{OWNER_TOKEN}\n"))
            .expect("write token");
        Self { dir }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn db_path(&self) -> PathBuf {
        self.path().join("data/platform.db")
    }

    /// Writes `platformd.toml` with relative paths and returns its path.
    pub fn write_config(&self, listen: &str) -> PathBuf {
        self.write_config_text(&format!(
            "listen = \"{listen}\"\n\
             db_path = \"data/platform.db\"\n\
             owner_token_file = \"owner.token\"\n"
        ))
    }

    pub fn write_config_text(&self, text: &str) -> PathBuf {
        let path = self.path().join("platformd.toml");
        std::fs::write(&path, text).expect("write config");
        path
    }

    pub fn load_config(&self) -> Config {
        Config::load(&self.write_config("127.0.0.1:0")).expect("valid config")
    }
}

pub struct RunningServer {
    pub addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), platformd::ServeError>>>,
}

impl RunningServer {
    pub async fn start(config: &Config) -> Self {
        let server = platformd::start(config).await.expect("server starts");
        let addr = server.local_addr().expect("local addr");
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(server.serve(async move {
            let _ = stopped.await;
        }));
        Self {
            addr,
            stop: Some(stop),
            task: Some(task),
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    /// Triggers graceful shutdown and waits for `serve` to return.
    pub async fn shutdown(mut self) -> Result<(), platformd::ServeError> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let task = self.task.take().expect("task present until shutdown");
        task.await.expect("serve task did not panic")
    }
}
