//! S15 corrections (`IF-WIRE-015`, API_V1 "Message 송신과 durable
//! commit"): `corrects` is an extra field naming the sender's own message in
//! the resolved room; it routes nothing, never moves a message into a
//! thread, and joins the idempotency hash only when present.

mod common;

use common::{Api, Fixture, OWNER_TOKEN, Reply, RunningServer};
use serde_json::{Value, json};

async fn start() -> (Fixture, RunningServer, Api) {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config_with_owner(&["human"])).await;
    let api = server.api();
    (fx, server, api)
}

async fn send(api: &Api, token: &str, body: Value) -> Reply {
    api.post("/v1/messages", token, body).await
}

async fn sent(api: &Api, token: &str, body: Value) -> Value {
    let reply = send(api, token, body).await;
    assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
    reply.body
}

fn id(message: &Value) -> &str {
    message["message_id"].as_str().unwrap()
}

const MISSING_ID: &str = "msg-01J00000000000000000000000";

#[tokio::test]
async fn a_sender_corrects_its_own_message_and_the_original_stays() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let original = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "3시", "client_key": "o"}),
    )
    .await;
    assert_eq!(original["corrects"], Value::Null);
    // Scenario 30.
    let correction = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "4시", "client_key": "c", "corrects": id(&original)}),
    )
    .await;
    assert_eq!(correction["corrects"], original["message_id"]);
    assert_eq!(correction["thread_id"], Value::Null);
    assert_eq!(correction["reply_to"], Value::Null);
    let stored = api
        .get(&format!("/v1/messages/{}", id(&original)), &a.api_key)
        .await;
    assert_eq!(stored.body["body"], "3시");
    assert_eq!(stored.body["corrects"], Value::Null);

    // A correction of a correction is allowed.
    let again = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "5시", "client_key": "c2", "corrects": id(&correction)}),
    )
    .await;
    assert_eq!(again["corrects"], correction["message_id"]);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_correction_joins_a_thread_only_through_reply_to() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let root = sent(
        &api,
        &b.api_key,
        json!({"room_id": "all", "body": "root", "client_key": "r"}),
    )
    .await;
    let in_thread = sent(
        &api,
        &a.api_key,
        json!({"reply_to": id(&root), "body": "wrong", "client_key": "w"}),
    )
    .await;
    let outside = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "fixed", "client_key": "f", "corrects": id(&in_thread)}),
    )
    .await;
    assert_eq!(outside["thread_id"], Value::Null, "no automatic placement");
    let inside = sent(
        &api,
        &a.api_key,
        json!({"reply_to": id(&in_thread), "body": "fixed", "client_key": "f2", "corrects": id(&in_thread)}),
    )
    .await;
    assert_eq!(inside["thread_id"], root["message_id"]);
    assert_eq!(inside["corrects"], in_thread["message_id"]);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn corrections_of_other_messages_are_refused_in_order() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    api.register("human", None).await;
    let mine = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "mine", "client_key": "m"}),
    )
    .await;
    let theirs = sent(
        &api,
        &b.api_key,
        json!({"room_id": "all", "body": "theirs", "client_key": "t"}),
    )
    .await;
    let elsewhere = sent(
        &api,
        &a.api_key,
        json!({"to": "agent-b", "body": "dm", "client_key": "d"}),
    )
    .await;
    let correcting = |key: &str, target: &str| json!({"room_id": "all", "body": "x", "client_key": key, "corrects": target});

    // Scenario 31.
    send(&api, &a.api_key, correcting("1", id(&theirs)))
        .await
        .assert_error(422, "corrects_not_own");
    send(&api, &a.api_key, correcting("2", id(&elsewhere)))
        .await
        .assert_error(422, "corrects_room_mismatch");
    send(&api, &a.api_key, correcting("3", MISSING_ID))
        .await
        .assert_error(404, "corrects_not_found");
    let malformed = send(&api, &a.api_key, correcting("4", "nope")).await;
    malformed.assert_error(422, "invalid_field");
    assert_eq!(malformed.error_field(), Some("corrects"));
    let wrong_type = send(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "x", "client_key": "5", "corrects": 7}),
    )
    .await;
    wrong_type.assert_error(422, "invalid_field");
    assert_eq!(wrong_type.error_field(), Some("corrects"));

    // `corrects` is no destination.
    send(
        &api,
        &a.api_key,
        json!({"body": "x", "client_key": "6", "corrects": id(&mine)}),
    )
    .await
    .assert_error(422, "invalid_destination");
    // Its format comes right after the destination's, before mentions.
    let format_first = send(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "x", "client_key": "7", "corrects": "nope", "mentions": ["Bad"]}),
    )
    .await;
    assert_eq!(format_first.error_field(), Some("corrects"));
    // The room checks and mentions (step 17) come before corrects (17a–c).
    send(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "x", "client_key": "8", "corrects": MISSING_ID, "mentions": ["agent-z"]}),
    )
    .await
    .assert_error(422, "mention_not_member");
    send(
        &api,
        &a.api_key,
        json!({"room_id": "room:nowhere", "body": "x", "client_key": "9", "corrects": MISSING_ID}),
    )
    .await
    .assert_error(403, "not_room_member");

    // Owner posts are judged by their sender_id.
    let human_post = sent(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "room_id": "all", "body": "h", "client_key": "h"}),
    )
    .await;
    let fixed = sent(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "room_id": "all", "body": "h2", "client_key": "h2", "corrects": id(&human_post)}),
    )
    .await;
    assert_eq!(fixed["corrects"], human_post["message_id"]);
    send(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "room_id": "all", "body": "h3", "client_key": "h3", "corrects": id(&mine)}),
    )
    .await
    .assert_error(422, "corrects_not_own");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn corrects_joins_the_idempotency_hash_only_when_present() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let first = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "one", "client_key": "1"}),
    )
    .await;
    let second = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "two", "client_key": "2"}),
    )
    .await;
    let plain = json!({"room_id": "all", "body": "plain", "client_key": "p"});
    let original = sent(&api, &a.api_key, plain.clone()).await;
    // Without `corrects` the request is the S13 one: a repeat is 200, and
    // a null `corrects` is the same request.
    let repeat = send(&api, &a.api_key, plain).await;
    assert_eq!(repeat.status.as_u16(), 200, "{}", repeat.body);
    assert_eq!(repeat.body["message_id"], original["message_id"]);
    let null = send(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "plain", "client_key": "p", "corrects": null}),
    )
    .await;
    assert_eq!(null.status.as_u16(), 200, "{}", null.body);
    // Adding `corrects` makes it another request.
    send(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "plain", "client_key": "p", "corrects": id(&first)}),
    )
    .await
    .assert_error(409, "idempotency_conflict");

    let correction =
        json!({"room_id": "all", "body": "fix", "client_key": "c", "corrects": id(&first)});
    let corrected = sent(&api, &a.api_key, correction.clone()).await;
    let replay = send(&api, &a.api_key, correction).await;
    assert_eq!(replay.status.as_u16(), 200, "{}", replay.body);
    assert_eq!(replay.body["message_id"], corrected["message_id"]);
    assert_eq!(replay.body["corrects"], first["message_id"]);
    // Scenario 31: only `corrects` changed.
    send(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "fix", "client_key": "c", "corrects": id(&second)}),
    )
    .await
    .assert_error(409, "idempotency_conflict");
    server.shutdown().await.unwrap();
}
