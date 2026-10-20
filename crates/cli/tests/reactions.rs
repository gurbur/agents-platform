//! S16 CLI reactions (`IF-CLI-001` S16): `react`, `unreact`,
//! `react --list [--all]`, `reaction-kind set`, the reaction summary in
//! `history` and `message show`, the `reaction` row of `inbox`, and the
//! `claimed_by`/`done_by` lines of `thread show`.

mod common;

use common::{
    CaptureServer, OWNER_TOKEN, Platformd, assert_server_error, json, ok, platform, stderr, value,
};
use serde_json::json;

const MESSAGE_ID: &str = "msg-01J00000000000000000000000";

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

fn canned_react() -> serde_json::Value {
    json!({
        "message_id": MESSAGE_ID, "agent_id": "agent-b", "kind": "thumbsup", "change_seq": "7",
        "created_at": "2026-10-20T09:01:00.000Z",
        "reactions": [
            {"kind": "thumbsup", "count": 2, "agents": ["agent-b", "human"]},
            {"kind": "eyes", "count": 1, "agents": ["agent-c"]}
        ]
    })
}

fn canned_kind() -> serde_json::Value {
    json!({"slug": "party", "glyph": "🎉", "label": "축하", "sort": 80, "enabled": true})
}

#[tokio::test]
async fn arguments_are_checked_before_any_request() {
    let capture = CaptureServer::start(vec![(200, canned_react())]);
    for args in [
        &["react", "nope", "thumbsup"][..],
        &["react", MESSAGE_ID, "Bad"],
        &["react", MESSAGE_ID, "a"],
        &["react", MESSAGE_ID],
        &["react"],
        &["react", "--list", MESSAGE_ID, "thumbsup"],
        &["react", "--all"],
        &["react", MESSAGE_ID, "thumbsup", "--as", "Bad"],
        &["unreact", MESSAGE_ID, "Bad"],
        &["unreact", "nope", "eyes"],
        &[
            "reaction-kind",
            "set",
            "Bad",
            "--glyph",
            "🎉",
            "--label",
            "x",
        ],
        &[
            "reaction-kind",
            "set",
            "party",
            "--glyph",
            "a b",
            "--label",
            "x",
        ],
        &[
            "reaction-kind",
            "set",
            "party",
            "--glyph",
            "",
            "--label",
            "x",
        ],
        &[
            "reaction-kind",
            "set",
            "party",
            "--glyph",
            "🎉🎉🎉🎉🎉",
            "--label",
            "x",
        ],
        &[
            "reaction-kind",
            "set",
            "party",
            "--glyph",
            "🎉",
            "--label",
            " ",
        ],
        &[
            "reaction-kind",
            "set",
            "party",
            "--glyph",
            "🎉",
            "--label",
            "a\nb",
        ],
        &["reaction-kind", "set", "party", "--glyph", "🎉"],
        &[
            "reaction-kind",
            "set",
            "party",
            "--glyph",
            "🎉",
            "--label",
            "x",
            "--sort",
            "-1",
        ],
        &[
            "reaction-kind",
            "set",
            "party",
            "--glyph",
            "🎉",
            "--label",
            "x",
            "--enabled",
            "maybe",
        ],
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
}

#[tokio::test]
async fn requests_carry_only_the_given_options() {
    let capture = CaptureServer::start(vec![
        (201, canned_react()),
        (200, canned_react()),
        (201, canned_react()),
    ]);
    let created = ok(&run(&capture.url, &["react", MESSAGE_ID, "thumbsup"]).await);
    let existing = ok(&run(&capture.url, &["react", MESSAGE_ID, "thumbsup", "--wake"]).await);
    ok(&run(
        &capture.url,
        &["react", MESSAGE_ID, "thumbsup", "--as", "human", "--wake"],
    )
    .await);
    let requests = capture.captured();
    for request in &requests {
        assert_eq!(request.method, "PUT");
        assert_eq!(
            request.target,
            format!("/v1/messages/{MESSAGE_ID}/reactions/thumbsup")
        );
    }
    assert_eq!(requests[0].json(), json!({}));
    assert_eq!(requests[1].json(), json!({"wake": true}));
    assert_eq!(
        requests[2].json(),
        json!({"wake": true, "sender_id": "human"})
    );
    assert_eq!(value(&created, "message_id"), MESSAGE_ID);
    assert_eq!(value(&created, "agent_id"), "agent-b");
    assert_eq!(value(&created, "kind"), "thumbsup");
    assert_eq!(value(&created, "change_seq"), "7");
    assert_eq!(value(&created, "created_at"), "2026-10-20T09:01:00.000Z");
    assert_eq!(value(&created, "status"), "created");
    assert_eq!(
        value(&created, "reactions"),
        "thumbsup×2(agent-b,human) eyes×1(agent-c)"
    );
    assert_eq!(value(&existing, "status"), "existing");

    // unreact: no body for an agent, the identity for the owner.
    let capture = CaptureServer::start(vec![(204, json!(null))]);
    let removed = ok(&run(&capture.url, &["unreact", MESSAGE_ID, "eyes"]).await);
    assert_eq!(removed, format!("unreacted: eyes on {MESSAGE_ID}\n"));
    let as_owner = ok(&run(
        &capture.url,
        &["--json", "unreact", MESSAGE_ID, "eyes", "--as", "human"],
    )
    .await);
    let printed: serde_json::Value = serde_json::from_str(&as_owner).unwrap();
    assert_eq!(
        printed,
        json!({"message_id": MESSAGE_ID, "kind": "eyes", "removed": true})
    );
    let requests = capture.captured();
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(
        requests[0].target,
        format!("/v1/messages/{MESSAGE_ID}/reactions/eyes")
    );
    assert!(requests[0].body.is_empty(), "an agent sends no body");
    assert_eq!(requests[1].method, "DELETE");
    assert_eq!(requests[1].json(), json!({"sender_id": "human"}));

    // reaction-kind set: glyph and label always, sort and enabled if given.
    let capture = CaptureServer::start(vec![(201, canned_kind()), (200, canned_kind())]);
    let set = ok(&run(
        &capture.url,
        &[
            "reaction-kind",
            "set",
            "party",
            "--glyph",
            "🎉",
            "--label",
            "축하",
        ],
    )
    .await);
    ok(&run(
        &capture.url,
        &[
            "reaction-kind",
            "set",
            "party",
            "--glyph",
            "🎉",
            "--label",
            "축하",
            "--sort",
            "5",
            "--enabled",
            "false",
        ],
    )
    .await);
    let requests = capture.captured();
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(requests[0].target, "/v1/reaction-kinds/party");
    assert_eq!(requests[0].json(), json!({"glyph": "🎉", "label": "축하"}));
    assert_eq!(
        requests[1].json(),
        json!({"glyph": "🎉", "label": "축하", "sort": 5, "enabled": false})
    );
    for (key, expected) in [
        ("slug", "party"),
        ("glyph", "🎉"),
        ("label", "축하"),
        ("sort", "80"),
        ("enabled", "true"),
        ("status", "created"),
    ] {
        assert_eq!(value(&set, key), expected, "{key}");
    }

    // react --list [--all]
    let list = json!({"kinds": [canned_kind()]});
    let capture = CaptureServer::start(vec![(200, list.clone()), (200, list)]);
    let listed = ok(&run(&capture.url, &["react", "--list"]).await);
    ok(&run(&capture.url, &["react", "--list", "--all"]).await);
    let targets: Vec<String> = capture
        .captured()
        .iter()
        .map(|request| request.target.clone())
        .collect();
    assert_eq!(
        targets,
        ["/v1/reaction-kinds", "/v1/reaction-kinds?all=true"]
    );
    let rows = table(&listed);
    assert_eq!(rows[0], ["SLUG", "GLYPH", "LABEL", "SORT", "ENABLED"]);
    assert_eq!(rows[1], ["party", "🎉", "축하", "80", "true"]);
}

#[tokio::test]
async fn reactions_against_a_hub() {
    let server = Platformd::start_with_owner(&["human"]).await;
    let a = server.register("agent-a").await;
    let b = server.register("agent-b").await;
    let c = server.register("agent-c").await;
    server.register("human").await;

    let sent = json(
        &server
            .cli(&a, &["--json", "send", "--room", "all", "--body", "hello"])
            .await,
    );
    let target = sent["message_id"].as_str().unwrap().to_owned();
    ok(&server
        .cli(&a, &["inbox", "read", "--through-seq", "1"])
        .await);

    let first = ok(&server.cli(&b, &["react", &target, "thumbsup"]).await);
    assert_eq!(value(&first, "status"), "created");
    assert_eq!(value(&first, "change_seq"), "1");
    assert_eq!(value(&first, "reactions"), "thumbsup×1(agent-b)");
    let again = ok(&server.cli(&b, &["react", &target, "thumbsup"]).await);
    assert_eq!(value(&again, "status"), "existing");
    assert_eq!(value(&again, "change_seq"), "1");
    let raw = json(
        &server
            .cli(&c, &["--json", "react", &target, "eyes", "--wake"])
            .await,
    );
    assert_eq!(raw["change_seq"], "2");
    assert_eq!(raw["reactions"][1]["kind"], "eyes");
    let owner = ok(&server
        .cli(
            OWNER_TOKEN,
            &["react", &target, "thumbsup", "--as", "human"],
        )
        .await);
    assert_eq!(value(&owner, "agent_id"), "human");
    assert_eq!(
        value(&owner, "reactions"),
        "thumbsup×2(agent-b,human) eyes×1(agent-c)"
    );
    assert_server_error(
        &server.cli(&b, &["react", &target, "nosuchkind"]).await,
        "unknown_reaction_kind",
    );

    // message show and history carry the summary.
    let shown = ok(&server.cli(&c, &["message", "show", &target]).await);
    assert_eq!(
        value(&shown, "reactions"),
        "thumbsup×2(agent-b,human) eyes×1(agent-c)"
    );
    let history = ok(&server
        .cli(&c, &["history", "--room", "all", "--after", "0"])
        .await);
    let header = table(&history)[0].clone();
    assert_eq!(
        header[9..],
        ["REPLIES", "REACTIONS", "BODY"],
        "REACTIONS between REPLIES and BODY"
    );
    let row = history.lines().find(|line| line.contains(&target)).unwrap();
    assert!(
        row.ends_with("thumbsup×2(agent-b,human) eyes×1(agent-c)  hello"),
        "{row}"
    );

    // The author's inbox shows the reaction events.
    let inbox = ok(&server.cli(&a, &["inbox"]).await);
    let rows = table(&inbox);
    let reactions: Vec<&Vec<&str>> = rows
        .iter()
        .filter(|row| row.get(1) == Some(&"reaction"))
        .collect();
    assert_eq!(reactions.len(), 3, "{inbox}");
    let first_row = reactions[0];
    assert_eq!(first_row[0], "2");
    assert_eq!(first_row[2], "all");
    assert_eq!(first_row[3], "-", "no event_seq");
    assert_eq!(first_row[4], target);
    assert!(first_row[5].ends_with('Z'));
    assert_eq!(first_row[6], "agent-b");
    assert_eq!(first_row[7], "quiet");
    assert_eq!(first_row[8..], ["-", "-", "-", "-", "-", "+thumbsup"]);
    assert_eq!(reactions[1][6], "agent-c");
    assert_eq!(reactions[1][7], "wake", "--wake");
    assert_eq!(reactions[1][13], "+eyes");
    assert_eq!(reactions[2][6], "human");

    // unreact
    let removed = ok(&server.cli(&b, &["unreact", &target, "thumbsup"]).await);
    assert_eq!(removed, format!("unreacted: thumbsup on {target}\n"));
    ok(&server.cli(&b, &["unreact", &target, "thumbsup"]).await);
    ok(&server
        .cli(
            OWNER_TOKEN,
            &["unreact", &target, "thumbsup", "--as", "human"],
        )
        .await);
    let shown = ok(&server.cli(&c, &["message", "show", &target]).await);
    assert_eq!(value(&shown, "reactions"), "eyes×1(agent-c)");
    let plain = json(
        &server
            .cli(&a, &["--json", "send", "--room", "all", "--body", "plain"])
            .await,
    );
    let plain_id = plain["message_id"].as_str().unwrap();
    let shown = ok(&server.cli(&c, &["message", "show", plain_id]).await);
    assert_eq!(value(&shown, "reactions"), "-");
    server.stop().await;
}

#[tokio::test]
async fn the_registry_against_a_hub() {
    let server = Platformd::start_with_owner(&["human"]).await;
    let a = server.register("agent-a").await;

    let listed = ok(&server.cli(&a, &["react", "--list"]).await);
    let slugs: Vec<&str> = table(&listed)[1..].iter().map(|row| row[0]).collect();
    assert_eq!(
        slugs,
        [
            "ack", "thumbsup", "heart", "eyes", "question", "done", "claim"
        ]
    );
    let created = ok(&server
        .cli(
            OWNER_TOKEN,
            &[
                "reaction-kind",
                "set",
                "party",
                "--glyph",
                "🎉",
                "--label",
                "축하",
            ],
        )
        .await);
    assert_eq!(value(&created, "status"), "created");
    assert_eq!(value(&created, "sort"), "80");
    let listed = ok(&server.cli(&a, &["react", "--list"]).await);
    assert_eq!(
        table(&listed).last().unwrap()[..],
        ["party", "🎉", "축하", "80", "true"]
    );
    let disabled = ok(&server
        .cli(
            OWNER_TOKEN,
            &[
                "reaction-kind",
                "set",
                "party",
                "--glyph",
                "🎉",
                "--label",
                "축하",
                "--enabled",
                "false",
            ],
        )
        .await);
    assert_eq!(value(&disabled, "status"), "updated");
    assert_eq!(value(&disabled, "enabled"), "false");
    let listed = ok(&server.cli(&a, &["react", "--list"]).await);
    assert!(!listed.contains("party"));
    let all = ok(&server.cli(OWNER_TOKEN, &["react", "--list", "--all"]).await);
    assert!(
        all.lines()
            .any(|line| line.starts_with("party") && line.ends_with("false"))
    );
    assert_server_error(
        &server.cli(&a, &["react", "--list", "--all"]).await,
        "forbidden",
    );
    assert_server_error(
        &server
            .cli(
                &a,
                &[
                    "reaction-kind",
                    "set",
                    "x_kind",
                    "--glyph",
                    "🧪",
                    "--label",
                    "x",
                ],
            )
            .await,
        "forbidden",
    );
    assert_server_error(
        &server
            .cli(
                OWNER_TOKEN,
                &[
                    "reaction-kind",
                    "set",
                    "twin",
                    "--glyph",
                    "👍",
                    "--label",
                    "x",
                ],
            )
            .await,
        "glyph_taken",
    );
    server.stop().await;
}

#[tokio::test]
async fn thread_show_names_who_claimed_and_who_is_done() {
    let server = Platformd::start_with_owner(&["human"]).await;
    let a = server.register("agent-a").await;
    let b = server.register("agent-b").await;
    let c = server.register("agent-c").await;

    let root = json(
        &server
            .cli(&a, &["--json", "send", "--room", "all", "--body", "task"])
            .await,
    );
    let root_id = root["message_id"].as_str().unwrap().to_owned();
    ok(&server
        .cli(&b, &["send", "--reply-to", &root_id, "--body", "me?"])
        .await);
    let shown = ok(&server.cli(&c, &["thread", "show", &root_id]).await);
    assert_eq!(value(&shown, "claimed_by"), "-");
    assert_eq!(value(&shown, "done_by"), "-");

    ok(&server.cli(&c, &["react", &root_id, "claim"]).await);
    ok(&server.cli(&b, &["react", &root_id, "claim"]).await);
    let shown = ok(&server.cli(&a, &["thread", "show", &root_id]).await);
    assert_eq!(value(&shown, "claimed_by"), "agent-b,agent-c");
    assert_eq!(value(&shown, "done_by"), "-");
    ok(&server.cli(&c, &["react", &root_id, "done"]).await);
    let shown = ok(&server.cli(&a, &["thread", "show", &root_id]).await);
    assert_eq!(value(&shown, "done_by"), "agent-c");
    // `--json` stays the thread view as the hub sent it.
    let raw = json(
        &server
            .cli(&a, &["--json", "thread", "show", &root_id])
            .await,
    );
    assert!(raw.get("claimed_by").is_none());
    server.stop().await;
}
