//! S6 browser UI: embedded static files, security headers, and the
//! read-only contract (every endpoint the page calls is a GET).

mod common;

use std::collections::BTreeSet;

use common::{Fixture, OWNER_TOKEN, RunningServer};
use reqwest::header::HeaderMap;
use reqwest::{Method, StatusCode};

const INDEX_HTML: &str = include_str!("../ui/index.html");
const APP_JS: &str = include_str!("../ui/app.js");
const APP_CSS: &str = include_str!("../ui/app.css");
const FAVICON_SVG: &str = include_str!("../ui/favicon.svg");

/// The S6 spec's policy, verbatim.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
                   img-src 'self'; base-uri 'none'; frame-ancestors 'none'";

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
}

#[test]
fn script_only_sends_get_and_never_parses_html_or_persists_beyond_the_session() {
    let methods: Vec<&str> = APP_JS
        .match_indices("method:")
        .map(|(at, _)| APP_JS[at..].lines().next().unwrap_or_default().trim())
        .collect();
    assert_eq!(methods, ["method: 'GET',"]);
    for forbidden in [
        "innerHTML",
        "outerHTML",
        "insertAdjacentHTML",
        "document.write",
        "localStorage",
        "document.cookie",
        "eval(",
    ] {
        assert!(!APP_JS.contains(forbidden), "app.js uses {forbidden}");
    }
    assert!(APP_JS.starts_with("'use strict';"));
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

/// Every endpoint the page calls answers a GET from the owner, and the page
/// names no other endpoint.
#[tokio::test]
async fn every_endpoint_the_page_calls_is_an_owner_get() {
    let calls = [
        ("/v1/agents", "/v1/agents".to_owned()),
        ("/v1/messages", "/v1/messages?room_id=all".to_owned()),
        (
            "/v1/agents/${encodeURIComponent(agentId)}/inbox",
            "/v1/agents/agent-a/inbox".to_owned(),
        ),
        (
            "/v1/agents/${encodeURIComponent(agentId)}/delivery",
            "/v1/agents/agent-a/delivery".to_owned(),
        ),
    ];
    let expected: BTreeSet<&str> = calls.iter().map(|(literal, _)| *literal).collect();
    assert_eq!(endpoint_literals(APP_JS), expected);

    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;
    let api = server.api();
    api.register("agent-a", None).await;
    for (_, path) in &calls {
        let reply = api.get(path, OWNER_TOKEN).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}: {}", reply.body);
    }

    server.shutdown().await.unwrap();
}
