//! S15 CLI threads and corrections (`IF-CLI-001` S15): `send --corrects`,
//! `thread show|mute|unmute`, `history --thread|--top-level`, and the new
//! inbox, history, `sent` and `message show` columns and lines.

mod common;

use common::{
    CaptureServer, OWNER_TOKEN, Platformd, assert_server_error, json, ok, platform, stderr, stdout,
    value,
};
use serde_json::json;

const MESSAGE_ID: &str = "msg-01J00000000000000000000000";
const ROOT_ID: &str = "msg-01J00000000000000000000001";

fn table(text: &str) -> Vec<Vec<&str>> {
    text.lines()
        .map(|line| line.split_whitespace().collect())
        .collect()
}

async fn run(url: &str, args: &[&str]) -> std::process::Output {
    let mut full = vec!["--url", url, "--token", "t"];
    full.extend_from_slice(args);
    platform(&full).await
}

fn canned_sent() -> serde_json::Value {
    json!({
        "message_id": MESSAGE_ID, "event_seq": "7", "room_id": "all", "room_kind": "all",
        "sender_id": "agent-a", "body": "x", "reply_to": null, "thread_id": null,
        "corrects": ROOT_ID, "wake": true, "mentions": [], "thread_summary": null,
        "created_at": "2026-10-20T09:00:00.000Z", "deduplicated": false
    })
}

#[tokio::test]
async fn send_corrects_needs_a_destination_and_a_message_id() {
    let capture = CaptureServer::start(vec![(201, canned_sent())]);
    for args in [
        &["send", "--corrects", ROOT_ID, "--body", "x"][..],
        &["send", "--room", "all", "--corrects", "nope", "--body", "x"],
    ] {
        let output = run(&capture.url, args).await;
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    assert!(capture.captured().is_empty(), "nothing was sent");

    let out = ok(&run(
        &capture.url,
        &[
            "send",
            "--room",
            "all",
            "--corrects",
            ROOT_ID,
            "--body",
            "x",
            "--client-key",
            "k",
        ],
    )
    .await);
    let body = capture.captured()[0].json();
    assert_eq!(
        body,
        json!({"room_id": "all", "body": "x", "client_key": "k", "corrects": ROOT_ID})
    );
    assert_eq!(value(&out, "thread_id"), "-");
    assert_eq!(value(&out, "corrects"), ROOT_ID);
}

#[tokio::test]
async fn threads_show_mute_and_filter_history_against_a_hub() {
    let server = Platformd::start_with_owner(&["human"]).await;
    let a = server.register("agent-a").await;
    let b = server.register("agent-b").await;
    let c = server.register("agent-c").await;
    server.register("human").await;

    let root = json(
        &server
            .cli(&a, &["--json", "send", "--room", "all", "--body", "root"])
            .await,
    );
    let root_id = root["message_id"].as_str().unwrap().to_owned();
    let reply = ok(&server
        .cli(&b, &["send", "--reply-to", &root_id, "--body", "re"])
        .await);
    assert_eq!(value(&reply, "thread_id"), root_id);
    assert_eq!(value(&reply, "corrects"), "-");
    let reply_id = value(&reply, "message_id");
    ok(&server
        .cli(
            OWNER_TOKEN,
            &[
                "send",
                "--as",
                "human",
                "--reply-to",
                &reply_id,
                "--body",
                "h",
            ],
        )
        .await);

    // thread show
    let shown = ok(&server.cli(&c, &["thread", "show", &root_id]).await);
    assert_eq!(value(&shown, "thread_id"), root_id);
    assert_eq!(value(&shown, "room_id"), "all");
    assert_eq!(value(&shown, "root_sender_id"), "agent-a");
    assert_eq!(value(&shown, "reply_count"), "2");
    assert!(value(&shown, "last_message_at").ends_with('Z'));
    let rows = table(&shown);
    let header = rows
        .iter()
        .position(|row| row.first() == Some(&"AGENT_ID"))
        .expect("participant table");
    assert_eq!(
        rows[header],
        ["AGENT_ID", "JOINED_VIA", "MUTED", "JOINED_AT"]
    );
    let participants: Vec<(&str, &str, &str)> = rows[header + 1..]
        .iter()
        .map(|row| (row[0], row[1], row[2]))
        .collect();
    assert_eq!(
        participants,
        [
            ("agent-a", "root", "false"),
            ("agent-b", "opener", "false"),
            ("human", "poster", "false")
        ]
    );
    let raw = json(
        &server
            .cli(&c, &["--json", "thread", "show", &root_id])
            .await,
    );
    assert_eq!(raw["reply_count"], 2);
    assert_server_error(
        &server.cli(&a, &["thread", "show", &reply_id]).await,
        "thread_not_found",
    );
    let malformed = server.cli(&a, &["thread", "show", "nope"]).await;
    assert_eq!(malformed.status.code(), Some(2), "{}", stderr(&malformed));

    // thread mute / unmute
    let muted = ok(&server.cli(&a, &["thread", "mute", &root_id]).await);
    assert_eq!(value(&muted, "thread_id"), root_id);
    assert_eq!(value(&muted, "agent_id"), "agent-a");
    assert_eq!(value(&muted, "joined_via"), "root");
    assert_eq!(value(&muted, "muted"), "true");
    assert!(value(&muted, "joined_at").ends_with('Z'));
    let unmuted = json(
        &server
            .cli(&a, &["--json", "thread", "unmute", &root_id])
            .await,
    );
    assert_eq!(unmuted["muted"], false);
    let owner = ok(&server
        .cli(OWNER_TOKEN, &["thread", "mute", &root_id, "--as", "human"])
        .await);
    assert_eq!(value(&owner, "agent_id"), "human");
    assert_eq!(value(&owner, "muted"), "true");
    assert_server_error(
        &server.cli(&c, &["thread", "mute", &root_id]).await,
        "thread_not_found",
    );

    // history filters and columns
    let top = ok(&server
        .cli(
            &c,
            &["history", "--room", "all", "--top-level", "--after", "0"],
        )
        .await);
    let rows = table(&top);
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
            "REACTIONS",
            "BODY"
        ]
    );
    assert_eq!(rows.len(), 3, "the root only, then the cursor line: {top}");
    assert_eq!(rows[1][1], root_id);
    assert_eq!(rows[1][7..], ["-", "-", "2", "-", "root"]);
    let thread = ok(&server
        .cli(
            &c,
            &[
                "history", "--room", "all", "--thread", &root_id, "--after", "0",
            ],
        )
        .await);
    let rows = table(&thread);
    assert_eq!(rows.len(), 5, "root and two replies: {thread}");
    assert_eq!(rows[2][1], reply_id);
    assert_eq!(rows[2][5], root_id, "reply_to");
    assert_eq!(rows[2][7..], [root_id.as_str(), "-", "-", "-", "re"]);
    let both = server
        .cli(
            &c,
            &[
                "history",
                "--room",
                "all",
                "--thread",
                &root_id,
                "--top-level",
            ],
        )
        .await;
    assert_eq!(both.status.code(), Some(2), "{}", stderr(&both));
    assert!(stdout(&both).is_empty());

    // inbox columns and message show lines
    let inbox = ok(&server.cli(&c, &["inbox"]).await);
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
    let reply_row = rows
        .iter()
        .find(|row| row.get(4) == Some(&reply_id.as_str()))
        .unwrap();
    assert_eq!(
        reply_row[8..],
        ["false", root_id.as_str(), "-", root_id.as_str(), "-", "re"]
    );
    let message = ok(&server.cli(&c, &["message", "show", &root_id]).await);
    assert_eq!(value(&message, "thread_id"), "-");
    assert_eq!(value(&message, "corrects"), "-");
    assert_eq!(value(&message, "reply_count"), "2");
    assert!(!value(&message, "last_reply_seq").starts_with('-'));
    let in_thread = ok(&server.cli(&c, &["message", "show", &reply_id]).await);
    assert_eq!(value(&in_thread, "thread_id"), root_id);
    assert_eq!(value(&in_thread, "reply_count"), "-");
    assert_eq!(value(&in_thread, "last_reply_seq"), "-");

    // A correction through the hub.
    let fixed = ok(&server
        .cli(
            &a,
            &[
                "send",
                "--room",
                "all",
                "--corrects",
                &root_id,
                "--body",
                "fix",
            ],
        )
        .await);
    assert_eq!(value(&fixed, "corrects"), root_id);
    assert_server_error(
        &server
            .cli(
                &a,
                &[
                    "send",
                    "--room",
                    "all",
                    "--corrects",
                    &reply_id,
                    "--body",
                    "x",
                ],
            )
            .await,
        "corrects_not_own",
    );
    server.stop().await;
}

#[tokio::test]
async fn thread_commands_validate_before_any_request() {
    let capture = CaptureServer::start(vec![(200, json!({}))]);
    for args in [
        &["thread", "show", "nope"][..],
        &["thread", "mute", "nope"],
        &["thread", "unmute", ROOT_ID, "--as", "Bad"],
        &["history", "--room", "all", "--thread", "nope"],
        &["history", "--thread", ROOT_ID],
    ] {
        let output = run(&capture.url, args).await;
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    assert!(capture.captured().is_empty(), "nothing was sent");

    let participant = json!({"thread_id": ROOT_ID, "agent_id": "human", "joined_via": "barged",
                             "muted": false, "joined_at": "2026-10-20T09:00:00.000Z"});
    let capture = CaptureServer::start(vec![(200, participant)]);
    ok(&run(
        &capture.url,
        &["thread", "unmute", ROOT_ID, "--as", "human"],
    )
    .await);
    let request = &capture.captured()[0];
    assert_eq!(request.method, "PUT");
    assert_eq!(
        request.target,
        format!("/v1/threads/{ROOT_ID}/participation")
    );
    assert_eq!(request.json(), json!({"wake": true, "agent_id": "human"}));

    let page = json!({"items": [], "next_before_event_seq": null, "has_more": false});
    let capture = CaptureServer::start(vec![(200, page.clone()), (200, page)]);
    ok(&run(&capture.url, &["history", "--room", "all", "--top-level"]).await);
    ok(&run(
        &capture.url,
        &["history", "--room", "all", "--thread", ROOT_ID],
    )
    .await);
    let targets: Vec<String> = capture
        .captured()
        .iter()
        .map(|request| request.target.clone())
        .collect();
    assert_eq!(
        targets,
        [
            "/v1/messages?room_id=all&top_level=true".to_owned(),
            format!("/v1/messages?room_id=all&thread_id={ROOT_ID}")
        ]
    );
}
