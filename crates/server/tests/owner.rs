//! S11: `GET /v1/owner` — the owner's `[owner] post_as` allowlist, which
//! the console uses to know who it speaks as (`IF-API-OWNER`).

mod common;

use common::{Fixture, OWNER_TOKEN, RunningServer};
use reqwest::Method;
use serde_json::json;

#[tokio::test]
async fn the_owner_reads_post_as_in_configuration_order() {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config_with_owner(&["human", "agent-x"])).await;
    let api = server.api();

    // Registration is not required.
    let reply = api.get("/v1/owner", OWNER_TOKEN).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.body, json!({"post_as": ["human", "agent-x"]}));

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_default_configuration_lists_no_one() {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;
    let api = server.api();

    let reply = api.get("/v1/owner", OWNER_TOKEN).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.body, json!({"post_as": []}));

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn only_the_owner_may_ask_and_without_parameters() {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config_with_owner(&["human"])).await;
    let api = server.api();
    let human = api.register("human", None).await;

    api.send(Method::GET, "/v1/owner", None, None)
        .await
        .assert_error(401, "unauthorized");
    // Even the allowlisted agent's own key.
    api.get("/v1/owner", &human.api_key)
        .await
        .assert_error(403, "forbidden");
    let reply = api.get("/v1/owner?verbose=1", OWNER_TOKEN).await;
    reply.assert_error(422, "unexpected_field");
    assert_eq!(reply.error_field(), Some("verbose"));
    api.send(
        Method::POST,
        "/v1/owner",
        Some(OWNER_TOKEN),
        Some(json!({})),
    )
    .await
    .assert_error(405, "method_not_allowed");

    server.shutdown().await.unwrap();
}
