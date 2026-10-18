//! Every JSON-body endpoint rejects a body that is valid JSON but not an
//! object (`IF-WIRE-002`): 422 `invalid_field` without `details.field`.
//!
//! Each array below is what the endpoint's fields would read positionally,
//! so a derive that accepts sequences would let it through as a valid
//! request.

mod common;

use common::{Api, Fixture, OWNER_TOKEN, Reply, RunningServer};
use serde_json::{Value, json};

async fn start() -> (Fixture, RunningServer, Api) {
    let fx = Fixture::new();
    let config = fx.load_config_with_identity(
        "issuer = \"test-hub\"\n\
         audiences = [\"garage-docs\"]\n\
         default_ttl_secs = 60\n\
         max_ttl_secs = 120\n",
    );
    let server = RunningServer::start(&config).await;
    let api = server.api();
    (fx, server, api)
}

fn assert_not_object(reply: &Reply) {
    reply.assert_error(422, "invalid_field");
    assert_eq!(reply.error_field(), None, "body: {}", reply.body);
}

#[tokio::test]
async fn register_rejects_an_array_body() {
    let (_fx, server, api) = start().await;
    let body = json!(["agent-x", "X", Value::Null]);
    assert_not_object(&api.post("/v1/agents", OWNER_TOKEN, body).await);

    let directory = api.get("/v1/agents", OWNER_TOKEN).await;
    assert_eq!(directory.body["items"], json!([]), "nothing registered");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn profile_rejects_an_array_body() {
    let (_fx, server, api) = start().await;
    api.register("agent-a", None).await;
    let body = json!(["New name"]);
    assert_not_object(
        &api.put("/v1/agents/agent-a/profile", OWNER_TOKEN, body)
            .await,
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn receiver_rejects_an_array_body() {
    let (_fx, server, api) = start().await;
    api.register("agent-a", None).await;
    let body = json!([Value::Null]);
    assert_not_object(
        &api.patch("/v1/agents/agent-a/receiver", OWNER_TOKEN, body)
            .await,
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn membership_rejects_an_array_body() {
    let (_fx, server, api) = start().await;
    api.register("agent-a", None).await;
    let body = json!([false]);
    let path = "/v1/agents/agent-a/membership/all";
    assert_not_object(&api.put(path, OWNER_TOKEN, body).await);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn send_message_rejects_an_array_body() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let body = json!(["all", "hello", "key-1"]);
    assert_not_object(&api.post("/v1/messages", &a.api_key, body).await);

    let history = api.get("/v1/messages?room_id=all", OWNER_TOKEN).await;
    assert_eq!(history.body["items"], json!([]), "nothing stored");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn read_marker_rejects_an_array_body() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let body = json!(["0"]);
    assert_not_object(&api.put("/v1/inbox/read-marker", &a.api_key, body).await);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn identity_token_rejects_an_array_body() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let body = json!(["garage-docs"]);
    assert_not_object(&api.post("/v1/identity/tokens", &a.api_key, body).await);
    server.shutdown().await.unwrap();
}
