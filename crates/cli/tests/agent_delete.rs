//! S12 CLI: `agent delete <agent_id> [--yes]` through the real binary
//! against an in-process platformd (`IF-CLI-001` S12 additions).

mod common;

use common::{OWNER_TOKEN, Platformd, assert_server_error, json, ok, stderr, stdout};

fn assert_not_confirmed(output: &std::process::Output) {
    assert_eq!(output.status.code(), Some(2), "stderr: {}", stderr(output));
    assert!(stdout(output).is_empty());
    assert!(
        stderr(output).contains("not confirmed"),
        "{}",
        stderr(output)
    );
}

/// The directory's agent ids, from `agent list --json`.
async fn listed(server: &Platformd) -> Vec<String> {
    let directory = json(&server.cli(OWNER_TOKEN, &["--json", "agent", "list"]).await);
    directory["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["agent_id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_with_yes_prints_the_id_and_the_agent_is_gone() {
    let server = Platformd::start_with_owner(&["human"]).await;
    let key = server.register("agent-z").await;
    server.register("agent-y").await;

    let out = ok(&server
        .cli(OWNER_TOKEN, &["agent", "delete", "agent-z", "--yes"])
        .await);
    assert_eq!(out, "deleted: agent-z\n");
    assert_eq!(listed(&server).await, ["agent-y"]);
    assert_server_error(&server.cli(&key, &["inbox"]).await, "unauthorized");

    let body = json(
        &server
            .cli(
                OWNER_TOKEN,
                &["--json", "agent", "delete", "agent-y", "--yes"],
            )
            .await,
    );
    assert_eq!(
        body,
        serde_json::json!({"agent_id": "agent-y", "deleted": true})
    );

    assert_server_error(
        &server
            .cli(OWNER_TOKEN, &["agent", "delete", "agent-y", "--yes"])
            .await,
        "agent_not_found",
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_yes_the_typed_agent_id_confirms() {
    let server = Platformd::start().await;
    server.register("agent-z").await;
    server.register("agent-y").await;

    // No stdin (EOF) and a different answer both refuse without a request.
    assert_not_confirmed(
        &server
            .cli(OWNER_TOKEN, &["agent", "delete", "agent-z"])
            .await,
    );
    for answer in ["y\n", "agent-y\n", "AGENT-Z\n", "\n"] {
        assert_not_confirmed(
            &server
                .cli_with_stdin(
                    OWNER_TOKEN,
                    &["agent", "delete", "agent-z"],
                    answer.as_bytes(),
                )
                .await,
        );
    }
    assert_eq!(listed(&server).await, ["agent-y", "agent-z"]);

    let confirmed = server
        .cli_with_stdin(
            OWNER_TOKEN,
            &["agent", "delete", "agent-z"],
            b"  agent-z  \n",
        )
        .await;
    assert_eq!(ok(&confirmed), "deleted: agent-z\n");
    assert!(
        stderr(&confirmed).contains("agent-z"),
        "the prompt names the agent"
    );
    assert_eq!(listed(&server).await, ["agent-y"]);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn agents_and_protected_ids_are_refused() {
    let server = Platformd::start_with_owner(&["human"]).await;
    server.register("human").await;
    let key = server.register("agent-z").await;

    assert_server_error(
        &server
            .cli(&key, &["agent", "delete", "agent-z", "--yes"])
            .await,
        "forbidden",
    );
    assert_server_error(
        &server
            .cli(OWNER_TOKEN, &["agent", "delete", "human", "--yes"])
            .await,
        "agent_protected",
    );
    assert_eq!(listed(&server).await, ["agent-z", "human"]);
    server.stop().await;
}
