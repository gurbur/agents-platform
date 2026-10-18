//! S10 CLI: `platform rooms` and `platform room post` against an in-process
//! platformd with `[owner] post_as = ["human"]`.

mod common;

use common::{OWNER_TOKEN, Platformd, assert_server_error, json, ok, stderr, stdout, value};

#[tokio::test]
async fn rooms_lists_every_room_as_a_table_and_as_json() {
    let server = Platformd::start_with_owner(&["human"]).await;
    let a = server.register("agent-a").await;
    server.register("agent-b").await;
    ok(&server
        .cli(
            &a,
            &[
                "send",
                "--to",
                "agent-b",
                "--body",
                "hi",
                "--client-key",
                "k-1",
            ],
        )
        .await);

    let table = ok(&server.cli(OWNER_TOKEN, &["rooms"]).await);
    let lines: Vec<Vec<&str>> = table
        .lines()
        .map(|line| line.split_whitespace().collect())
        .collect();
    assert_eq!(
        lines[0],
        ["ROOM", "KIND", "MEMBERS", "LAST_AT", "LAST_SENDER", "COUNT"],
        "{table}"
    );
    assert_eq!(lines.len(), 3, "{table}");
    assert_eq!(
        lines[1][..3],
        ["dm:agent-a:agent-b", "dm", "agent-a,agent-b"]
    );
    assert_eq!(lines[1][4..], ["agent-a", "1"]);
    assert!(lines[1][3].ends_with('Z'), "{table}");
    assert_eq!(lines[2], ["all", "all", "agent-a,agent-b", "-", "-", "0"]);

    let raw = json(&server.cli(OWNER_TOKEN, &["--json", "rooms"]).await);
    assert_eq!(raw["rooms"][0]["room_id"], "dm:agent-a:agent-b");
    assert_eq!(raw["rooms"][1]["last_event_seq"], serde_json::Value::Null);

    assert_server_error(&server.cli(&a, &["rooms"]).await, "forbidden");
    server.stop().await;
}

#[tokio::test]
async fn room_post_sends_as_the_allowlisted_agent() {
    let server = Platformd::start_with_owner(&["human"]).await;
    let a = server.register("agent-a").await;
    let b = server.register("agent-b").await;
    let human = server.register("human").await;
    let dm = ok(&server
        .cli(
            &a,
            &[
                "send",
                "--to",
                "agent-b",
                "--body",
                "hi",
                "--client-key",
                "k-1",
            ],
        )
        .await);
    let dm_id = value(&dm, "message_id");

    let posted = ok(&server
        .cli(
            OWNER_TOKEN,
            &[
                "room",
                "post",
                "dm:agent-a:agent-b",
                "--as",
                "human",
                "--body",
                "잠깐만",
                "--reply-to",
                &dm_id,
                "--client-key",
                "barge-1",
            ],
        )
        .await);
    assert_eq!(value(&posted, "room_id"), "dm:agent-a:agent-b");
    assert_eq!(value(&posted, "sender_id"), "human");
    assert_eq!(value(&posted, "reply_to"), dm_id);
    assert_eq!(value(&posted, "client_key"), "barge-1");
    let message_id = value(&posted, "message_id");

    for key in [&a, &b] {
        let inbox = json(&server.cli(key, &["--json", "inbox"]).await);
        let items = inbox["items"].as_array().unwrap();
        assert!(
            items
                .iter()
                .any(|item| item["message_id"] == message_id.as_str()
                    && item["sender_id"] == "human"),
            "{inbox}"
        );
    }
    let human_inbox = json(&server.cli(&human, &["--json", "inbox"]).await);
    assert_eq!(human_inbox["items"], serde_json::json!([]));

    let replay = ok(&server
        .cli(
            OWNER_TOKEN,
            &[
                "room",
                "post",
                "dm:agent-a:agent-b",
                "--as",
                "human",
                "--body",
                "잠깐만",
                "--reply-to",
                &dm_id,
                "--client-key",
                "barge-1",
            ],
        )
        .await);
    assert_eq!(value(&replay, "message_id"), message_id);
    assert!(replay.contains("deduplicated"), "{replay}");

    let generated = server
        .cli(
            OWNER_TOKEN,
            &[
                "--json", "room", "post", "all", "--as", "human", "--body", "to all",
            ],
        )
        .await;
    let body = json(&generated);
    assert_eq!(body["room_id"], "all");
    assert_eq!(body["sender_id"], "human");
    let key_line = stderr(&generated);
    let key = key_line
        .trim()
        .strip_prefix("platform: client_key ")
        .unwrap_or_else(|| panic!("generated key on stderr: {key_line}"));
    assert_eq!(key.len(), 26, "a ULID: {key}");
    server.stop().await;
}

#[tokio::test]
async fn room_post_errors_and_exit_codes() {
    let server = Platformd::start_with_owner(&["human"]).await;
    let a = server.register("agent-a").await;
    server.register("agent-b").await;
    server.register("human").await;

    let agent_key = server
        .cli(&a, &["room", "post", "all", "--as", "human", "--body", "x"])
        .await;
    assert_server_error(&agent_key, "forbidden");
    let not_allowed = server
        .cli(
            OWNER_TOKEN,
            &["room", "post", "all", "--as", "agent-a", "--body", "x"],
        )
        .await;
    assert_server_error(&not_allowed, "forbidden");
    let unknown = server
        .cli(
            OWNER_TOKEN,
            &[
                "room",
                "post",
                "dm:agent-a:ghost",
                "--as",
                "human",
                "--body",
                "x",
            ],
        )
        .await;
    assert_server_error(&unknown, "unknown_room");

    let invalid: [&[&str]; 4] = [
        &[
            "room",
            "post",
            "dm:agent-b:agent-a",
            "--as",
            "human",
            "--body",
            "x",
        ],
        &["room", "post", "all", "--as", "Human", "--body", "x"],
        &[
            "room",
            "post",
            "all",
            "--as",
            "human",
            "--body",
            "x",
            "--reply-to",
            "nope",
        ],
        &["room", "post", "all", "--as", "human"],
    ];
    for args in invalid {
        let output = server.cli(OWNER_TOKEN, args).await;
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(stdout(&output).is_empty());
    }
    server.stop().await;
}
