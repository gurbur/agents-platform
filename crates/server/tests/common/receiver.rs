//! A programmable receiver (axum on an ephemeral port): scripted responses,
//! a record of every request, and the highest number of requests it has
//! seen at once.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, CONTENT_LENGTH};
use axum::http::{HeaderMap, StatusCode, Version};
use axum::response::Response;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// How the response body is framed.
#[derive(Debug, Clone)]
pub enum MockBody {
    /// No body with an explicit `Content-Length: 0`.
    Empty,
    /// These bytes with their `Content-Length`.
    Bytes(Vec<u8>),
    /// `Transfer-Encoding: chunked` with no data, so no `Content-Length`.
    ChunkedEmpty,
}

#[derive(Debug, Clone)]
pub struct MockResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: MockBody,
    /// Time the handler waits before answering.
    pub delay: Duration,
}

impl MockResponse {
    /// The v1 success: 202, `Content-Length: 0`, empty body.
    pub fn accepted() -> Self {
        Self::status(202)
    }

    /// `status` with an empty body and `Content-Length: 0`.
    pub fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: MockBody::Empty,
            delay: Duration::ZERO,
        }
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    pub fn with_body(mut self, body: MockBody) -> Self {
        self.body = body;
        self
    }

    pub fn after(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

/// One request as it arrived.
#[derive(Debug, Clone)]
pub struct Received {
    pub at: Instant,
    pub wall: SystemTime,
    pub method: String,
    pub path_and_query: String,
    pub version: Version,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Received {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    pub fn authorization(&self) -> Option<&str> {
        self.header(AUTHORIZATION.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("wake body is JSON")
    }

    /// The `inbox_seq` string of the wake body.
    pub fn inbox_seq(&self) -> String {
        self.json()["inbox_seq"]
            .as_str()
            .expect("inbox_seq is a string")
            .to_owned()
    }
}

struct MockState {
    script: VecDeque<MockResponse>,
    fallback: MockResponse,
    received: Vec<Received>,
    in_flight: usize,
    max_in_flight: usize,
}

type Shared = Arc<Mutex<MockState>>;

fn lock(state: &Shared) -> MutexGuard<'_, MockState> {
    state.lock().expect("mock receiver lock")
}

pub struct MockReceiver {
    addr: SocketAddr,
    state: Shared,
    task: JoinHandle<()>,
}

impl MockReceiver {
    /// Answers every request with [`MockResponse::accepted`] unless scripted.
    pub async fn start() -> Self {
        Self::with_fallback(MockResponse::accepted()).await
    }

    pub async fn with_fallback(fallback: MockResponse) -> Self {
        let state: Shared = Arc::new(Mutex::new(MockState {
            script: VecDeque::new(),
            fallback,
            received: Vec::new(),
            in_flight: 0,
            max_in_flight: 0,
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
        let addr = listener.local_addr().expect("mock addr");
        let router = Router::new()
            .fallback(handle)
            .with_state(Arc::clone(&state));
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.expect("mock serves");
        });
        Self { addr, state, task }
    }

    /// `http://127.0.0.1:<port>/wake`.
    pub fn url(&self) -> String {
        format!("http://{}/wake", self.addr)
    }

    /// Answers the next unscripted request with `response`.
    pub fn push(&self, response: MockResponse) {
        lock(&self.state).script.push_back(response);
    }

    /// Answers requests with `response` once the script is used up.
    pub fn set_fallback(&self, response: MockResponse) {
        lock(&self.state).fallback = response;
    }

    pub fn received(&self) -> Vec<Received> {
        lock(&self.state).received.clone()
    }

    pub fn count(&self) -> usize {
        lock(&self.state).received.len()
    }

    pub fn max_in_flight(&self) -> usize {
        lock(&self.state).max_in_flight
    }

    /// Waits until at least `count` requests have arrived; panics after `limit`.
    pub async fn wait_for(&self, count: usize, limit: Duration) -> Vec<Received> {
        let started = Instant::now();
        loop {
            let received = self.received();
            if received.len() >= count {
                return received;
            }
            assert!(
                started.elapsed() < limit,
                "expected {count} requests within {limit:?}, got {}",
                received.len()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

impl Drop for MockReceiver {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Decrements the in-flight count even when the client gives up and the
/// handler is dropped.
struct InFlightGuard(Shared);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        lock(&self.0).in_flight -= 1;
    }
}

async fn handle(State(state): State<Shared>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let at = Instant::now();
    let wall = SystemTime::now();
    let body = axum::body::to_bytes(body, 64 * 1024)
        .await
        .expect("request body");
    let response = {
        let mut state = lock(&state);
        state.received.push(Received {
            at,
            wall,
            method: parts.method.to_string(),
            path_and_query: parts
                .uri
                .path_and_query()
                .map(ToString::to_string)
                .unwrap_or_default(),
            version: parts.version,
            headers: parts.headers.clone(),
            body,
        });
        state.in_flight += 1;
        state.max_in_flight = state.max_in_flight.max(state.in_flight);
        state
            .script
            .pop_front()
            .unwrap_or_else(|| state.fallback.clone())
    };
    let _guard = InFlightGuard(Arc::clone(&state));
    tokio::time::sleep(response.delay).await;
    build(response)
}

fn build(response: MockResponse) -> Response {
    let mut builder =
        Response::builder().status(StatusCode::from_u16(response.status).expect("valid status"));
    for (name, value) in &response.headers {
        builder = builder.header(name, value);
    }
    let body = match response.body {
        MockBody::Empty => {
            builder = builder.header(CONTENT_LENGTH, "0");
            Body::empty()
        }
        MockBody::Bytes(bytes) => Body::from(bytes),
        MockBody::ChunkedEmpty => {
            Body::from_stream(futures_util::stream::iter([Ok::<Bytes, Infallible>(
                Bytes::new(),
            )]))
        }
    };
    builder.body(body).expect("valid response")
}
