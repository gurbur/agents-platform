//! Shared fixtures: an in-process platformd, a runner for the real
//! `platform` binary, output parsers and an interposing HTTP proxy.

#![allow(dead_code)] // Each test binary uses a different subset.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use platformd::config::Config;
use tokio::sync::oneshot;

pub const BIN: &str = env!("CARGO_BIN_EXE_platform");
pub const OWNER_TOKEN: &str = "cli-test-owner-token-0123456789abcdef0123";

pub struct Platformd {
    _dir: tempfile::TempDir,
    pub addr: SocketAddr,
    pub url: String,
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<Result<(), platformd::ServeError>>,
}

impl Platformd {
    pub async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("owner.token"), OWNER_TOKEN).unwrap();
        let config_path = dir.path().join("platformd.toml");
        std::fs::write(
            &config_path,
            "listen = \"127.0.0.1:0\"\ndb_path = \"platform.db\"\nowner_token_file = \"owner.token\"\n",
        )
        .unwrap();
        let config = Config::load(&config_path).unwrap();
        let server = platformd::start(&config).await.unwrap();
        let addr = server.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(server.serve(async move {
            let _ = stopped.await;
        }));
        Self {
            _dir: dir,
            addr,
            url: format!("http://{addr}"),
            stop,
            task,
        }
    }

    pub async fn stop(self) {
        let _ = self.stop.send(());
        self.task.await.unwrap().unwrap();
    }

    /// Runs `platform --url <server> --token <token> args...`.
    pub async fn cli(&self, token: &str, args: &[&str]) -> Output {
        let mut full = vec!["--url", self.url.as_str(), "--token", token];
        full.extend_from_slice(args);
        platform(&full).await
    }

    /// Like [`Platformd::cli`] with `stdin` piped in.
    pub async fn cli_with_stdin(&self, token: &str, args: &[&str], stdin: &[u8]) -> Output {
        let mut full = vec!["--url", self.url.as_str(), "--token", token];
        full.extend_from_slice(args);
        run(&full, Some(stdin.to_vec())).await
    }

    /// Registers through the CLI as the owner and returns the API key.
    pub async fn register(&self, agent_id: &str) -> String {
        let output = self
            .cli(OWNER_TOKEN, &["agent", "register", agent_id])
            .await;
        let out = ok(&output);
        value(&out, "agent_api_key")
    }
}

/// Runs `platform` without inheriting PLATFORM_* from the developer's shell.
pub async fn platform(args: &[&str]) -> Output {
    run(args, None).await
}

async fn run(args: &[&str], stdin: Option<Vec<u8>>) -> Output {
    let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    tokio::task::spawn_blocking(move || {
        let mut child = Command::new(BIN)
            .args(&args)
            .env_remove("PLATFORM_URL")
            .env_remove("PLATFORM_TOKEN")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(bytes) = stdin {
            child.stdin.take().unwrap().write_all(&bytes).unwrap();
        }
        child.wait_with_output().unwrap()
    })
    .await
    .unwrap()
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Asserts exit 0 and returns stdout.
pub fn ok(output: &Output) -> String {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        stdout(output),
        stderr(output)
    );
    stdout(output)
}

/// Asserts exit 0 and parses stdout as one JSON document.
pub fn json(output: &Output) -> serde_json::Value {
    let out = ok(output);
    serde_json::from_str(&out).unwrap_or_else(|err| panic!("invalid JSON ({err}): {out}"))
}

/// Asserts exit 1 with `platform: <code>: <message> (request req-…)` on stderr.
pub fn assert_server_error(output: &Output, code: &str) {
    let err = stderr(output);
    assert_eq!(output.status.code(), Some(1), "stderr: {err}");
    let line = err
        .strip_prefix(&format!("platform: {code}: "))
        .unwrap_or_else(|| panic!("expected code {code}: {err}"));
    let request = line
        .trim_end()
        .rsplit_once(" (request ")
        .map(|(_, id)| id)
        .unwrap_or_else(|| panic!("no request id: {err}"));
    assert!(
        request.starts_with("req-") && request.ends_with(')'),
        "{err}"
    );
}

/// The value of a `key  value` line.
pub fn value(text: &str, key: &str) -> String {
    text.lines()
        .find_map(|line| {
            let rest = line.strip_prefix(key)?;
            rest.starts_with(' ').then(|| rest.trim_start().to_owned())
        })
        .unwrap_or_else(|| panic!("no `{key}` line in:\n{text}"))
}

/// The first column of every table row (header and summary excluded).
pub fn first_column(text: &str) -> Vec<String> {
    let mut lines = text.lines();
    lines.next(); // header
    lines
        .filter_map(|line| line.split_whitespace().next())
        .filter(|cell| cell.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_owned)
        .collect()
}

/// An address nothing listens on: bind an ephemeral port, then release it.
pub fn closed_port_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

/// One HTTP/1.1 exchange on a fresh connection (`Connection: close`).
/// Returns the raw response bytes.
pub fn raw_exchange(upstream: SocketAddr, request_head: &str, body: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(upstream).unwrap();
    stream.write_all(request_head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    response
}

/// `POST /v1/messages` straight to the server, bypassing the CLI.
pub fn post_message(upstream: SocketAddr, token: &str, to: &str, body: &str, client_key: &str) {
    let payload = serde_json::json!({"to": to, "body": body, "client_key": client_key}).to_string();
    let head = format!(
        "POST /v1/messages HTTP/1.1\r\nhost: {upstream}\r\nauthorization: Bearer {token}\r\n\
         content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        payload.len()
    );
    let response = raw_exchange(upstream, &head, payload.as_bytes());
    let status_line = String::from_utf8_lossy(&response);
    assert!(status_line.starts_with("HTTP/1.1 201"), "{status_line}");
}

type Hook = Box<dyn FnOnce() + Send>;

/// Forwards every request to `upstream` and runs `hook` once, after the
/// first `/v1/inbox` response arrived but before it is passed on. Records
/// each request target.
pub struct InterposingProxy {
    pub url: String,
    pub targets: Arc<Mutex<Vec<String>>>,
}

impl InterposingProxy {
    pub fn start(upstream: SocketAddr, hook: Hook) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let targets = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&targets);
        std::thread::spawn(move || {
            let mut hook = Some(hook);
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let target = forward(stream, upstream, &mut hook);
                recorded.lock().unwrap().push(target);
            }
        });
        Self { url, targets }
    }

    pub fn targets(&self) -> Vec<String> {
        self.targets.lock().unwrap().clone()
    }
}

/// Proxies one request with `Connection: close` on both legs.
fn forward(client: TcpStream, upstream: SocketAddr, hook: &mut Option<Hook>) -> String {
    let mut reader = BufReader::new(client);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    let target = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    let mut head = request_line.clone();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
        let name = line
            .split(':')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name == "connection" {
            continue;
        }
        if name == "content-length" {
            content_length = line.split(':').nth(1).unwrap().trim().parse().unwrap();
        }
        head.push_str(&line);
    }
    head.push_str("connection: close\r\n\r\n");
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).unwrap();

    let response = raw_exchange(upstream, &head, &body);
    if target.starts_with("/v1/inbox")
        && let Some(hook) = hook.take()
    {
        hook();
    }
    let mut client = reader.into_inner();
    client.write_all(&response).unwrap();
    target
}
