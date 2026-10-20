//! S13 CLI rooms and event output (`IF-CLI-001` S13): `room list|create|
//! invite|remove|archive`, `receiver set --wake-policy`, the new inbox,
//! history and message columns, and `inbox --all-pages --json` keeping the
//! server's items verbatim.

mod common;

use common::{
    CaptureServer, OWNER_TOKEN, Platformd, assert_server_error, json, ok, platform, stderr, stdout,
    value,
};
use serde_json::json;

fn table(text: &str) -> Vec<Vec<&str>> {
    text.lines()
        .map(|line| line.split_whitespace().collect())
        .collect()
}

#[tokio::test]
async fn the_owner_manages_topic_rooms_and_agents_list_their_own() {
    let server = Platformd::start_with_owner(&["human"]).await;
    let a = server.register("agent-a").await;
    let c = server.register("agent-c").await;

    let created = ok(&server
        .cli(
            OWNER_TOKEN,
            &["room", "create", "room:launch", "--title", "출시"],
        )
        .await);
    assert_eq!(value(&created, "room_id"), "room:launch");
    assert_eq!(value(&created, "kind"), "topic");
    assert_eq!(value(&created, "title"), "출시");
    assert_eq!(value(&created, "members"), "-");
    assert_eq!(value(&created, "created_by"), "owner");
    assert_eq!(value(&created, "archived_at"), "-");
    assert!(value(&created, "created_at").ends_with('Z'));

    let invited = ok(&server
        .cli(OWNER_TOKEN, &["room", "invite", "room:launch", "agent-a"])
        .await);
    assert_eq!(value(&invited, "members"), "agent-a");
    let raw = json(
        &server
            .cli(
                OWNER_TOKEN,
                &["--json", "room", "invite", "room:launch", "agent-c"],
            )
            .await,
    );
    assert_eq!(raw["members"], json!(["agent-a", "agent-c"]));
    let removed = ok(&server
        .cli(OWNER_TOKEN, &["room", "remove", "room:launch", "agent-c"])
        .await);
    assert_eq!(removed.trim(), "removed: agent-c from room:launch");
    let removed_json = json(
        &server
            .cli(
                OWNER_TOKEN,
                &["--json", "room", "remove", "room:launch", "agent-c"],
            )
            .await,
    );
    assert_eq!(
        removed_json,
        json!({"room_id": "room:launch", "agent_id": "agent-c", "removed": true})
    );

    ok(&server
        .cli(&a, &["send", "--room", "room:launch", "--body", "hi"])
        .await);
    let listed = ok(&server.cli(&a, &["room", "list"]).await);
    let rows = table(&listed);
    assert_eq!(
        rows[0],
        [
            "ROOM",
            "KIND",
            "TITLE",
            "MEMBERS",
            "LAST_AT",
            "LAST_SENDER",
            "COUNT",
            "ARCHIVED_AT"
        ]
    );
    assert_eq!(rows[1][..4], ["room:launch", "topic", "출시", "agent-a"]);
    assert_eq!(rows[1][5..], ["agent-a", "1", "-"]);
    assert_eq!(rows.len(), 3, "room:launch and all: {listed}");
    let mine = json(&server.cli(&c, &["--json", "room", "list"]).await);
    let ids: Vec<&str> = mine["rooms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|room| room["room_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["all"]);

    let archived = ok(&server
        .cli(OWNER_TOKEN, &["room", "archive", "room:launch"])
        .await);
    assert!(value(&archived, "archived_at").ends_with('Z'));
    assert_server_error(
        &server
            .cli(&a, &["send", "--room", "room:launch", "--body", "late"])
            .await,
        "room_archived",
    );
    assert_server_error(
        &server.cli(&a, &["room", "create", "room:mine"]).await,
        "forbidden",
    );
    assert_server_error(
        &server
            .cli(OWNER_TOKEN, &["room", "create", "room:launch"])
            .await,
        "room_id_taken",
    );
    assert_server_error(
        &server
            .cli(OWNER_TOKEN, &["room", "invite", "room:missing", "agent-a"])
            .await,
        "unknown_room",
    );
    server.stop().await;
}

#[tokio::test]
async fn room_arguments_are_checked_before_any_request() {
    let capture = CaptureServer::start(vec![(200, json!({}))]);
    let long_title = "가".repeat(platform_wire::MAX_ROOM_TITLE_CHARS + 1);
    let invalid: [&[&str]; 7] = [
        &["room", "create", "all"],
        &["room", "create", "dm:agent-a:agent-b"],
        &["room", "create", "room:Launch"],
        &["room", "create", "room:ok", "--title", long_title.as_str()],
        &["room", "invite", "room:ok", "Bad"],
        &["room", "remove", "nope", "agent-a"],
        &["rooms"],
    ];
    for args in invalid {
        let mut full = vec!["--url", capture.url.as_str(), "--token", "t"];
        full.extend_from_slice(args);
        let output = platform(&full).await;
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(stdout(&output).is_empty());
    }
    let post = platform(&[
        "--url",
        capture.url.as_str(),
        "--token",
        "t",
        "room",
        "post",
        "all",
        "--as",
        "human",
        "--body",
        "x",
    ])
    .await;
    assert_eq!(post.status.code(), Some(2), "room post is gone");
    assert!(capture.captured().is_empty());
}

#[tokio::test]
async fn receiver_set_takes_a_wake_policy() {
    let server = Platformd::start().await;
    let a = server.register("agent-a").await;
    let set = ok(&server
        .cli(
            &a,
            &[
                "receiver",
                "set",
                "agent-a",
                "http://127.0.0.1:9/n",
                "--wake-policy",
                "always",
            ],
        )
        .await);
    assert_eq!(value(&set, "wake_policy"), "always");
    let kept = ok(&server
        .cli(&a, &["receiver", "set", "agent-a", "http://127.0.0.1:9/m"])
        .await);
    assert_eq!(value(&kept, "wake_policy"), "always", "omitted means kept");
    let bad = server
        .cli(
            &a,
            &[
                "receiver",
                "set",
                "agent-a",
                "http://127.0.0.1:9/n",
                "--wake-policy",
                "never",
            ],
        )
        .await;
    assert_eq!(bad.status.code(), Some(2), "{}", stderr(&bad));
    server.stop().await;
}

#[tokio::test]
async fn inbox_history_and_message_show_print_the_event_fields() {
    let server = Platformd::start().await;
    let a = server.register("agent-a").await;
    let b = server.register("agent-b").await;
    let first = ok(&server
        .cli(&a, &["send", "--to", "agent-b", "--body", "q"])
        .await);
    let first_id = value(&first, "message_id");
    ok(&server
        .cli(
            &b,
            &[
                "send",
                "--reply-to",
                &first_id,
                "--quiet",
                "--mention",
                "agent-a",
                "--body",
                "a",
            ],
        )
        .await);

    let inbox = ok(&server.cli(&a, &["inbox"]).await);
    let rows = table(&inbox);
    assert_eq!(
        rows[0],
        [
            "INBOX_SEQ",
            "KIND",
            "ROOM_ID",
            "EVENT_SEQ",
            "MESSAGE_ID",
            "CREATED_AT",
            "SENDER_ID",
            "WAKE",
            "WOKE",
            "REPLY_TO",
            "MENTIONS",
            "THREAD_ID",
            "CORRECTS",
            "BODY"
        ]
    );
    assert_eq!(rows[1][..3], ["1", "message", "dm:agent-a:agent-b"]);
    // a's own row never wakes a; b's quiet answer names a, so it did. S15:
    // the answer is in the thread of a's message.
    assert_eq!(
        rows[1][6..],
        ["agent-a", "wake", "false", "-", "-", "-", "-", "q"]
    );
    assert_eq!(
        rows[2][6..],
        [
            "agent-b",
            "quiet",
            "true",
            first_id.as_str(),
            "agent-a",
            first_id.as_str(),
            "-",
            "a"
        ]
    );

    let history = ok(&server
        .cli(
            &a,
            &["history", "--room", "dm:agent-a:agent-b", "--after", "0"],
        )
        .await);
    let rows = table(&history);
    assert_eq!(
        rows[0],
        [
            "EVENT_SEQ",
            "MESSAGE_ID",
            "CREATED_AT",
            "SENDER_ID",
            "WAKE",
            "REPLY_TO",
            "MENTIONS",
            "THREAD_ID",
            "CORRECTS",
            "REPLIES",
            "BODY"
        ]
    );
    assert_eq!(
        rows[1][7..],
        ["-", "-", "1", "q"],
        "S15: the root's replies"
    );
    assert_eq!(
        rows[2][3..],
        [
            "agent-b",
            "quiet",
            first_id.as_str(),
            "agent-a",
            first_id.as_str(),
            "-",
            "-",
            "a"
        ]
    );

    let shown = ok(&server.cli(&a, &["message", "show", &first_id]).await);
    assert_eq!(value(&shown, "room_kind"), "dm");
    assert_eq!(value(&shown, "wake"), "true");
    assert_eq!(value(&shown, "mentions"), "-");
    server.stop().await;
}

#[tokio::test]
async fn inbox_all_pages_json_keeps_unknown_kinds_and_fields() {
    let message = json!({
        "inbox_seq": "1", "kind": "message", "woke": true, "message_id": "msg-01J00000000000000000000000",
        "event_seq": "5", "room_id": "room:launch", "room_kind": "topic", "sender_id": "agent-b",
        "body": "x", "reply_to": null, "wake": true, "mentions": [],
        "created_at": "2026-10-20T09:00:00.000Z", "reactions": []
    });
    let reaction = json!({"inbox_seq": "2", "kind": "reaction", "emoji": "👍"});
    let first = json!({
        "agent_id": "agent-a", "read_marker": "0", "snapshot_head_seq": "2",
        "items": [message], "next_after_seq": "1", "has_more": true
    });
    let second = json!({
        "agent_id": "agent-a", "read_marker": "0", "snapshot_head_seq": "2",
        "items": [reaction], "next_after_seq": "2", "has_more": false
    });
    let capture = CaptureServer::start(vec![(200, first), (200, second)]);

    let output = platform(&[
        "--url",
        capture.url.as_str(),
        "--token",
        "t",
        "--json",
        "inbox",
        "--all-pages",
        "--limit",
        "1",
    ])
    .await;
    let document = json(&output);
    assert_eq!(document["items"], json!([message, reaction]), "verbatim");
    assert_eq!(document["snapshot_head_seq"], "2");

    let capture = CaptureServer::start(vec![(
        200,
        json!({"agent_id": "agent-a", "read_marker": "0", "snapshot_head_seq": "2",
               "items": [reaction], "next_after_seq": "2", "has_more": false}),
    )]);
    let text = ok(&platform(&["--url", capture.url.as_str(), "--token", "t", "inbox"]).await);
    let rows = table(&text);
    assert_eq!(
        rows[1],
        [
            "2", "reaction", "-", "-", "-", "-", "-", "-", "-", "-", "-", "-", "-", "-"
        ]
    );
}
