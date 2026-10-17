//! S9 CLI: `agent register --name`, `profile set|set-name|show` and the
//! `agent list` columns, through the real binary against an in-process
//! platformd (`IF-CLI-001` S9 additions).

mod common;

use common::{
    OWNER_TOKEN, Platformd, assert_server_error, closed_port_addr, default_name, json, ok,
    platform, stderr, stdout, value,
};
use platform_wire::{MAX_NAME_CHARS, MAX_ROLE_CHARS};

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

#[tokio::test(flavor = "multi_thread")]
async fn register_requires_and_sends_a_name() {
    let server = Platformd::start().await;

    let missing = server
        .cli(OWNER_TOKEN, &["agent", "register", "agent-a"])
        .await;
    assert_usage_error(&missing);
    assert!(stderr(&missing).contains("--name"), "{}", stderr(&missing));
    for name in ["", "   ", "a\tb"] {
        assert_usage_error(&offline(&["agent", "register", "agent-a", "--name", name]).await);
    }
    let too_long = "가".repeat(MAX_NAME_CHARS + 1);
    assert_usage_error(&offline(&["agent", "register", "agent-a", "--name", &too_long]).await);

    let registered = server
        .cli(
            OWNER_TOKEN,
            &["agent", "register", "agent-a", "--name", " 에이전트 A "],
        )
        .await;
    let out = ok(&registered);
    assert_eq!(value(&out, "name"), "에이전트 A");
    let lines: Vec<&str> = out
        .lines()
        .map(|line| line.split(' ').next().unwrap())
        .collect();
    assert_eq!(&lines[..2], ["agent_id", "name"], "{out}");

    let as_json = json(
        &server
            .cli(
                OWNER_TOKEN,
                &["--json", "agent", "register", "agent-b", "--name", "B"],
            )
            .await,
    );
    assert_eq!(as_json["name"], "B");
    assert_eq!(as_json["profile"]["role"], serde_json::Value::Null);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_set_sends_given_fields_and_clears() {
    let server = Platformd::start().await;
    let key_a = server.register("agent-a").await;

    let set = server
        .cli_with_stdin(
            &key_a,
            &[
                "profile",
                "set",
                "agent-a",
                "--gender-form",
                "none",
                "--role",
                "배포와 운영 점검",
                "--intro-file",
                "-",
            ],
            "로그를 봅니다.\n급하면 DM으로 주세요.\n".as_bytes(),
        )
        .await;
    let out = ok(&set);
    assert_eq!(value(&out, "agent_id"), "agent-a");
    assert_eq!(value(&out, "name"), default_name("agent-a"));
    assert_eq!(value(&out, "gender_form"), "none");
    assert_eq!(value(&out, "age_band"), "-");
    assert_eq!(value(&out, "role"), "배포와 운영 점검");
    assert!(
        out.ends_with("intro\n로그를 봅니다.\n급하면 DM으로 주세요.\n"),
        "{out}"
    );

    let partial = json(
        &server
            .cli(
                &key_a,
                &[
                    "--json",
                    "profile",
                    "set",
                    "agent-a",
                    "--age-band",
                    "20s",
                    "--clear",
                    "role",
                ],
            )
            .await,
    );
    assert_eq!(partial["profile"]["age_band"], "20s");
    assert_eq!(partial["profile"]["role"], serde_json::Value::Null);
    assert_eq!(
        partial["profile"]["gender_form"], "none",
        "omitted field kept"
    );
    assert_eq!(
        partial["profile"]["intro"],
        "로그를 봅니다.\n급하면 DM으로 주세요."
    );

    let dir = tempfile::tempdir().unwrap();
    let intro = dir.path().join("intro.txt");
    std::fs::write(&intro, "파일에서 읽은 소개").unwrap();
    let from_file = json(
        &server
            .cli(
                OWNER_TOKEN,
                &[
                    "--json",
                    "profile",
                    "set",
                    "agent-a",
                    "--intro-file",
                    intro.to_str().unwrap(),
                ],
            )
            .await,
    );
    assert_eq!(from_file["profile"]["intro"], "파일에서 읽은 소개");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_set_usage_errors_send_nothing() {
    let too_long = "a".repeat(MAX_ROLE_CHARS + 1);
    for args in [
        &["profile", "set", "agent-a"][..],
        &[
            "profile", "set", "agent-a", "--role", "x", "--clear", "role",
        ],
        &["profile", "set", "agent-a", "--clear", "name"],
        &["profile", "set", "agent-a", "--gender-form", "Female"],
        &["profile", "set", "agent-a", "--age-band", "30s-40s"],
        &["profile", "set", "agent-a", "--role", &too_long],
        &["profile", "set", "agent-a", "--role", "two\nlines"],
        &["profile", "set", "agent-a", "--role", "  "],
        &["profile", "set-name", "agent-a", ""],
        &["profile", "set", "Agent-A", "--role", "x"],
    ] {
        assert_usage_error(&offline(args).await);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn set_name_is_owner_only() {
    let server = Platformd::start().await;
    let key_a = server.register("agent-a").await;

    let forbidden = server
        .cli(&key_a, &["profile", "set-name", "agent-a", "새 이름"])
        .await;
    assert_server_error(&forbidden, "forbidden");
    assert!(stdout(&forbidden).is_empty());

    let renamed = ok(&server
        .cli(
            OWNER_TOKEN,
            &["profile", "set-name", "agent-a", "에이전트 A"],
        )
        .await);
    assert_eq!(value(&renamed, "name"), "에이전트 A");
    let shown = ok(&server.cli(&key_a, &["profile", "show", "agent-a"]).await);
    assert_eq!(value(&shown, "name"), "에이전트 A");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_show_prints_one_directory_item() {
    let server = Platformd::start().await;
    let key_a = server.register("agent-a").await;
    let key_b = server.register("agent-b").await;
    ok(&server
        .cli(&key_a, &["profile", "set", "agent-a", "--role", "ops"])
        .await);

    let shown = ok(&server.cli(&key_b, &["profile", "show", "agent-a"]).await);
    let keys: Vec<&str> = shown
        .lines()
        .map(|line| line.split(' ').next().unwrap())
        .collect();
    assert_eq!(
        keys,
        [
            "agent_id",
            "name",
            "gender_form",
            "age_band",
            "role",
            "updated_at",
            "intro"
        ]
    );
    assert_eq!(value(&shown, "role"), "ops");
    assert_eq!(value(&shown, "intro"), "-");
    assert_eq!(value(&shown, "gender_form"), "-");

    let agent_json = json(
        &server
            .cli(&key_b, &["--json", "profile", "show", "agent-a"])
            .await,
    );
    assert_eq!(agent_json["agent_id"], "agent-a");
    let raw = stdout(
        &server
            .cli(&key_b, &["--json", "profile", "show", "agent-a"])
            .await,
    );
    assert!(
        raw.starts_with(r#"{"agent_id":"agent-a","name":"#),
        "the item keeps the server's key order: {raw}"
    );
    assert_eq!(agent_json["profile"]["role"], "ops");
    assert!(agent_json.get("items").is_none(), "one item, not the list");
    let owner_json = json(
        &server
            .cli(OWNER_TOKEN, &["--json", "profile", "show", "agent-a"])
            .await,
    );
    assert_eq!(owner_json["delivery"]["state"], "no_receiver");

    let missing = server.cli(&key_b, &["profile", "show", "agent-zzz"]).await;
    assert_server_error(&missing, "agent_not_found");
    assert!(stdout(&missing).is_empty());
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_list_has_name_and_role_columns_for_both_principals() {
    let server = Platformd::start().await;
    let key_a = server.register("agent-a").await;
    ok(&server
        .cli(&key_a, &["profile", "set", "agent-a", "--role", "ops"])
        .await);

    let header = |text: &str| -> Vec<String> {
        text.lines()
            .next()
            .unwrap()
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    };
    let agent_view = ok(&server.cli(&key_a, &["agent", "list"]).await);
    assert_eq!(
        header(&agent_view),
        ["AGENT_ID", "NAME", "JOINED_ALL", "ROLE"]
    );
    let row = agent_view.lines().nth(1).unwrap();
    assert!(row.starts_with("agent-a "), "{row}");
    assert!(row.contains(&default_name("agent-a")), "{row}");
    assert!(row.trim_end().ends_with("ops"), "{row}");

    let owner_view = ok(&server.cli(OWNER_TOKEN, &["agent", "list"]).await);
    assert_eq!(
        header(&owner_view),
        [
            "AGENT_ID",
            "NAME",
            "JOINED_ALL",
            "DELIVERY",
            "ROUTE_VERSION",
            "CREATED_AT",
            "RECEIVER_URL",
            "ROLE"
        ]
    );
    assert!(
        owner_view
            .lines()
            .nth(1)
            .unwrap()
            .trim_end()
            .ends_with("ops")
    );
    server.stop().await;
}
