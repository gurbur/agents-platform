//! Shared fixtures: a temp directory holding a config file and token file,
//! an in-process server bound to an ephemeral port, and a mock receiver.

#![allow(dead_code)] // Each test binary uses a different subset.

pub mod receiver;

use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use platformd::config::Config;
use platformd::delivery::DeliverySignal;
use platformd::store::{Store, StoreError};
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

    /// Like [`Fixture::load_config`] with a `[delivery]` table holding
    /// `delivery` (TOML `key = value` lines).
    pub fn load_config_with_delivery(&self, delivery: &str) -> Config {
        let path = self.write_config_text(&format!(
            "listen = \"127.0.0.1:0\"\n\
             db_path = \"data/platform.db\"\n\
             owner_token_file = \"owner.token\"\n\
             [delivery]\n{delivery}\n"
        ));
        Config::load(&path).expect("valid config")
    }

    /// Like [`Fixture::load_config`] with an `[identity]` table holding
    /// `identity` (TOML `key = value` lines; `signing_key_file` is added) and
    /// a signing key generated into `identity.key`.
    pub fn load_config_with_identity(&self, identity: &str) -> Config {
        let key = self.path().join("identity.key");
        if !key.exists() {
            platformd::identity::keygen::generate(&key).expect("generate identity key");
        }
        let path = self.write_config_text(&format!(
            "listen = \"127.0.0.1:0\"\n\
             db_path = \"data/platform.db\"\n\
             owner_token_file = \"owner.token\"\n\
             [identity]\n\
             signing_key_file = \"identity.key\"\n{identity}\n"
        ));
        Config::load(&path).expect("valid config")
    }

    /// Runs `f` on a second connection to the database. WAL lets it read
    /// while a server is running; the connection is closed before returning.
    pub async fn read_db<T, F>(&self, f: F) -> T
    where
        F: FnOnce(&mut rusqlite::Connection) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        let (store, thread) = Store::open(&self.db_path()).expect("open database");
        let value = store.run(f).await.expect("database read");
        drop(store);
        thread
            .shutdown(std::time::Duration::from_secs(5))
            .await
            .expect("database closes");
        value
    }
}

pub struct RunningServer {
    pub addr: SocketAddr,
    /// The server's delivery signal.
    pub delivery: DeliverySignal,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), platformd::ServeError>>>,
}

impl RunningServer {
    pub async fn start(config: &Config) -> Self {
        let server = platformd::start(config).await.expect("server starts");
        let addr = server.local_addr().expect("local addr");
        let delivery = server.delivery_signal();
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(server.serve(async move {
            let _ = stopped.await;
        }));
        Self {
            addr,
            delivery,
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

/// Polls `probe` every few milliseconds until it returns `Some`; panics with
/// `what` after `limit`. Bounds a wait without asserting how fast it is.
pub async fn eventually<T, F, Fut>(limit: Duration, what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let started = Instant::now();
    loop {
        if let Some(value) = probe().await {
            return value;
        }
        assert!(
            started.elapsed() < limit,
            "timed out after {limit:?}: {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A JSON HTTP client bound to one running server.
pub struct Api {
    client: reqwest::Client,
    base: String,
}

/// Status, headers and parsed JSON body (`Value::Null` when the body is empty).
pub struct Reply {
    pub status: reqwest::StatusCode,
    pub headers: reqwest::header::HeaderMap,
    pub body: serde_json::Value,
}

impl Reply {
    /// The `error.code` of an `IF-API-COMMON` error body.
    pub fn error_code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or_default()
    }

    pub fn error_field(&self) -> Option<&str> {
        self.body["error"]["details"]["field"].as_str()
    }

    /// Asserts status and error code, and that `request_id` matches the header.
    pub fn assert_error(&self, status: u16, code: &str) {
        assert_eq!(self.status.as_u16(), status, "body: {}", self.body);
        assert_eq!(self.error_code(), code, "body: {}", self.body);
        let header = self
            .headers
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert_eq!(self.body["error"]["request_id"], header);
        assert!(self.body["error"]["details"].is_object(), "{}", self.body);
    }
}

/// The display name [`Api::register`] gives `agent_id`.
pub fn default_name(agent_id: &str) -> String {
    format!("Agent {agent_id}")
}

/// The two one-time credentials and the registration body.
pub struct Registered {
    pub body: serde_json::Value,
    pub api_key: String,
    pub receiver_secret: String,
}

impl Api {
    pub async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        token: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> Reply {
        let bytes = body.map(|value| serde_json::to_vec(&value).expect("serialize body"));
        let content_type = bytes.as_ref().map(|_| "application/json");
        self.send_raw(method, path, token, content_type, bytes)
            .await
    }

    pub async fn send_raw(
        &self,
        method: reqwest::Method,
        path: &str,
        token: Option<&str>,
        content_type: Option<&str>,
        body: Option<Vec<u8>>,
    ) -> Reply {
        let mut request = self.client.request(method, format!("{}{path}", self.base));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(content_type) = content_type {
            request = request.header(reqwest::header::CONTENT_TYPE, content_type);
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        let response = request.send().await.expect("request sent");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.bytes().await.expect("response body");
        let body = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("JSON response body")
        };
        Reply {
            status,
            headers,
            body,
        }
    }

    pub async fn get(&self, path: &str, token: &str) -> Reply {
        self.send(reqwest::Method::GET, path, Some(token), None)
            .await
    }

    pub async fn post(&self, path: &str, token: &str, body: serde_json::Value) -> Reply {
        self.send(reqwest::Method::POST, path, Some(token), Some(body))
            .await
    }

    pub async fn patch(&self, path: &str, token: &str, body: serde_json::Value) -> Reply {
        self.send(reqwest::Method::PATCH, path, Some(token), Some(body))
            .await
    }

    pub async fn put(&self, path: &str, token: &str, body: serde_json::Value) -> Reply {
        self.send(reqwest::Method::PUT, path, Some(token), Some(body))
            .await
    }

    /// `POST /v1/agents/{id}/tokens/rotate` without a body.
    pub async fn rotate(&self, agent_id: &str, token: &str) -> Reply {
        let path = format!("/v1/agents/{agent_id}/tokens/rotate");
        self.send(reqwest::Method::POST, &path, Some(token), None)
            .await
    }

    /// `POST /v1/messages` with `reply_to` omitted.
    pub async fn send_message(&self, token: &str, to: &str, body: &str, client_key: &str) -> Reply {
        self.post(
            "/v1/messages",
            token,
            serde_json::json!({"to": to, "body": body, "client_key": client_key}),
        )
        .await
    }

    /// Sends and asserts 201; returns the message body.
    pub async fn sent(
        &self,
        token: &str,
        to: &str,
        body: &str,
        client_key: &str,
    ) -> serde_json::Value {
        let reply = self.send_message(token, to, body, client_key).await;
        assert_eq!(reply.status.as_u16(), 201, "body: {}", reply.body);
        reply.body
    }

    /// Registers as the owner with the name [`default_name`] and asserts 201.
    pub async fn register(&self, agent_id: &str, receiver_url: Option<&str>) -> Registered {
        self.register_named(agent_id, &default_name(agent_id), receiver_url)
            .await
    }

    /// Registers as the owner with `name` and asserts 201.
    pub async fn register_named(
        &self,
        agent_id: &str,
        name: &str,
        receiver_url: Option<&str>,
    ) -> Registered {
        let reply = self
            .post(
                "/v1/agents",
                OWNER_TOKEN,
                serde_json::json!({"agent_id": agent_id, "name": name, "receiver_url": receiver_url}),
            )
            .await;
        assert_eq!(reply.status.as_u16(), 201, "body: {}", reply.body);
        let api_key = reply.body["agent_api_key"]
            .as_str()
            .expect("agent_api_key")
            .to_owned();
        let receiver_secret = reply.body["receiver_secret"]
            .as_str()
            .expect("receiver_secret")
            .to_owned();
        Registered {
            body: reply.body,
            api_key,
            receiver_secret,
        }
    }
}

impl RunningServer {
    pub fn api(&self) -> Api {
        Api {
            client: reqwest::Client::new(),
            base: format!("http://{}", self.addr),
        }
    }
}
