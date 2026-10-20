//! S16 reaction kind registry (`IF-WIRE-017`, ADR-0014, API_V1
//! "GET /v1/reaction-kinds", "PUT /v1/reaction-kinds/{slug}"): seven seeded
//! kinds every principal reads, the owner's create-or-update in the
//! documented order with the glyph unique, and disabling a kind refusing
//! new reactions only.

mod common;

use common::{Api, Fixture, OWNER_TOKEN, Reply, RunningServer, ordered_keys};
use platform_wire::reactions::{MAX_GLYPH_CHARS, MAX_REACTION_LABEL_CHARS};
use reqwest::Method;
use serde_json::{Value, json};

async fn start() -> (Fixture, RunningServer, Api) {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config_with_owner(&["human"])).await;
    let api = server.api();
    (fx, server, api)
}

async fn put_kind(api: &Api, token: &str, slug: &str, body: Value) -> Reply {
    api.put(&format!("/v1/reaction-kinds/{slug}"), token, body)
        .await
}

fn slugs(list: &Value) -> Vec<String> {
    list["kinds"]
        .as_array()
        .unwrap_or_else(|| panic!("no kinds in {list}"))
        .iter()
        .map(|kind| kind["slug"].as_str().unwrap().to_owned())
        .collect()
}

const SEEDED: [(&str, &str, &str, i64); 7] = [
    ("ack", "👌", "확인했다", 10),
    ("thumbsup", "👍", "좋다", 20),
    ("heart", "❤️", "고마워", 30),
    ("eyes", "👀", "보고 있다", 40),
    ("question", "❓", "확인 필요", 50),
    ("done", "☑️", "처리 끝", 60),
    ("claim", "🙋", "맡음", 70),
];

#[tokio::test]
async fn every_principal_reads_the_seven_seeded_kinds_in_order() {
    let (_fx, server, api) = start().await;
    let agent = api.register("agent-a", None).await;
    let want: Vec<Value> = SEEDED
        .iter()
        .map(|(slug, glyph, label, sort)| {
            json!({"slug": slug, "glyph": glyph, "label": label, "sort": sort, "enabled": true})
        })
        .collect();
    for token in [OWNER_TOKEN, agent.api_key.as_str()] {
        let reply = api.get("/v1/reaction-kinds", token).await;
        assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
        assert_eq!(reply.body, json!({"kinds": want}));
    }
    let raw = api
        .send_bytes(
            Method::GET,
            "/v1/reaction-kinds",
            Some(&agent.api_key),
            &[],
            None,
        )
        .await;
    let text = raw.text();
    let at = text.find('[').unwrap() + 1;
    let end = at + text[at..].find('}').unwrap() + 1;
    assert_eq!(
        ordered_keys(&text[at..end]),
        ["slug", "glyph", "label", "sort", "enabled"]
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn only_the_owner_lists_disabled_kinds() {
    let (_fx, server, api) = start().await;
    let agent = api.register("agent-a", None).await;
    api.get("/v1/reaction-kinds?all=true", &agent.api_key)
        .await
        .assert_error(403, "forbidden");
    let off = api
        .get("/v1/reaction-kinds?all=false", &agent.api_key)
        .await;
    assert_eq!(off.status.as_u16(), 200, "{}", off.body);
    for (query, code) in [
        ("?all=yes", "invalid_field"),
        ("?all=true&all=true", "invalid_field"),
        ("?enabled=true", "unexpected_field"),
    ] {
        api.get(&format!("/v1/reaction-kinds{query}"), OWNER_TOKEN)
            .await
            .assert_error(422, code);
    }
    // Unknown parameters come before the agent's 403.
    api.get("/v1/reaction-kinds?all=true&x=1", &agent.api_key)
        .await
        .assert_error(422, "unexpected_field");
    api.send(Method::GET, "/v1/reaction-kinds", None, None)
        .await
        .assert_error(401, "unauthorized");

    let disabled = put_kind(
        &api,
        OWNER_TOKEN,
        "eyes",
        json!({"glyph": "👀", "label": "보고 있다", "enabled": false}),
    )
    .await;
    assert_eq!(disabled.status.as_u16(), 200, "{}", disabled.body);
    let enabled = api.get("/v1/reaction-kinds", OWNER_TOKEN).await;
    assert!(!slugs(&enabled.body).contains(&"eyes".to_owned()));
    let all = api.get("/v1/reaction-kinds?all=true", OWNER_TOKEN).await;
    assert_eq!(slugs(&all.body).len(), 7);
    assert_eq!(all.body["kinds"][3]["enabled"], false);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_owner_creates_and_updates_a_kind() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;

    let created = put_kind(
        &api,
        OWNER_TOKEN,
        "party",
        json!({"glyph": "🎉", "label": " 축하 "}),
    )
    .await;
    assert_eq!(created.status.as_u16(), 201, "{}", created.body);
    assert_eq!(
        created.body,
        json!({"slug": "party", "glyph": "🎉", "label": "축하", "sort": 80, "enabled": true}),
        "sort after the largest, enabled, the label trimmed"
    );
    let keys = api
        .keys_of(
            Method::PUT,
            "/v1/reaction-kinds/party",
            OWNER_TOKEN,
            Some(json!({"glyph": "🎉", "label": "축하"})),
        )
        .await;
    assert_eq!(keys, ["slug", "glyph", "label", "sort", "enabled"]);
    let listed = api.get("/v1/reaction-kinds", &a.api_key).await;
    assert_eq!(slugs(&listed.body).last().unwrap(), "party");

    // An update keeps the omitted sort and enabled; the same body is 200.
    let same = put_kind(
        &api,
        OWNER_TOKEN,
        "party",
        json!({"glyph": "🎉", "label": "축하"}),
    )
    .await;
    assert_eq!(same.status.as_u16(), 200, "{}", same.body);
    assert_eq!(same.body, created.body);
    let created_at = fx
        .read_db(|conn| {
            Ok(conn.query_row(
                "SELECT created_at FROM reaction_kinds WHERE slug = 'party'",
                [],
                |row| row.get::<_, String>(0),
            )?)
        })
        .await;
    let moved = put_kind(
        &api,
        OWNER_TOKEN,
        "party",
        json!({"glyph": "🥳", "label": "파티", "sort": 5}),
    )
    .await;
    assert_eq!(moved.status.as_u16(), 200, "{}", moved.body);
    assert_eq!(
        moved.body,
        json!({"slug": "party", "glyph": "🥳", "label": "파티", "sort": 5, "enabled": true})
    );
    let listed = api.get("/v1/reaction-kinds", &a.api_key).await;
    assert_eq!(slugs(&listed.body)[0], "party", "sort 5 comes first");
    let kept = fx
        .read_db(|conn| {
            Ok(conn.query_row(
                "SELECT created_at FROM reaction_kinds WHERE slug = 'party'",
                [],
                |row| row.get::<_, String>(0),
            )?)
        })
        .await;
    assert_eq!(kept, created_at, "an update keeps created_at");

    // A new kind may be created disabled with its own sort.
    let hidden = put_kind(
        &api,
        OWNER_TOKEN,
        "hidden",
        json!({"glyph": "🙈", "label": "숨김", "sort": 0, "enabled": false}),
    )
    .await;
    assert_eq!(hidden.status.as_u16(), 201, "{}", hidden.body);
    assert_eq!(hidden.body["sort"], 0);
    assert_eq!(hidden.body["enabled"], false);

    // Disabling refuses new reactions only.
    let target = api
        .post(
            "/v1/messages",
            &a.api_key,
            json!({"room_id": "all", "body": "x", "client_key": "x"}),
        )
        .await;
    let target = target.body["message_id"].as_str().unwrap().to_owned();
    let path = format!("/v1/messages/{target}/reactions/party");
    let reacted = api.send(Method::PUT, &path, Some(&b.api_key), None).await;
    assert_eq!(reacted.status.as_u16(), 201, "{}", reacted.body);
    let off = put_kind(
        &api,
        OWNER_TOKEN,
        "party",
        json!({"glyph": "🥳", "label": "파티", "enabled": false}),
    )
    .await;
    assert_eq!(off.status.as_u16(), 200);
    assert_eq!(off.body["sort"], 5, "kept");
    api.send(Method::PUT, &path, Some(&a.api_key), None)
        .await
        .assert_error(422, "unknown_reaction_kind");
    // Even a repeat of an existing one: the kind is checked before.
    api.send(Method::PUT, &path, Some(&b.api_key), None)
        .await
        .assert_error(422, "unknown_reaction_kind");
    let shown = api.get(&format!("/v1/messages/{target}"), &a.api_key).await;
    assert_eq!(
        shown.body["reactions"],
        json!([{"kind": "party", "count": 1, "agents": ["agent-b"]}])
    );
    let removed = api
        .send(Method::DELETE, &path, Some(&b.api_key), None)
        .await;
    assert_eq!(removed.status.as_u16(), 204);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn put_checks_run_in_the_documented_order() {
    let (_fx, server, api) = start().await;
    let agent = api.register("agent-a", None).await;
    let good = json!({"glyph": "🎉", "label": "축하"});

    api.send(
        Method::PUT,
        "/v1/reaction-kinds/party",
        None,
        Some(good.clone()),
    )
    .await
    .assert_error(401, "unauthorized");
    // The agent's 403 comes before the slug.
    put_kind(&api, &agent.api_key, "Bad", good.clone())
        .await
        .assert_error(403, "forbidden");
    for slug in ["Bad", "a", "1ab", "ab-c", &"a".repeat(33)] {
        let reply = put_kind(&api, OWNER_TOKEN, slug, good.clone()).await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some("slug"), "{slug}");
    }
    assert_eq!(
        put_kind(&api, OWNER_TOKEN, &"a".repeat(32), good.clone())
            .await
            .status
            .as_u16(),
        201,
        "32 bytes is the longest slug"
    );
    api.send_raw(
        Method::PUT,
        "/v1/reaction-kinds/party",
        Some(OWNER_TOKEN),
        Some("text/plain"),
        Some(b"{}".to_vec()),
    )
    .await
    .assert_error(415, "unsupported_media_type");
    let extra = put_kind(
        &api,
        OWNER_TOKEN,
        "party",
        json!({"glyph": "🎉", "label": "축하", "slug": "x"}),
    )
    .await;
    extra.assert_error(422, "unexpected_field");
    let max_label = "가".repeat(MAX_REACTION_LABEL_CHARS);
    let long_label = "가".repeat(MAX_REACTION_LABEL_CHARS + 1);
    let max_glyph = "🎉".repeat(MAX_GLYPH_CHARS);
    let long_glyph = "🎉".repeat(MAX_GLYPH_CHARS + 1);
    for (body, field) in [
        (json!({"label": "x"}), "glyph"),
        (json!({"glyph": "🎉"}), "label"),
        (json!({"glyph": 1, "label": "x"}), "glyph"),
        (json!({"glyph": "🎉", "label": "x", "sort": "1"}), "sort"),
        (json!({"glyph": "🎉", "label": "x", "sort": 1.5}), "sort"),
        (
            json!({"glyph": "🎉", "label": "x", "enabled": "no"}),
            "enabled",
        ),
        // Value rules: glyph, then label, then sort.
        (json!({"glyph": "", "label": ""}), "glyph"),
        (json!({"glyph": long_glyph, "label": "x"}), "glyph"),
        (json!({"glyph": "a b", "label": "x"}), "glyph"),
        (json!({"glyph": " 🎉", "label": "x"}), "glyph"),
        (json!({"glyph": "🎉\u{7}", "label": "x"}), "glyph"),
        (json!({"glyph": "🎉", "label": "  "}), "label"),
        (json!({"glyph": "🎉", "label": long_label}), "label"),
        (json!({"glyph": "🎉", "label": "a\nb"}), "label"),
        (json!({"glyph": "🎉", "label": "x", "sort": -1}), "sort"),
    ] {
        let reply = put_kind(&api, OWNER_TOKEN, "party", body.clone()).await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some(field), "{body}");
    }
    let edges = put_kind(
        &api,
        OWNER_TOKEN,
        "edges",
        json!({"glyph": max_glyph, "label": format!(" {max_label} ")}),
    )
    .await;
    assert_eq!(edges.status.as_u16(), 201, "{}", edges.body);
    assert_eq!(edges.body["label"], max_label.as_str());

    // The glyph is unique, except against the kind itself.
    put_kind(
        &api,
        OWNER_TOKEN,
        "party",
        json!({"glyph": "👍", "label": "x"}),
    )
    .await
    .assert_error(409, "glyph_taken");
    let own = put_kind(
        &api,
        OWNER_TOKEN,
        "thumbsup",
        json!({"glyph": "👍", "label": "좋아요"}),
    )
    .await;
    assert_eq!(own.status.as_u16(), 200, "{}", own.body);
    put_kind(
        &api,
        OWNER_TOKEN,
        "eyes",
        json!({"glyph": "👍", "label": "x"}),
    )
    .await
    .assert_error(409, "glyph_taken");
    server.shutdown().await.unwrap();
}
