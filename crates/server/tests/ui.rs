//! The browser console (S6, replaced by the S11 chat console): embedded
//! static files, security headers, no external resources, text-only
//! rendering, the token kept out of persistent storage, and every
//! endpoint the page calls is a real route the owner may use with that
//! method.

mod common;

use std::collections::BTreeSet;

use common::{Fixture, OWNER_TOKEN, RunningServer};
use reqwest::header::HeaderMap;
use reqwest::{Method, StatusCode};
use serde_json::json;

const INDEX_HTML: &str = include_str!("../ui/index.html");
const APP_JS: &str = include_str!("../ui/app.js");
const APP_CSS: &str = include_str!("../ui/app.css");
const FAVICON_SVG: &str = include_str!("../ui/favicon.svg");

/// The S6 policy plus `blob:` images for the fetched avatars (S11).
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
                   img-src 'self' blob:; base-uri 'none'; frame-ancestors 'none'";

/// The SVG namespace the script uses to build icons; a name, not a fetch.
const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";

fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .unwrap_or_else(|| panic!("missing header {name}"))
        .to_str()
        .expect("ascii header")
}

fn assert_security_headers(headers: &HeaderMap) {
    assert_eq!(header(headers, "content-security-policy"), CSP);
    assert_eq!(header(headers, "x-content-type-options"), "nosniff");
    assert_eq!(header(headers, "referrer-policy"), "no-referrer");
    assert_eq!(header(headers, "cache-control"), "no-store");
}

async fn fetch(server: &RunningServer, method: Method, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .request(method, server.url(path))
        .send()
        .await
        .expect("request sent")
}

#[tokio::test]
async fn embedded_files_are_served_byte_for_byte_with_security_headers() {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;

    let files = [
        ("/", "text/html; charset=utf-8", INDEX_HTML),
        ("/assets/app.js", "text/javascript; charset=utf-8", APP_JS),
        ("/assets/app.css", "text/css; charset=utf-8", APP_CSS),
        ("/assets/favicon.svg", "image/svg+xml", FAVICON_SVG),
    ];
    for (path, content_type, source) in files {
        let response = fetch(&server, Method::GET, path).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(header(response.headers(), "content-type"), content_type);
        assert_security_headers(response.headers());
        let body = response.bytes().await.expect("body");
        assert_eq!(body.as_ref(), source.as_bytes(), "{path}");
    }

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn head_matches_get_without_a_body() {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;

    let response = fetch(&server, Method::HEAD, "/").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header(response.headers(), "content-type"),
        "text/html; charset=utf-8"
    );
    assert_security_headers(response.headers());
    assert!(response.bytes().await.expect("body").is_empty());

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn unknown_assets_and_write_methods_get_the_error_body() {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;
    let api = server.api();

    api.send(Method::GET, "/assets/nope", None, None)
        .await
        .assert_error(404, "not_found");
    for method in [Method::POST, Method::PUT, Method::DELETE] {
        api.send(method.clone(), "/", None, None)
            .await
            .assert_error(405, "method_not_allowed");
        api.send(method, "/assets/app.js", None, None)
            .await
            .assert_error(405, "method_not_allowed");
    }

    server.shutdown().await.unwrap();
}

#[test]
fn page_has_no_inline_script_or_style() {
    assert!(!INDEX_HTML.contains("<style"));
    assert!(!INDEX_HTML.contains(" style="));
    // The only script element is the external, empty-bodied app.js.
    assert_eq!(INDEX_HTML.matches("<script").count(), 1);
    assert!(INDEX_HTML.contains("<script src=\"/assets/app.js\" defer></script>"));
    // Script-built nodes may not carry a style attribute either (CSSOM only).
    assert!(!APP_JS.contains("setAttribute('style'"));
    assert!(!APP_JS.contains("{ style:") && !APP_JS.contains(", style:"));
}

#[test]
fn phone_layout_keeps_text_controls_at_16px_without_disabling_zoom() {
    // iOS zooms into a focused control under 16px; the fix is the font size,
    // never a viewport that forbids pinch zoom.
    assert!(!INDEX_HTML.contains("maximum-scale") && !INDEX_HTML.contains("user-scalable"));
    let phone = APP_CSS
        .split("@media (max-width:720px){")
        .nth(1)
        .expect("phone media query");
    let rule = phone
        .lines()
        .find(|line| line.contains("font-size:16px"))
        .expect("16px rule in the phone layout");
    for control in [
        ".field input",
        ".box textarea",
        "dl.kv input",
        "dl.kv textarea",
        "dl.kv select",
        ".url input",
    ] {
        assert!(rule.contains(control), "{control} not at 16px on phones");
    }
    assert!(APP_CSS.contains("touch-action:manipulation"));
}

#[test]
fn swipe_to_reply_leaves_vertical_scroll_and_zoom_to_the_browser() {
    assert!(APP_CSS.contains(".msg,.b-row{touch-action:pan-y pinch-zoom}"));
    assert!(APP_JS.contains("event.pointerType !== 'touch'"));
    for kind in ["pointerdown", "pointermove", "pointerup", "pointercancel"] {
        let line = APP_JS
            .lines()
            .find(|line| line.contains(&format!("feed.addEventListener('{kind}'")))
            .unwrap_or_else(|| panic!("no {kind} listener on the feed"));
        assert!(line.contains("{ passive: true }"), "{line}");
    }
    // Only the touchmove listener may cancel, to lock scroll under a claimed swipe.
    assert!(
        APP_JS
            .contains("feed.addEventListener('touchmove', onSwipeTouchMove, { passive: false });")
    );
    assert!(APP_CSS.contains("overscroll-behavior:contain"));
}

#[test]
fn no_file_loads_anything_from_another_origin() {
    let script = APP_JS.replace(SVG_NAMESPACE, "");
    for (name, source) in [
        ("index.html", INDEX_HTML),
        ("app.css", APP_CSS),
        ("app.js", script.as_str()),
    ] {
        for remote in ["http://", "https://", "//cdn", "@import", "url("] {
            assert!(!source.contains(remote), "{name} contains {remote}");
        }
    }
    let local_refs: Vec<&str> = INDEX_HTML
        .match_indices("=\"/")
        .map(|(at, _)| {
            let rest = &INDEX_HTML[at + 2..];
            &rest[..rest.find('"').unwrap_or(rest.len())]
        })
        .collect();
    assert_eq!(
        local_refs,
        ["/assets/favicon.svg", "/assets/app.css", "/assets/app.js"]
    );
}

#[test]
fn script_never_parses_html_and_keeps_the_token_out_of_local_storage() {
    for forbidden in [
        "innerHTML",
        "outerHTML",
        "insertAdjacentHTML",
        "document.write",
        "document.cookie",
        "eval(",
        "new Function",
    ] {
        assert!(!APP_JS.contains(forbidden), "app.js uses {forbidden}");
    }
    assert!(APP_JS.starts_with("'use strict';"));

    // localStorage is reached through one getter and one setter, for view
    // preferences only.
    let local: Vec<&str> = APP_JS
        .lines()
        .filter(|line| line.contains("localStorage"))
        .map(str::trim)
        .filter(|line| !line.starts_with("//"))
        .collect();
    assert_eq!(
        local,
        [
            "return window.localStorage.getItem(key);",
            "window.localStorage.setItem(key, value);"
        ]
    );
    for line in APP_JS.lines().filter(|line| line.contains("prefSet(")) {
        assert!(!line.to_lowercase().contains("token"), "{line}");
    }
    // The token key is only ever used with sessionStorage.
    for line in APP_JS
        .lines()
        .filter(|line| line.contains("TOKEN_STORAGE_KEY"))
    {
        let line = line.trim();
        assert!(
            line.starts_with("const TOKEN_STORAGE_KEY") || line.contains("sessionStorage"),
            "{line}"
        );
    }
}

/// S14: presence comes from the directory the page already polls; the page
/// never calls the presence endpoints and puts the note in as text.
#[test]
fn presence_is_read_from_the_directory_only() {
    assert!(
        APP_JS.contains(".presence"),
        "the page reads the directory presence"
    );
    assert!(
        APP_JS.contains(".last_seen_at"),
        "the page reads last_seen_at"
    );
    assert!(
        !APP_JS.contains("/presence"),
        "no presence endpoint is called"
    );
    for state in ["online", "busy", "offline"] {
        assert!(
            APP_CSS.contains(&format!(".dot.{state}")),
            "a {state} dot style"
        );
    }
    // The note reaches the DOM through `h(…, { text: … })` or a text node
    // only; innerHTML and friends are refused for the whole script above.
    let note_lines: Vec<&str> = APP_JS
        .lines()
        .filter(|line| line.contains(".note"))
        .collect();
    assert!(!note_lines.is_empty());
    for line in note_lines {
        assert!(!line.contains("HTML"), "{line}");
    }
}

/// Every `API.name(` the script calls, with the method it uses.
fn request_pairs(source: &str) -> BTreeSet<(&str, &str)> {
    source
        .match_indices("request('")
        .map(|(at, _)| {
            let rest = &source[at + "request('".len()..];
            let (method, rest) = rest.split_once("', API.").expect("request(METHOD, API.x(");
            let name = &rest[..rest.find('(').expect("call")];
            (method, name)
        })
        .collect()
}

/// The `/v1/` path literals in `app.js`, up to the closing quote.
fn endpoint_literals(source: &str) -> BTreeSet<&str> {
    source
        .match_indices("/v1/")
        .map(|(at, _)| {
            let rest = &source[at..];
            let end = rest.find(['\'', '"', '`']).unwrap_or(rest.len());
            &rest[..end]
        })
        .collect()
}

/// What the page calls: API name, method, path literal, and a concrete
/// request for it. `{root}` in a path or body stands for the root of a
/// thread the test opens first (S15).
struct Call {
    name: &'static str,
    method: Method,
    literal: &'static str,
    path: &'static str,
    body: Option<(&'static str, Vec<u8>)>,
}

const ROOT_PLACEHOLDER: &str = "{root}";

fn json_body(value: serde_json::Value) -> Option<(&'static str, Vec<u8>)> {
    Some(("application/json", serde_json::to_vec(&value).unwrap()))
}

fn calls() -> Vec<Call> {
    let png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0];
    let call = |name, method, literal, path, body| Call {
        name,
        method,
        literal,
        path,
        body,
    };
    vec![
        call("owner", Method::GET, "/v1/owner", "/v1/owner", None),
        call("agents", Method::GET, "/v1/agents", "/v1/agents", None),
        call(
            "agents",
            Method::POST,
            "/v1/agents",
            "/v1/agents",
            json_body(json!({"agent_id": "agent-new", "name": "New"})),
        ),
        call("rooms", Method::GET, "/v1/rooms", "/v1/rooms", None),
        // S13: the owner's topic room management.
        call(
            "rooms",
            Method::POST,
            "/v1/rooms",
            "/v1/rooms",
            json_body(json!({"room_id": "room:console", "title": "Console"})),
        ),
        call(
            "roomMember",
            Method::PUT,
            "/v1/rooms/${encodeURIComponent(roomId)}/members/${encodeURIComponent(agentId)}",
            "/v1/rooms/room%3Aconsole/members/human",
            None,
        ),
        call(
            "messages",
            Method::GET,
            "/v1/messages",
            "/v1/messages?room_id=all",
            None,
        ),
        // S13: owner posting is the one send endpoint with `sender_id`.
        call(
            "messages",
            Method::POST,
            "/v1/messages",
            "/v1/messages",
            json_body(json!({
                "sender_id": "human",
                "room_id": "room:console",
                "body": "hi",
                "client_key": "console-1",
                "wake": false
            })),
        ),
        call(
            "message",
            Method::GET,
            "/v1/messages/${encodeURIComponent(messageId)}",
            "/v1/messages/msg-01JZZZZZZZZZZZZZZZZZZZZZZZ",
            None,
        ),
        // S15: the timeline reads top-level messages, the thread panel one
        // thread, its participants and the "나" mute toggle.
        call(
            "messages",
            Method::GET,
            "/v1/messages",
            "/v1/messages?room_id=all&top_level=true",
            None,
        ),
        call(
            "messages",
            Method::GET,
            "/v1/messages",
            "/v1/messages?room_id=all&thread_id={root}",
            None,
        ),
        call(
            "thread",
            Method::GET,
            "/v1/threads/${encodeURIComponent(rootId)}",
            "/v1/threads/{root}",
            None,
        ),
        call(
            "participation",
            Method::PUT,
            "/v1/threads/${encodeURIComponent(rootId)}/participation",
            "/v1/threads/{root}/participation",
            json_body(json!({"wake": false, "agent_id": "human"})),
        ),
        // S15: a correction is the one send with `corrects`.
        call(
            "messages",
            Method::POST,
            "/v1/messages",
            "/v1/messages",
            json_body(json!({
                "sender_id": "human",
                "room_id": "all",
                "body": "fixed",
                "client_key": "console-fix",
                "corrects": "{root}"
            })),
        ),
        call(
            "roomMember",
            Method::DELETE,
            "/v1/rooms/${encodeURIComponent(roomId)}/members/${encodeURIComponent(agentId)}",
            "/v1/rooms/room%3Aconsole/members/human",
            None,
        ),
        call(
            "roomArchive",
            Method::POST,
            "/v1/rooms/${encodeURIComponent(roomId)}/archive",
            "/v1/rooms/room%3Aconsole/archive",
            None,
        ),
        call(
            "profile",
            Method::PUT,
            "/v1/agents/${encodeURIComponent(agentId)}/profile",
            "/v1/agents/agent-a/profile",
            json_body(json!({"role": "ops"})),
        ),
        call(
            "avatar",
            Method::PUT,
            "/v1/agents/${encodeURIComponent(agentId)}/avatar",
            "/v1/agents/agent-a/avatar",
            Some(("image/png", png)),
        ),
        call(
            "avatar",
            Method::GET,
            "/v1/agents/${encodeURIComponent(agentId)}/avatar",
            "/v1/agents/agent-a/avatar",
            None,
        ),
        call(
            "avatar",
            Method::DELETE,
            "/v1/agents/${encodeURIComponent(agentId)}/avatar",
            "/v1/agents/agent-a/avatar",
            None,
        ),
        call(
            "receiver",
            Method::PATCH,
            "/v1/agents/${encodeURIComponent(agentId)}/receiver",
            "/v1/agents/agent-a/receiver",
            json_body(json!({"receiver_url": null})),
        ),
        call(
            "membership",
            Method::PUT,
            "/v1/agents/${encodeURIComponent(agentId)}/membership/all",
            "/v1/agents/agent-a/membership/all",
            json_body(json!({"joined": true})),
        ),
        call(
            "delivery",
            Method::GET,
            "/v1/agents/${encodeURIComponent(agentId)}/delivery",
            "/v1/agents/agent-a/delivery",
            None,
        ),
        call(
            "retry",
            Method::POST,
            "/v1/agents/${encodeURIComponent(agentId)}/delivery/retry",
            "/v1/agents/agent-a/delivery/retry",
            None,
        ),
        call(
            "inbox",
            Method::GET,
            "/v1/agents/${encodeURIComponent(agentId)}/inbox",
            "/v1/agents/agent-a/inbox",
            None,
        ),
        // Last: deletes the agent the POST above registered.
        call(
            "agent",
            Method::DELETE,
            "/v1/agents/${encodeURIComponent(agentId)}",
            "/v1/agents/agent-new",
            None,
        ),
    ]
}

/// The page names exactly these endpoints and methods, and the owner gets a
/// success from each (in this order, so the avatar exists to fetch and
/// delete, the topic room exists to join, post into and archive, and the
/// registered agent to delete). The console never calls the read-marker
/// endpoint, nor the owner-posting route S13 removed.
#[tokio::test]
async fn every_endpoint_the_page_calls_answers_the_owner() {
    let calls = calls();
    let literals: BTreeSet<&str> = calls.iter().map(|call| call.literal).collect();
    assert_eq!(endpoint_literals(APP_JS), literals);
    let pairs: BTreeSet<(&str, &str)> = calls
        .iter()
        .map(|call| (call.method.as_str(), call.name))
        .collect();
    assert_eq!(request_pairs(APP_JS), pairs);
    assert!(!APP_JS.contains("read-marker"));
    for literal in endpoint_literals(APP_JS) {
        assert!(
            !(literal.starts_with("/v1/rooms/") && literal.ends_with("/messages")),
            "removed route {literal}"
        );
    }

    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config_with_owner(&["human"])).await;
    let api = server.api();
    let agent = api.register("agent-a", None).await;
    api.register("human", None).await;
    // A thread human started in `all` and agent-a opened.
    let root = api
        .post(
            "/v1/messages",
            OWNER_TOKEN,
            json!({"sender_id": "human", "room_id": "all", "body": "root", "client_key": "root"}),
        )
        .await;
    assert_eq!(root.status.as_u16(), 201, "{}", root.body);
    let root_id = root.body["message_id"].as_str().unwrap().to_owned();
    let reply = api
        .post(
            "/v1/messages",
            &agent.api_key,
            json!({"reply_to": root_id, "body": "re", "client_key": "re"}),
        )
        .await;
    assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
    for call in &calls {
        let (content_type, body) = match &call.body {
            Some((content_type, body)) if *content_type == "application/json" => (
                vec![("content-type", *content_type)],
                Some(
                    String::from_utf8(body.clone())
                        .expect("a JSON body is UTF-8")
                        .replace(ROOT_PLACEHOLDER, &root_id)
                        .into_bytes(),
                ),
            ),
            Some((content_type, body)) => {
                (vec![("content-type", *content_type)], Some(body.clone()))
            }
            None => (Vec::new(), None),
        };
        let path = call.path.replace(ROOT_PLACEHOLDER, &root_id);
        let reply = api
            .send_bytes(
                call.method.clone(),
                &path,
                Some(OWNER_TOKEN),
                &content_type,
                body,
            )
            .await;
        let expected_missing = call.name == "message";
        if expected_missing {
            // A real route: the made-up id is simply not there.
            reply.assert_error(404, "message_not_found");
            continue;
        }
        assert!(
            reply.status.is_success(),
            "{} {}: {} {}",
            call.method,
            path,
            reply.status,
            String::from_utf8_lossy(&reply.bytes)
        );
    }

    server.shutdown().await.unwrap();
}

/// The one-time credentials of a registration (S12) are shown as dialog
/// text only: nothing in the script logs or stores them, and closing the
/// dialog wipes both values.
#[test]
fn registration_credentials_are_never_logged_or_stored() {
    assert!(!APP_JS.contains("console."), "app.js writes to the console");
    for line in APP_JS
        .lines()
        .filter(|line| line.contains("agent_api_key") || line.contains("receiver_secret"))
    {
        let line = line.trim();
        for sink in ["Storage", "prefSet", "location", "history.", "S."] {
            assert!(!line.contains(sink), "{line}");
        }
        assert!(line.starts_with("$('cred"), "only the dialog text: {line}");
    }
    let closed = APP_JS
        .split("async function onCredentialsClosed() {")
        .nth(1)
        .expect("close handler");
    let body = &closed[..closed.find("\n}").expect("end of handler")];
    for node in ["credKey", "credSecret"] {
        assert!(
            body.contains(&format!("$('{node}').textContent = '';")),
            "closing does not wipe {node}"
        );
    }
    assert!(APP_JS.contains("$('cred').addEventListener('close', onCredentialsClosed);"));
}

/// S15: the timeline reads `top_level=true` and folds new replies from the
/// one existing feed poll (no new timer); the thread panel reads
/// `thread_id`, replies with `reply_to` only, and mutes "나" by naming it
/// as `agent_id`; a correction carries `corrects`. Server text stays text.
#[test]
fn threads_and_corrections_use_the_documented_queries_and_no_new_polling() {
    for needle in [
        "top_level: 'true'",
        "thread_id: ",
        "corrects",
        "thread_summary",
        "reply_count",
        "agent_id: S.me",
        "답글 ",
        "정정됨",
    ] {
        assert!(APP_JS.contains(needle), "app.js lacks {needle}");
    }
    assert_eq!(
        APP_JS.matches("setInterval(").count(),
        2,
        "the feed poll and the room refresh stay the only timers"
    );
    // The panel exists in the page and has a phone layout.
    assert!(INDEX_HTML.contains("id=\"thread\""));
    let phone = APP_CSS
        .split("@media (max-width:720px){")
        .nth(1)
        .expect("phone media query");
    assert!(
        phone.contains(".thread{"),
        "no phone layout for the thread panel"
    );
    // A send names one destination: reply_to for a thread, room_id otherwise.
    assert!(!APP_JS.contains("body.thread_id"));
}
