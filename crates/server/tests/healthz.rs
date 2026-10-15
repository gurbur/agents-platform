mod common;

use common::{Fixture, RunningServer};
use platformd::api::X_REQUEST_ID;
use platformd::store::migrations::LATEST_SCHEMA_VERSION;
use reqwest::StatusCode;
use serde_json::{Value, json};

fn request_id_of(response: &reqwest::Response) -> String {
    response
        .headers()
        .get(X_REQUEST_ID)
        .expect("x-request-id header present")
        .to_str()
        .expect("ascii header")
        .to_owned()
}

fn assert_request_id_shape(id: &str) {
    let ulid = id.strip_prefix("req-").unwrap_or_else(|| panic!("{id}"));
    assert_eq!(ulid.len(), 26, "{id}");
    assert!(
        ulid.chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase()),
        "{id}"
    );
}

#[tokio::test]
async fn healthz_returns_ok_body_and_request_id() {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;

    let response = reqwest::get(server.url("/healthz")).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_request_id_shape(&request_id_of(&response));
    let body: Value = response.json().await.unwrap();
    assert_eq!(
        body,
        json!({"status": "ok", "schema_version": LATEST_SCHEMA_VERSION})
    );

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn every_response_gets_a_distinct_request_id() {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;

    let first = reqwest::get(server.url("/healthz")).await.unwrap();
    let second = reqwest::get(server.url("/healthz")).await.unwrap();
    let missing = reqwest::get(server.url("/no-such-path")).await.unwrap();

    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let ids = [
        request_id_of(&first),
        request_id_of(&second),
        request_id_of(&missing),
    ];
    for id in &ids {
        assert_request_id_shape(id);
    }
    assert_ne!(ids[0], ids[1]);
    assert_ne!(ids[1], ids[2]);

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn unknown_paths_get_the_error_body() {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;

    let response = reqwest::get(server.url("/no-such-path")).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let request_id = request_id_of(&response);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "not_found");
    assert_eq!(body["error"]["request_id"], request_id.as_str());

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn graceful_shutdown_closes_the_listener() {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;
    let addr = server.addr;
    assert!(reqwest::get(server.url("/healthz")).await.is_ok());

    server.shutdown().await.expect("clean shutdown");

    assert!(tokio::net::TcpStream::connect(addr).await.is_err());
}

#[tokio::test]
async fn restarting_on_the_same_database_succeeds() {
    let fx = Fixture::new();
    let config = fx.load_config();

    let first = RunningServer::start(&config).await;
    first.shutdown().await.unwrap();

    let second = RunningServer::start(&config).await;
    let response = reqwest::get(second.url("/healthz")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    second.shutdown().await.unwrap();
}

#[tokio::test]
async fn database_directory_is_created() {
    let fx = Fixture::new();
    assert!(!fx.db_path().parent().unwrap().exists());

    let server = RunningServer::start(&fx.load_config()).await;
    assert!(fx.db_path().is_file());
    server.shutdown().await.unwrap();
}
