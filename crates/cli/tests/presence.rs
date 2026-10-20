//! S14 CLI (`IF-CLI-001` presence): `presence set|clear` through the real
//! binary, the local pre-checks, the output, and the presence columns of
//! `agent list` and lines of `profile show`.

mod common;

use common::{
    CaptureServer, OWNER_TOKEN, Platformd, assert_server_error, closed_port_addr, json, ok,
    platform, stderr, stdout, value,
};
use platform_wire::MAX_PRESENCE_NOTE_CHARS;
use serde_json::json;

/// Runs against a port nothing listens on: exit 2 proves the CLI refused
/// before sending anything (a request would exit 3).
async fn offline(args: &[&str]) -> std::process::Output {
    let url = format!("http://{}", closed_port_addr());
    let mut full = vec!["--url", url.as_str(), "--token", "t"];
    full.extend_from_slice(args);
    platform(&full).await
}

fn assert_usage_error(output: &std::process::Output) {
    assert_eq!(output.status.code(), Some(2), "stderr: {}", stderr(output));
    assert!(stdout(output).is_empty());
}

/// A 200 body as the hub would send it.
fn declared() -> serde_json::Value {
    json!({"agent_id": "agent-a",
           "presence": {"state": "busy", "note": "배포 점검 중",
                        "updated_at": "2026-10-20T09:00:00.000Z", "expires_at": "2026-10-20T09:05:00.000Z"},
           "last_seen_at": null})
}

#[tokio::test(flavor = "multi_thread")]
async fn presence_set_sends_only_the_given_options() {
    let capture = CaptureServer::start(vec![(200, declared())]);
    let run = |args: Vec<&'static str>| {
        let url = capture.url.clone();
        async move {
            let mut full = vec!["--url", url.as_str(), "--token", "t", "presence", "set"];
            full.extend(args);
            platform(&full).await
        }
    };

    ok(&run(vec!["agent-a", "online"]).await);
    ok(&run(vec![
        "agent-a",
        "busy",
        "--note",
        " 배포 점검 중 ",
        "--ttl",
        "300",
    ])
    .await);
    ok(&run(vec!["agent-a", "busy", "--ttl", "5"]).await);

    let requests = capture.captured();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert_eq!(request.method, "PUT");
        assert_eq!(request.target, "/v1/agents/agent-a/presence");
    }
    assert_eq!(requests[0].json(), json!({"state": "online"}));
    assert_eq!(
        requests[1].json(),
        json!({"state": "busy", "note": " 배포 점검 중 ", "ttl_secs": 300}),
        "the note goes as given; the hub trims"
    );
    assert_eq!(requests[2].json(), json!({"state": "busy", "ttl_secs": 5}));
}

#[tokio::test(flavor = "multi_thread")]
async fn presence_set_refuses_bad_input_before_sending() {
    let too_long = "a".repeat(MAX_PRESENCE_NOTE_CHARS + 1);
    let cases: Vec<Vec<&str>> = vec![
        vec!["agent-a", "offline"],
        vec!["agent-a", "Online"],
        vec!["agent-a", ""],
        vec!["agent-a", "busy", "--note", ""],
        vec!["agent-a", "busy", "--note", "  "],
        vec!["agent-a", "busy", "--note", too_long.as_str()],
        vec!["agent-a", "busy", "--note", "a\nb"],
        vec!["agent-a", "busy", "--note", "a\tb"],
        vec!["agent-a", "online", "--ttl", "0"],
        vec!["agent-a", "online", "--ttl", "-1"],
        vec!["agent-a", "online", "--ttl", "1.5"],
        vec!["agent-a", "online", "--ttl", "soon"],
        vec!["Agent-A", "online"],
    ];
    for case in cases {
        let mut args = vec!["presence", "set"];
        args.extend(case.iter().copied());
        assert_usage_error(&offline(&args).await);
    }
    let at_limit = "가".repeat(MAX_PRESENCE_NOTE_CHARS);
    let capture = CaptureServer::start(vec![(200, declared())]);
    ok(&platform(&[
        "--url",
        &capture.url,
        "--token",
        "t",
        "presence",
        "set",
        "agent-a",
        "busy",
        "--note",
        &at_limit,
    ])
    .await);
}

#[tokio::test(flavor = "multi_thread")]
async fn presence_set_and_clear_against_the_hub() {
    let server = Platformd::start().await;
    let key_a = server.register("agent-a").await;
    let key_b = server.register("agent-b").await;

    let out = ok(&server
        .cli(
            &key_a,
            &[
                "presence",
                "set",
                "agent-a",
                "busy",
                "--note",
                "배포 점검 중",
                "--ttl",
                "300",
            ],
        )
        .await);
    let keys: Vec<&str> = out
        .lines()
        .map(|line| line.split(' ').next().unwrap())
        .collect();
    assert_eq!(
        keys,
        [
            "agent_id",
            "state",
            "note",
            "updated_at",
            "expires_at",
            "last_seen_at"
        ]
    );
    assert_eq!(value(&out, "agent_id"), "agent-a");
    assert_eq!(value(&out, "state"), "busy");
    assert_eq!(value(&out, "note"), "배포 점검 중");
    assert!(value(&out, "expires_at").ends_with('Z'), "{out}");

    let raw = json(
        &server
            .cli(
                OWNER_TOKEN,
                &["--json", "presence", "set", "agent-a", "online"],
            )
            .await,
    );
    assert_eq!(raw["presence"]["state"], "online");
    assert_eq!(raw["presence"]["note"], serde_json::Value::Null);
    assert!(raw.get("last_seen_at").is_some());
    let shown = ok(&server
        .cli(OWNER_TOKEN, &["presence", "set", "agent-a", "online"])
        .await);
    assert_eq!(value(&shown, "note"), "-");

    let forbidden = server
        .cli(&key_b, &["presence", "set", "agent-a", "online"])
        .await;
    assert_server_error(&forbidden, "forbidden");
    assert!(stdout(&forbidden).is_empty());
    let too_long = server
        .cli(
            &key_a,
            &["presence", "set", "agent-a", "online", "--ttl", "86401"],
        )
        .await;
    assert_server_error(&too_long, "ttl_too_long");

    let cleared = ok(&server.cli(&key_a, &["presence", "clear", "agent-a"]).await);
    assert_eq!(cleared, "presence cleared: agent-a\n");
    let cleared_json = json(
        &server
            .cli(OWNER_TOKEN, &["--json", "presence", "clear", "agent-a"])
            .await,
    );
    assert_eq!(
        cleared_json,
        json!({"agent_id": "agent-a",
               "presence": {"state": "offline", "note": null, "updated_at": null, "expires_at": null}})
    );
    let other = server.cli(&key_b, &["presence", "clear", "agent-a"]).await;
    assert_server_error(&other, "forbidden");
    let missing = server
        .cli(OWNER_TOKEN, &["presence", "clear", "agent-zz"])
        .await;
    assert_server_error(&missing, "agent_not_found");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_list_and_profile_show_carry_presence() {
    let server = Platformd::start().await;
    let key_a = server.register("agent-a").await;
    server.register("agent-b").await;
    ok(&server
        .cli(
            &key_a,
            &["presence", "set", "agent-a", "busy", "--note", "점검"],
        )
        .await);

    for token in [key_a.as_str(), OWNER_TOKEN] {
        let list = ok(&server.cli(token, &["agent", "list"]).await);
        let header: Vec<&str> = list.lines().next().unwrap().split_whitespace().collect();
        let presence_at = header.iter().position(|cell| *cell == "PRESENCE").unwrap();
        assert_eq!(header[presence_at + 1], "LAST_SEEN", "{list}");
        let row_a: Vec<&str> = list.lines().nth(1).unwrap().split_whitespace().collect();
        let row_b: Vec<&str> = list.lines().nth(2).unwrap().split_whitespace().collect();
        // Names are "Agent agent-x": two cells, so the columns sit one
        // further right in the rows than in the header.
        assert_eq!(row_a[presence_at + 1], "busy", "{list}");
        assert!(
            row_a[presence_at + 2].ends_with('Z'),
            "agent-a was seen: {list}"
        );
        assert_eq!(row_b[presence_at + 1], "offline", "{list}");
        assert_eq!(
            row_b[presence_at + 2],
            "-",
            "agent-b was never seen: {list}"
        );
        assert!(
            !list.contains("점검"),
            "the note is not in the table: {list}"
        );
    }

    let shown = ok(&server
        .cli(OWNER_TOKEN, &["profile", "show", "agent-a"])
        .await);
    assert_eq!(value(&shown, "presence"), "busy");
    assert_eq!(value(&shown, "presence_note"), "점검");
    assert!(value(&shown, "last_seen_at").ends_with('Z'), "{shown}");
    let never = ok(&server
        .cli(OWNER_TOKEN, &["profile", "show", "agent-b"])
        .await);
    assert_eq!(value(&never, "presence"), "offline");
    assert_eq!(value(&never, "presence_note"), "-");
    assert_eq!(value(&never, "last_seen_at"), "-");
    server.stop().await;
}
