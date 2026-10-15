//! The read-only browser UI (slice S6): static files embedded at build time.
//!
//! The page talks to the v1 API with `fetch`, so it shares the API's
//! authentication, paging and no-ack rules; nothing here touches the store or
//! a credential. Every file carries the same security headers. Unknown
//! `/assets/*` paths fall through to the router's 404 `not_found`.

use axum::Router;
use axum::http::HeaderName;
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS,
};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use super::AppState;

/// No inline script or style, no third-party origin, no framing.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
                   img-src 'self'; base-uri 'none'; frame-ancestors 'none'";

const INDEX_HTML: &str = include_str!("../../ui/index.html");
const APP_JS: &str = include_str!("../../ui/app.js");
const APP_CSS: &str = include_str!("../../ui/app.css");
const FAVICON_SVG: &str = include_str!("../../ui/favicon.svg");

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/assets/app.js", get(app_js))
        .route("/assets/app.css", get(app_css))
        .route("/assets/favicon.svg", get(favicon))
}

async fn index() -> Response {
    embedded("text/html; charset=utf-8", INDEX_HTML)
}

async fn app_js() -> Response {
    embedded("text/javascript; charset=utf-8", APP_JS)
}

async fn app_css() -> Response {
    embedded("text/css; charset=utf-8", APP_CSS)
}

async fn favicon() -> Response {
    embedded("image/svg+xml", FAVICON_SVG)
}

fn embedded(content_type: &'static str, body: &'static str) -> Response {
    let headers: [(HeaderName, &'static str); 5] = [
        (CONTENT_TYPE, content_type),
        (CONTENT_SECURITY_POLICY, CSP),
        (X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (REFERRER_POLICY, "no-referrer"),
        (CACHE_CONTROL, "no-store"),
    ];
    (headers, body).into_response()
}
