//! Every API_V1 "CLI 매핑" command run once through the real binary against
//! an in-process platformd. Credentials come from the CLI's own output.

mod common;

use common::{
    InterposingProxy, OWNER_TOKEN, Platformd, assert_server_error, closed_port_addr, first_column,
    json, ok, platform, post_message, stderr, stdout, value,
};

const SHOWN_ONCE: &str = "shown once";

#[tokio::test(flavor = "multi_thread")]
async fn agent_register_list_inbox_and_token_rotation() {
    let server = Platformd::start().await;

    let registered = server
        .cli(
            OWNER_TOKEN,
            &["agent", "register", "agent-a", "--name", "에이전트 A"],
        )
        .await;
    let out = ok(&registered);
    let key_a = value(&out, "agent_api_key");
    assert!(key_a.starts_with("pak_"), "{out}");
    assert!(value(&out, "receiver_secret").starts_with("prs_"), "{out}");
    assert_eq!(value(&out, "joined_all"), "true");
    assert_eq!(value(&out, "receiver_url"), "-");
    assert!(stderr(&registered).contains(SHOWN_ONCE));

    let registered_json = server
        .cli(
            OWNER_TOKEN,
            &[
                "agent",
                "register",
                "agent-b",
                "--name",
                "B",
                "--receiver-url",
                "http://127.0.0.1:9/n",
                "--json",
            ],
        )
        .await;
    let body = json(&registered_json);
    assert_eq!(body["agent_id"], "agent-b");
    assert_eq!(body["receiver_url"], "http://127.0.0.1:9/n");
    assert!(stderr(&registered_json).contains(SHOWN_ONCE));

    let taken = server
        .cli(
            OWNER_TOKEN,
            &["agent", "register", "agent-a", "--name", "A"],
        )
        .await;
    assert_server_error(&taken, "agent_id_taken");

    let owner_list = ok(&server.cli(OWNER_TOKEN, &["agent", "list"]).await);
    assert!(owner_list.starts_with("AGENT_ID"), "{owner_list}");
    assert!(owner_list.contains("RECEIVER_URL"), "{owner_list}");
    assert!(owner_list.contains("http://127.0.0.1:9/n"), "{owner_list}");
    let owner_json = json(&server.cli(OWNER_TOKEN, &["agent", "list", "--json"]).await);
    assert_eq!(owner_json["items"].as_array().unwrap().len(), 2);
    assert!(owner_json["items"][0]["delivery"]["state"].is_string());

    let agent_list = ok(&server.cli(&key_a, &["agent", "list"]).await);
    let header: Vec<&str> = agent_list
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .collect();
    assert_eq!(header, ["AGENT_ID", "NAME", "JOINED_ALL", "ROLE"]);
    assert!(!agent_list.contains("RECEIVER_URL"), "{agent_list}");
    assert_eq!(agent_list.lines().count(), 3, "{agent_list}");

    ok(&server
        .cli(&key_a, &["send", "--to", "agent-b", "--body", "for b"])
        .await);
    let owner_inbox = ok(&server
        .cli(OWNER_TOKEN, &["agent", "inbox", "agent-b"])
        .await);
    assert_eq!(first_column(&owner_inbox), ["1"], "{owner_inbox}");
    assert!(owner_inbox.contains("dm:agent-a:agent-b"), "{owner_inbox}");
    let owner_inbox_json = json(
        &server
            .cli(
                OWNER_TOKEN,
                &["agent", "inbox", "agent-b", "--limit", "1", "--json"],
            )
            .await,
    );
    assert_eq!(owner_inbox_json["agent_id"], "agent-b");
    assert_eq!(owner_inbox_json["items"][0]["body"], "for b");

    let rotated = server
        .cli(OWNER_TOKEN, &["agent", "tokens", "rotate", "agent-a"])
        .await;
    let out = ok(&rotated);
    let new_key_a = value(&out, "agent_api_key");
    assert!(new_key_a.starts_with("pak_") && new_key_a != key_a, "{out}");
    assert_eq!(value(&out, "previous_keys_revoked"), "true");
    assert_eq!(value(&out, "route_version"), "2");
    assert!(stderr(&rotated).contains(SHOWN_ONCE));

    assert_server_error(
        &server.cli(&key_a, &["agent", "list"]).await,
        "unauthorized",
    );
    ok(&server.cli(&new_key_a, &["agent", "list"]).await);
    let rotated_json = json(
        &server
            .cli(
                OWNER_TOKEN,
                &["agent", "tokens", "rotate", "agent-a", "--json"],
            )
            .await,
    );
    assert_eq!(rotated_json["previous_keys_revoked"], true);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn receiver_and_all_membership() {
    let server = Platformd::start().await;
    let key_a = server.register("agent-a").await;
    let key_b = server.register("agent-b").await;
    let receiver_url = format!("http://{}/notify", closed_port_addr());

    let set = ok(&server
        .cli(OWNER_TOKEN, &["receiver", "set", "agent-a", &receiver_url])
        .await);
    assert_eq!(value(&set, "receiver_url"), receiver_url);
    assert_eq!(value(&set, "route_version"), "2");

    let unset = ok(&server.cli(&key_a, &["receiver", "unset", "agent-a"]).await);
    assert_eq!(value(&unset, "receiver_url"), "-");
    assert_eq!(value(&unset, "state"), "no_receiver");
    let unset_json = json(
        &server
            .cli(&key_a, &["receiver", "unset", "agent-a", "--json"])
            .await,
    );
    assert!(unset_json["receiver_url"].is_null());

    let left = ok(&server.cli(&key_b, &["all", "leave", "agent-b"]).await);
    assert_eq!(value(&left, "joined_all"), "false");
    assert_server_error(
        &server
            .cli(&key_b, &["send", "--room", "all", "--body", "x"])
            .await,
        "not_room_member",
    );
    let joined = json(
        &server
            .cli(&key_b, &["all", "join", "agent-b", "--json"])
            .await,
    );
    assert_eq!(joined["joined_all"], true);
    ok(&server
        .cli(&key_b, &["send", "--room", "all", "--body", "back"])
        .await);

    assert_server_error(
        &server.cli(&key_b, &["all", "leave", "agent-a"]).await,
        "forbidden",
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn send_body_sources_client_keys_and_deduplication() {
    let server = Platformd::start().await;
    let key_a = server.register("agent-a").await;
    server.register("agent-b").await;

    let first = ok(&server
        .cli(
            &key_a,
            &[
                "send",
                "--room",
                "all",
                "--body",
                "hello",
                "--client-key",
                "k1",
            ],
        )
        .await);
    assert_eq!(value(&first, "client_key"), "k1");
    assert_eq!(value(&first, "room_id"), "all");
    assert!(!first.contains("deduplicated"), "{first}");
    let message_id = value(&first, "message_id");

    let retry = ok(&server
        .cli(
            &key_a,
            &[
                "send",
                "--room",
                "all",
                "--body",
                "hello",
                "--client-key",
                "k1",
            ],
        )
        .await);
    assert!(
        retry.contains("(deduplicated — same client_key)"),
        "{retry}"
    );
    assert_eq!(value(&retry, "message_id"), message_id);
    let retry_json = json(
        &server
            .cli(
                &key_a,
                &[
                    "send",
                    "--room",
                    "all",
                    "--body",
                    "hello",
                    "--client-key",
                    "k1",
                    "--json",
                ],
            )
            .await,
    );
    assert_eq!(retry_json["deduplicated"], true);

    assert_server_error(
        &server
            .cli(
                &key_a,
                &[
                    "send",
                    "--room",
                    "all",
                    "--body",
                    "other",
                    "--client-key",
                    "k1",
                ],
            )
            .await,
        "idempotency_conflict",
    );

    let generated = ok(&server
        .cli(&key_a, &["send", "--to", "agent-b", "--body", "dm"])
        .await);
    assert_eq!(value(&generated, "room_id"), "dm:agent-a:agent-b");
    assert_eq!(value(&generated, "client_key").len(), 26, "{generated}");

    let generated_json = server
        .cli(&key_a, &["send", "--room", "all", "--body", "j", "--json"])
        .await;
    assert_eq!(json(&generated_json)["deduplicated"], false);
    let key_line = stderr(&generated_json);
    let key = key_line
        .trim_end()
        .strip_prefix("platform: client_key ")
        .unwrap_or_else(|| panic!("{key_line}"));
    assert_eq!(key.len(), 26, "{key_line}");

    let dir = tempfile::tempdir().unwrap();
    let body_path = dir.path().join("body.txt");
    std::fs::write(&body_path, "from file\n").unwrap();
    let from_file = ok(&server
        .cli(
            &key_a,
            &[
                "send",
                "--room",
                "all",
                "--body-file",
                body_path.to_str().unwrap(),
                "--json",
            ],
        )
        .await);
    let from_file: serde_json::Value = serde_json::from_str(&from_file).unwrap();
    assert_eq!(from_file["body"], "from file\n");

    let from_stdin = server
        .cli_with_stdin(
            &key_a,
            &["send", "--body-file", "-", "--reply-to", &message_id],
            "from stdin".as_bytes(),
        )
        .await;
    let out = ok(&from_stdin);
    assert_eq!(value(&out, "reply_to"), message_id);
    let shown = json(
        &server
            .cli(
                &key_a,
                &["message", "show", &value(&out, "message_id"), "--json"],
            )
            .await,
    );
    assert_eq!(shown["body"], "from stdin");

    let no_body = server.cli(&key_a, &["send", "--room", "all"]).await;
    assert_eq!(no_body.status.code(), Some(2), "{}", stderr(&no_body));
    let both = server
        .cli(
            &key_a,
            &["send", "--room", "all", "--body", "x", "--body-file", "-"],
        )
        .await;
    assert_eq!(both.status.code(), Some(2), "{}", stderr(&both));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn inbox_read_marker_history_and_message_show() {
    let server = Platformd::start().await;
    let key_a = server.register("agent-a").await;
    let key_b = server.register("agent-b").await;
    let mut ids = Vec::new();
    for body in ["one", "line1\nline2", "three"] {
        let out = ok(&server
            .cli(&key_a, &["send", "--room", "all", "--body", body])
            .await);
        ids.push(value(&out, "message_id"));
    }

    let inbox = ok(&server.cli(&key_b, &["inbox"]).await);
    assert!(inbox.starts_with("INBOX_SEQ"), "{inbox}");
    assert_eq!(first_column(&inbox), ["1", "2", "3"], "{inbox}");
    assert!(inbox.contains("line1\\nline2"), "{inbox}");
    assert!(inbox.contains("read_marker 0"), "{inbox}");
    let first_page = ok(&server.cli(&key_b, &["inbox", "--limit", "1"]).await);
    assert!(first_page.contains("next_after_seq 1"), "{first_page}");
    assert!(first_page.contains("has_more true"), "{first_page}");
    let bounded = json(
        &server
            .cli(
                &key_b,
                &["inbox", "--after-seq", "1", "--until-seq", "2", "--json"],
            )
            .await,
    );
    assert_eq!(bounded["items"].as_array().unwrap().len(), 1);
    assert_eq!(bounded["items"][0]["inbox_seq"], "2");

    let marked = ok(&server
        .cli(&key_b, &["inbox", "read", "--through-seq", "2"])
        .await);
    assert_eq!(value(&marked, "read_marker"), "2");
    assert_eq!(
        first_column(&ok(&server.cli(&key_b, &["inbox"]).await)),
        ["3"]
    );
    let no_op = json(
        &server
            .cli(&key_b, &["inbox", "read", "--through-seq", "1", "--json"])
            .await,
    );
    assert_eq!(no_op["read_marker"], "2");
    assert_server_error(
        &server
            .cli(&key_b, &["inbox", "read", "--through-seq", "99"])
            .await,
        "read_marker_ahead",
    );

    let newest_first = ok(&server.cli(&key_b, &["history", "--room", "all"]).await);
    assert!(newest_first.starts_with("EVENT_SEQ"), "{newest_first}");
    assert_eq!(
        first_column(&newest_first),
        ["3", "2", "1"],
        "{newest_first}"
    );
    assert!(newest_first.contains("line1\\nline2"), "{newest_first}");
    let oldest_first = ok(&server
        .cli(&key_b, &["history", "--room", "all", "--after", "1"])
        .await);
    assert_eq!(first_column(&oldest_first), ["2", "3"], "{oldest_first}");
    assert!(
        oldest_first.contains("next_after_event_seq 3"),
        "{oldest_first}"
    );
    let before = ok(&server
        .cli(
            &key_b,
            &["history", "--room", "all", "--before", "3", "--limit", "1"],
        )
        .await);
    assert_eq!(first_column(&before), ["2"], "{before}");
    assert!(before.contains("next_before_event_seq 2"), "{before}");
    assert!(before.contains("has_more true"), "{before}");
    let searched = json(
        &server
            .cli(
                &key_b,
                &["history", "--room", "all", "--q", "line1\nline", "--json"],
            )
            .await,
    );
    assert_eq!(searched["items"].as_array().unwrap().len(), 1);
    assert_eq!(searched["items"][0]["message_id"], ids[1].as_str());
    let both_cursors = server
        .cli(
            &key_b,
            &["history", "--room", "all", "--after", "1", "--before", "3"],
        )
        .await;
    assert_eq!(both_cursors.status.code(), Some(2));
    assert_server_error(
        &server
            .cli(&key_b, &["history", "--room", "dm:agent-a:agent-c"])
            .await,
        "forbidden",
    );

    let shown = ok(&server.cli(&key_b, &["message", "show", &ids[1]]).await);
    assert_eq!(value(&shown, "body"), "line1\\nline2");
    assert_eq!(value(&shown, "room_id"), "all");
    assert_server_error(
        &server
            .cli(
                &key_b,
                &["message", "show", "msg-00000000000000000000000000"],
            )
            .await,
        "message_not_found",
    );
    server.stop().await;
}

/// The traversal must stop at the first page's head even though a message
/// is committed between its pages.
#[tokio::test(flavor = "multi_thread")]
async fn inbox_all_pages_reads_one_snapshot() {
    let server = Platformd::start().await;
    let key_a = server.register("agent-a").await;
    let key_b = server.register("agent-b").await;
    let before_traversal = ["m1", "m2", "m3", "m4", "m5"];
    for body in before_traversal {
        ok(&server
            .cli(&key_a, &["send", "--room", "all", "--body", body])
            .await);
    }
    let upstream = server.addr;
    let late_sender = key_a.clone();
    let proxy = InterposingProxy::start(
        upstream,
        Box::new(move || post_message(upstream, &late_sender, "all", "late", "late-key")),
    );

    let output = platform(&[
        "--url",
        &proxy.url,
        "--token",
        &key_b,
        "inbox",
        "--all-pages",
        "--limit",
        "2",
        "--json",
    ])
    .await;

    let snapshot = json(&output);
    assert_eq!(snapshot["agent_id"], "agent-b");
    assert_eq!(snapshot["read_marker"], "0");
    let head = before_traversal.len().to_string();
    assert_eq!(snapshot["snapshot_head_seq"], head.as_str());
    let seqs: Vec<&str> = snapshot["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["inbox_seq"].as_str().unwrap())
        .collect();
    let expected: Vec<String> = (1..=before_traversal.len())
        .map(|n| n.to_string())
        .collect();
    assert_eq!(
        seqs, expected,
        "no gaps, no duplicates, nothing past the head"
    );

    let targets = proxy.targets();
    assert!(targets.len() > 1, "{targets:?}");
    assert!(!targets[0].contains("until_seq"), "{targets:?}");
    for later in &targets[1..] {
        assert!(later.contains(&format!("until_seq={head}")), "{targets:?}");
    }

    // The late message did land, and the traversal moved no marker.
    let after = json(&server.cli(&key_b, &["inbox", "--json"]).await);
    let head_after: u64 = after["snapshot_head_seq"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(head_after > before_traversal.len() as u64, "{after}");
    assert_eq!(after["read_marker"], "0");

    let human = ok(&server
        .cli(&key_b, &["inbox", "--all-pages", "--limit", "2"])
        .await);
    assert_eq!(first_column(&human).len() as u64, head_after, "{human}");
    assert!(human.contains(&format!("items {head_after}")), "{human}");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn delivery_status_and_retry() {
    let server = Platformd::start().await;
    let key_a = server.register("agent-a").await;
    let key_b = server.register("agent-b").await;

    let status = ok(&server.cli(&key_a, &["delivery", "agent-a"]).await);
    assert_eq!(value(&status, "state"), "no_receiver");
    assert_eq!(value(&status, "receiver_url_present"), "false");
    assert_eq!(value(&status, "last_error"), "-");
    let status_json = json(
        &server
            .cli(OWNER_TOKEN, &["delivery", "agent-a", "--json"])
            .await,
    );
    assert_eq!(status_json["state"], "no_receiver");

    let retry = ok(&server.cli(&key_a, &["delivery", "retry", "agent-a"]).await);
    assert_eq!(value(&retry, "state"), "no_receiver");
    let retry_json = json(
        &server
            .cli(OWNER_TOKEN, &["delivery", "retry", "agent-a", "--json"])
            .await,
    );
    assert_eq!(retry_json["state"], "no_receiver");

    assert_server_error(
        &server.cli(&key_b, &["delivery", "agent-a"]).await,
        "forbidden",
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_codes_for_api_commands() {
    let server = Platformd::start().await;

    let unauthorized = server.cli("not-a-key", &["inbox"]).await;
    assert_server_error(&unauthorized, "unauthorized");
    assert!(stdout(&unauthorized).is_empty());

    let invalid_id = server
        .cli(
            OWNER_TOKEN,
            &["agent", "register", "Agent-A", "--name", "A"],
        )
        .await;
    assert_eq!(invalid_id.status.code(), Some(2), "{}", stderr(&invalid_id));
    let unknown = server.cli(OWNER_TOKEN, &["agent", "frobnicate"]).await;
    assert_eq!(unknown.status.code(), Some(2));

    let closed = format!("http://{}", closed_port_addr());
    let network = platform(&["--url", &closed, "--token", "x", "agent", "list"]).await;
    assert_eq!(network.status.code(), Some(3), "{}", stderr(&network));
    assert!(stderr(&network).starts_with("platform: network error"));
    server.stop().await;
}
