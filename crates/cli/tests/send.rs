//! S13 CLI `send` (`IF-CLI-001`): exactly one of `--room`, `--to`,
//! `--reply-to`; `--quiet`, `--mention` and `--as` map onto the one send
//! body; the `sent` output gains `room_kind`, `wake` and `mentions`.

mod common;

use common::{
    CaptureServer, OWNER_TOKEN, Platformd, assert_server_error, json, ok, platform, stderr, stdout,
    value,
};
use serde_json::json;

const MESSAGE_ID: &str = "msg-01J00000000000000000000000";

fn canned_message(wake: bool, mentions: &[&str]) -> serde_json::Value {
    json!({
        "message_id": MESSAGE_ID, "event_seq": "7", "room_id": "dm:agent-a:agent-b",
        "room_kind": "dm", "sender_id": "agent-a", "body": "x", "reply_to": null,
        "wake": wake, "mentions": mentions, "created_at": "2026-10-20T09:00:00.000Z",
        "deduplicated": false
    })
}

async fn run(url: &str, args: &[&str]) -> std::process::Output {
    let mut full = vec!["--url", url, "--token", "t"];
    full.extend_from_slice(args);
    platform(&full).await
}

#[tokio::test]
async fn send_needs_exactly_one_valid_destination_before_any_request() {
    let capture = CaptureServer::start(vec![(201, canned_message(true, &[]))]);
    let invalid: [&[&str]; 9] = [
        &["send", "--body", "x"],
        &["send", "--room", "all", "--to", "agent-b", "--body", "x"],
        &[
            "send",
            "--room",
            "all",
            "--reply-to",
            MESSAGE_ID,
            "--body",
            "x",
        ],
        &[
            "send",
            "--to",
            "agent-b",
            "--reply-to",
            MESSAGE_ID,
            "--body",
            "x",
        ],
        &["send", "--to", "all", "--body", "x"],
        &["send", "--room", "room:A", "--body", "x"],
        &["send", "--reply-to", "nope", "--body", "x"],
        &["send", "--room", "all", "--mention", "Bad", "--body", "x"],
        &["send", "--room", "all", "--as", "Bad", "--body", "x"],
    ];
    for args in invalid {
        let output = run(&capture.url, args).await;
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(stdout(&output).is_empty(), "{args:?}");
    }
    assert!(capture.captured().is_empty(), "nothing was sent");
}

#[tokio::test]
async fn send_bodies_carry_only_the_given_options() {
    let capture = CaptureServer::start(vec![(201, canned_message(false, &["agent-b", "agent-c"]))]);
    let cases: [(&[&str], serde_json::Value); 3] = [
        (
            &["send", "--room", "all", "--body", "x", "--client-key", "k1"],
            json!({"room_id": "all", "body": "x", "client_key": "k1"}),
        ),
        (
            &[
                "send",
                "--to",
                "agent-b",
                "--quiet",
                "--mention",
                "agent-c",
                "--mention",
                "agent-b",
                "--body",
                "x",
                "--client-key",
                "k2",
            ],
            json!({"to": "agent-b", "body": "x", "client_key": "k2", "wake": false,
                   "mentions": ["agent-c", "agent-b"]}),
        ),
        (
            &[
                "send",
                "--reply-to",
                MESSAGE_ID,
                "--as",
                "human",
                "--body",
                "x",
                "--client-key",
                "k3",
            ],
            json!({"reply_to": MESSAGE_ID, "body": "x", "client_key": "k3", "sender_id": "human"}),
        ),
    ];
    for (args, _) in &cases {
        ok(&run(&capture.url, args).await);
    }
    let captured = capture.captured();
    assert_eq!(captured.len(), cases.len());
    for (request, (args, expected)) in captured.iter().zip(&cases) {
        assert_eq!(
            (request.method.as_str(), request.target.as_str()),
            ("POST", "/v1/messages")
        );
        assert_eq!(&request.json(), expected, "{args:?}");
    }
}

#[tokio::test]
async fn the_sent_output_shows_kind_wake_and_mentions() {
    let capture = CaptureServer::start(vec![
        (201, canned_message(false, &["agent-b", "agent-c"])),
        (201, canned_message(true, &[])),
    ]);
    let quiet = ok(&run(
        &capture.url,
        &[
            "send",
            "--to",
            "agent-b",
            "--quiet",
            "--body",
            "x",
            "--client-key",
            "k",
        ],
    )
    .await);
    assert_eq!(value(&quiet, "room_kind"), "dm");
    assert_eq!(value(&quiet, "wake"), "false");
    assert_eq!(value(&quiet, "mentions"), "agent-b,agent-c");
    assert_eq!(value(&quiet, "client_key"), "k");
    let plain = ok(&run(&capture.url, &["send", "--room", "all", "--body", "x"]).await);
    assert_eq!(value(&plain, "wake"), "true");
    assert_eq!(value(&plain, "mentions"), "-");
}

#[tokio::test]
async fn owner_posts_with_as_and_the_server_judges_the_rest() {
    let server = Platformd::start_with_owner(&["human"]).await;
    let a = server.register("agent-a").await;
    server.register("agent-b").await;
    server.register("human").await;

    let posted = json(
        &server
            .cli(
                OWNER_TOKEN,
                &[
                    "--json", "send", "--as", "human", "--room", "all", "--body", "공지",
                ],
            )
            .await,
    );
    assert_eq!(posted["sender_id"], "human");
    assert_eq!(posted["room_kind"], "all");

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
                "k1",
            ],
        )
        .await);
    let dm_id = value(&dm, "message_id");
    let barge = ok(&server
        .cli(
            OWNER_TOKEN,
            &[
                "send",
                "--as",
                "human",
                "--reply-to",
                &dm_id,
                "--body",
                "잠깐",
            ],
        )
        .await);
    assert_eq!(value(&barge, "room_id"), "dm:agent-a:agent-b");
    let barge_id = value(&barge, "message_id");
    let answer = ok(&server
        .cli(
            &a,
            &[
                "send",
                "--reply-to",
                &barge_id,
                "--mention",
                "human",
                "--body",
                "네",
            ],
        )
        .await);
    assert_eq!(value(&answer, "room_id"), "dm:agent-a:agent-b");
    assert_eq!(value(&answer, "mentions"), "human");

    assert_server_error(
        &server
            .cli(
                &a,
                &["send", "--as", "agent-a", "--room", "all", "--body", "x"],
            )
            .await,
        "unexpected_field",
    );
    assert_server_error(
        &server
            .cli(
                &a,
                &[
                    "send",
                    "--room",
                    "all",
                    "--mention",
                    "agent-a",
                    "--body",
                    "x",
                ],
            )
            .await,
        "invalid_field",
    );
    assert_server_error(
        &server
            .cli(&a, &["send", "--room", "room:missing", "--body", "x"])
            .await,
        "not_room_member",
    );
    server.stop().await;
}
