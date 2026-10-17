//! S9: directory profile over HTTP — registration with `name`, the
//! `PUT /v1/agents/{agent_id}/profile` partial update and its `IF-WIRE-006`
//! check order, and the `name`/`profile` fields of the directory
//! (PLATFORM_MESSAGING_SPEC 수용 기준 13).

mod common;

use std::collections::BTreeSet;

use common::{Api, Fixture, OWNER_TOKEN, Reply, RunningServer, default_name};
use platform_wire::{MAX_INTRO_CHARS, MAX_NAME_CHARS, MAX_ROLE_CHARS};
use platformd::api::MAX_WIRE_BODY_BYTES;
use platformd::clock;
use reqwest::Method;
use serde_json::{Value, json};

const PROFILE_KEYS: [&str; 5] = ["gender_form", "age_band", "role", "intro", "updated_at"];

fn profile_path(agent_id: &str) -> String {
    format!("/v1/agents/{agent_id}/profile")
}

fn keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect()
}

fn chars(c: char, count: usize) -> String {
    std::iter::repeat_n(c, count).collect()
}

async fn start() -> (Fixture, RunningServer, Api) {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;
    let api = server.api();
    (fx, server, api)
}

async fn put_profile(api: &Api, agent_id: &str, token: &str, body: Value) -> Reply {
    api.put(&profile_path(agent_id), token, body).await
}

/// The directory item of `agent_id` as `token` sees it.
async fn directory_item(api: &Api, agent_id: &str, token: &str) -> Value {
    let reply = api.get("/v1/agents", token).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["agent_id"] == agent_id)
        .unwrap_or_else(|| panic!("{agent_id} not listed: {}", reply.body))
        .clone()
}

fn assert_ok(reply: &Reply) {
    assert_eq!(reply.status, 200, "{}", reply.body);
}

fn assert_invalid_field(reply: &Reply, field: &str) {
    reply.assert_error(422, "invalid_field");
    assert_eq!(reply.error_field(), Some(field), "{}", reply.body);
}

fn updated_at(body: &Value) -> String {
    body["profile"]["updated_at"]
        .as_str()
        .expect("updated_at")
        .to_owned()
}

/// Waits until the server clock is past `timestamp`, so a later write can
/// only get a later `updated_at` (millisecond precision).
async fn wait_past(timestamp: &str) {
    common::eventually(
        std::time::Duration::from_secs(5),
        "the clock passes the last write",
        || async { (clock::now_rfc3339_ms().as_str() > timestamp).then_some(()) },
    )
    .await;
}

// ---- registration -----------------------------------------------------------------

#[tokio::test]
async fn registration_returns_the_name_and_an_empty_profile_dated_at_creation() {
    let (_fx, server, api) = start().await;

    let agent = api.register_named("agent-a", "  에이전트 A ", None).await;

    assert_eq!(agent.body["name"], "에이전트 A", "the name is trimmed");
    let profile = &agent.body["profile"];
    assert_eq!(keys(profile), PROFILE_KEYS.into_iter().collect());
    for field in ["gender_form", "age_band", "role", "intro"] {
        assert_eq!(profile[field], Value::Null, "{field}");
    }
    let owner_item = directory_item(&api, "agent-a", OWNER_TOKEN).await;
    assert_eq!(profile["updated_at"], owner_item["created_at"]);
    assert_eq!(owner_item["name"], "에이전트 A");
    assert_eq!(owner_item["profile"], *profile);

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn registration_name_is_required_and_validated() {
    let (_fx, server, api) = start().await;
    let register = |body: Value| api.post("/v1/agents", OWNER_TOKEN, body);

    assert_invalid_field(&register(json!({"agent_id": "agent-a"})).await, "name");
    for name in [
        json!(null),
        json!(5),
        json!(""),
        json!("   "),
        json!(chars('가', MAX_NAME_CHARS + 1)),
        json!("two\nlines"),
        json!("tab\there"),
    ] {
        let reply = register(json!({"agent_id": "agent-a", "name": name})).await;
        assert_invalid_field(&reply, "name");
    }

    let profile_field = register(json!({"agent_id": "agent-a", "name": "A", "role": "ops"})).await;
    profile_field.assert_error(422, "unexpected_field");
    assert_eq!(profile_field.error_field(), Some("role"));

    // invalid_agent_id comes before the name value, and the name value
    // before invalid_receiver_url.
    let both = register(json!({"agent_id": "Agent-A", "name": ""})).await;
    both.assert_error(422, "invalid_agent_id");
    let name_and_url =
        register(json!({"agent_id": "agent-a", "name": "", "receiver_url": "ftp://x/"})).await;
    assert_invalid_field(&name_and_url, "name");

    let longest = chars('🦀', MAX_NAME_CHARS);
    let ok = register(json!({"agent_id": "agent-a", "name": longest})).await;
    assert_eq!(ok.status, 201, "{}", ok.body);
    assert_eq!(ok.body["name"], longest.as_str());

    server.shutdown().await.unwrap();
}

// ---- directory shape -------------------------------------------------------------

#[tokio::test]
async fn directory_shows_declared_profiles_to_agents_and_the_owner() {
    let (_fx, server, api) = start().await;
    let a = api.register_named("agent-a", "에이전트 A", None).await;
    let b = api.register("agent-b", None).await;

    let declared = put_profile(&api, "agent-a",
            &a.api_key,
            json!({"gender_form": "none", "role": "배포와 운영 점검", "intro": "로그를 봅니다.\n급하면 DM."}),
        )
        .await;
    assert_ok(&declared);

    let expected_profile = json!({
        "gender_form": "none",
        "age_band": null,
        "role": "배포와 운영 점검",
        "intro": "로그를 봅니다.\n급하면 DM.",
        "updated_at": declared.body["profile"]["updated_at"],
    });
    for token in [b.api_key.as_str(), OWNER_TOKEN] {
        let item = directory_item(&api, "agent-a", token).await;
        assert_eq!(item["name"], "에이전트 A");
        assert_eq!(item["profile"], expected_profile);
        let other = directory_item(&api, "agent-b", token).await;
        assert_eq!(other["name"], default_name("agent-b").as_str());
        assert_eq!(other["profile"]["role"], Value::Null);
    }
    let agent_view = directory_item(&api, "agent-a", &b.api_key).await;
    for owner_only in ["receiver_url", "route_version", "created_at", "delivery"] {
        assert!(agent_view.get(owner_only).is_none(), "{owner_only}");
    }

    server.shutdown().await.unwrap();
}

// ---- PUT /v1/agents/{agent_id}/profile ----------------------------------------------

#[tokio::test]
async fn profile_update_requires_a_credential() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;

    let none = api
        .send(
            Method::PUT,
            &profile_path("agent-a"),
            None,
            Some(json!({"role": "x"})),
        )
        .await;
    none.assert_error(401, "unauthorized");
    put_profile(&api, "agent-a", &a.receiver_secret, json!({"role": "x"}))
        .await
        .assert_error(401, "unauthorized");

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_agent_cannot_change_another_agents_profile() {
    let (_fx, server, api) = start().await;
    api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;

    for body in [json!({"role": "x"}), json!({"bogus": 1}), json!({})] {
        put_profile(&api, "agent-a", &b.api_key, body)
            .await
            .assert_error(403, "forbidden");
    }
    put_profile(&api, "agent-zzz", &b.api_key, json!({"role": "x"}))
        .await
        .assert_error(403, "forbidden");
    let a = directory_item(&api, "agent-a", OWNER_TOKEN).await;
    assert_eq!(a["profile"]["role"], Value::Null);

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_agent_sending_name_is_forbidden_whatever_the_values() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;

    for body in [
        json!({"name": "새 이름"}),
        json!({"name": null}),
        json!({"name": ""}),
        json!({"name": "새 이름", "gender_form": "Female"}),
        json!({"role": chars('a', MAX_ROLE_CHARS + 1), "name": "x"}),
    ] {
        put_profile(&api, "agent-a", &a.api_key, body)
            .await
            .assert_error(403, "forbidden");
    }
    let item = directory_item(&api, "agent-a", OWNER_TOKEN).await;
    assert_eq!(item["name"], default_name("agent-a").as_str());

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_owner_sets_and_changes_the_name_but_cannot_clear_it() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;

    let renamed = put_profile(
        &api,
        "agent-a",
        OWNER_TOKEN,
        json!({"name": " 에이전트 A "}),
    )
    .await;
    assert_ok(&renamed);
    assert_eq!(
        keys(&renamed.body),
        ["agent_id", "name", "profile"].into_iter().collect()
    );
    assert_eq!(renamed.body["agent_id"], "agent-a");
    assert_eq!(renamed.body["name"], "에이전트 A");

    let changed = put_profile(&api, "agent-a", OWNER_TOKEN, json!({"name": "A2"})).await;
    assert_ok(&changed);
    assert_eq!(changed.body["name"], "A2");
    let seen_by_agent = directory_item(&api, "agent-a", &a.api_key).await;
    assert_eq!(seen_by_agent["name"], "A2");

    assert_invalid_field(
        &put_profile(&api, "agent-a", OWNER_TOKEN, json!({"name": null})).await,
        "name",
    );
    let owner_profile = put_profile(&api, "agent-a", OWNER_TOKEN, json!({"age_band": "20s"})).await;
    assert_ok(&owner_profile);
    assert_eq!(owner_profile.body["profile"]["age_band"], "20s");

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn body_shape_errors_are_unexpected_or_invalid_field() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let put = |body: Value| put_profile(&api, "agent-a", &a.api_key, body);

    let unexpected = put(json!({"role": "x", "pronouns": "they"})).await;
    unexpected.assert_error(422, "unexpected_field");
    assert_eq!(unexpected.error_field(), Some("pronouns"));
    let server_field = put(json!({"updated_at": "2026-10-17T00:00:00.000Z"})).await;
    server_field.assert_error(422, "unexpected_field");
    assert_eq!(server_field.error_field(), Some("updated_at"));

    for (field, value) in [
        ("gender_form", json!(1)),
        ("age_band", json!(["teen"])),
        ("role", json!(true)),
        ("intro", json!({"text": "x"})),
    ] {
        assert_invalid_field(&put(json!({ field: value })).await, field);
    }
    put(json!([])).await.assert_error(422, "invalid_field");

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn value_rules_are_invalid_field_naming_the_first_violation() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let put = |body: Value| put_profile(&api, "agent-a", &a.api_key, body);

    for (field, value) in [
        ("gender_form", json!("Female")),
        ("gender_form", json!(" none")),
        ("age_band", json!("30s-40s")),
        ("age_band", json!("")),
        ("role", json!(chars('a', MAX_ROLE_CHARS + 1))),
        ("role", json!("a\nb")),
        ("role", json!("  ")),
        ("intro", json!(chars('a', MAX_INTRO_CHARS + 1))),
        ("intro", json!("a\r\nb")),
    ] {
        assert_invalid_field(&put(json!({ field: value })).await, field);
    }
    let several = put(json!({
        "intro": "a\tb",
        "role": "",
        "age_band": "old",
        "gender_form": "nope"
    }))
    .await;
    assert_invalid_field(&several, "gender_form");
    let later = put(json!({"intro": "a\tb", "role": "", "gender_form": "male"})).await;
    assert_invalid_field(&later, "role");

    let item = directory_item(&api, "agent-a", OWNER_TOKEN).await;
    assert_eq!(
        item["profile"]["gender_form"],
        Value::Null,
        "nothing stored"
    );

    let bounds = put(json!({
        "role": chars('역', MAX_ROLE_CHARS),
        "intro": format!("{}\n", chars('소', MAX_INTRO_CHARS - 1)),
    }))
    .await;
    assert_ok(&bounds);

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn omitted_fields_stay_and_null_clears() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let put = |body: Value| put_profile(&api, "agent-a", &a.api_key, body);

    let declared = put(json!({
        "gender_form": "female",
        "age_band": "teen",
        "role": "  리뷰어  ",
        "intro": "안녕하세요"
    }))
    .await;
    assert_ok(&declared);
    assert_eq!(declared.body["profile"]["role"], "리뷰어", "trimmed");

    let partial = put(json!({"age_band": null, "intro": "다시 인사"})).await;
    assert_ok(&partial);
    let profile = &partial.body["profile"];
    assert_eq!(profile["gender_form"], "female");
    assert_eq!(profile["age_band"], Value::Null);
    assert_eq!(profile["role"], "리뷰어");
    assert_eq!(profile["intro"], "다시 인사");

    let item = directory_item(&api, "agent-a", &a.api_key).await;
    assert_eq!(item["profile"], *profile);

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn updated_at_changes_only_when_a_stored_value_changes() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let put = |body: Value| put_profile(&api, "agent-a", &a.api_key, body);
    let registered_at = updated_at(&a.body);

    wait_past(&registered_at).await;
    let first = put(json!({"role": "ops", "gender_form": "none"})).await;
    assert_ok(&first);
    let first_at = updated_at(&first.body);
    assert!(first_at > registered_at, "{first_at} > {registered_at}");

    wait_past(&first_at).await;
    for body in [
        json!({}),
        json!({"role": "ops"}),
        json!({"role": " ops ", "gender_form": "none", "intro": null, "age_band": null}),
    ] {
        let same = put(body.clone()).await;
        assert_ok(&same);
        assert_eq!(updated_at(&same.body), first_at, "{body}");
    }
    let renamed_same = put_profile(
        &api,
        "agent-a",
        OWNER_TOKEN,
        json!({"name": default_name("agent-a")}),
    )
    .await;
    assert_eq!(updated_at(&renamed_same.body), first_at, "same name");

    let changed = put(json!({"role": null})).await;
    assert_ok(&changed);
    let changed_at = updated_at(&changed.body);
    assert!(changed_at > first_at, "{changed_at} > {first_at}");

    wait_past(&changed_at).await;
    let renamed = put_profile(&api, "agent-a", OWNER_TOKEN, json!({"name": "새 이름"})).await;
    assert!(
        updated_at(&renamed.body) > changed_at,
        "a name change counts"
    );

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn owner_gets_404_for_non_slug_and_unregistered_paths() {
    let (_fx, server, api) = start().await;

    for agent_id in ["Not-A-Slug", "owner", "agent-zzz"] {
        put_profile(&api, agent_id, OWNER_TOKEN, json!({"role": "x"}))
            .await
            .assert_error(404, "agent_not_found");
    }

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn body_format_errors_are_415_400_and_413() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let path = profile_path("agent-a");
    let raw = |content_type: Option<&'static str>, body: Vec<u8>| {
        api.send_raw(
            Method::PUT,
            &path,
            Some(&a.api_key),
            content_type,
            Some(body),
        )
    };

    raw(Some("text/plain"), br#"{"role":"x"}"#.to_vec())
        .await
        .assert_error(415, "unsupported_media_type");
    raw(Some("application/json"), b"{\"role\":".to_vec())
        .await
        .assert_error(400, "invalid_json");
    let oversized = format!(r#"{{"role":"{}"}}"#, "a".repeat(MAX_WIRE_BODY_BYTES));
    raw(Some("application/json"), oversized.into_bytes())
        .await
        .assert_error(413, "request_too_large");

    server.shutdown().await.unwrap();
}

async fn put_raw(
    api: &Api,
    path: String,
    token: Option<&str>,
    content_type: &str,
    body: &str,
) -> Reply {
    api.send_raw(
        Method::PUT,
        &path,
        token,
        Some(content_type),
        Some(body.as_bytes().to_vec()),
    )
    .await
}

/// One request per adjacent pair of `IF-WIRE-006` steps, each failing both
/// checks, so the earlier step must win.
#[tokio::test]
async fn checks_run_in_the_documented_order() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let json_type = "application/json";
    let owner = Some(OWNER_TOKEN);
    let key_a = Some(a.api_key.as_str());
    let key_b = Some(b.api_key.as_str());

    // 1 before 2: no credential on another agent's path.
    put_raw(&api, profile_path("agent-a"), None, "text/plain", "{")
        .await
        .assert_error(401, "unauthorized");
    // 2 before 4: another agent's path with a broken body.
    put_raw(&api, profile_path("agent-a"), key_b, "text/plain", "{")
        .await
        .assert_error(403, "forbidden");
    // 3 before 4: the owner on a non-slug path with a broken body.
    put_raw(&api, profile_path("Bad-Slug"), owner, "text/plain", "{")
        .await
        .assert_error(404, "agent_not_found");
    // 4 before 9: an unregistered slug is judged after the body format.
    put_raw(&api, profile_path("agent-zzz"), owner, "text/plain", "{}")
        .await
        .assert_error(415, "unsupported_media_type");
    // 5 before 7: an unknown key wins over the agent's name.
    let unexpected = put_raw(
        &api,
        profile_path("agent-a"),
        key_a,
        json_type,
        r#"{"name":"x","pronouns":"they"}"#,
    )
    .await;
    unexpected.assert_error(422, "unexpected_field");
    // 6 before 7: a type error wins over the agent's name.
    let mistyped = put_raw(
        &api,
        profile_path("agent-a"),
        key_a,
        json_type,
        r#"{"name":"x","role":7}"#,
    )
    .await;
    assert_invalid_field(&mistyped, "role");
    // 7 before 8: the agent's name wins over a bad enum.
    put_raw(
        &api,
        profile_path("agent-a"),
        key_a,
        json_type,
        r#"{"gender_form":"Female","name":"x"}"#,
    )
    .await
    .assert_error(403, "forbidden");
    // 8 before 9: a value rule on an unregistered slug.
    let value_first = put_raw(
        &api,
        profile_path("agent-zzz"),
        owner,
        json_type,
        r#"{"gender_form":"Female"}"#,
    )
    .await;
    assert_invalid_field(&value_first, "gender_form");
    // 9: a valid body for an unregistered slug.
    put_raw(
        &api,
        profile_path("agent-zzz"),
        owner,
        json_type,
        r#"{"name":"Z"}"#,
    )
    .await
    .assert_error(404, "agent_not_found");

    server.shutdown().await.unwrap();
}

/// Scenario 13 end to end, as the acceptance criterion reads.
#[tokio::test]
async fn scenario_13_directory_profile() {
    let (_fx, server, api) = start().await;
    let a = api.register_named("agent-a", "에이전트 A", None).await;
    let b = api.register_named("agent-b", "에이전트 B", None).await;

    let declared = put_profile(
        &api,
        "agent-a",
        &a.api_key,
        json!({"gender_form": "none", "role": "배포와 운영 점검"}),
    )
    .await;
    assert_ok(&declared);
    let directory = api.get("/v1/agents", &b.api_key).await;
    let text = directory.body.to_string();
    assert!(!text.contains("pak_") && !text.contains("prs_"), "{text}");
    let item = directory_item(&api, "agent-a", &b.api_key).await;
    assert_eq!(item["name"], "에이전트 A");
    assert_eq!(item["profile"]["role"], "배포와 운영 점검");

    put_profile(&api, "agent-a", &a.api_key, json!({"name": "사칭"}))
        .await
        .assert_error(403, "forbidden");
    put_profile(&api, "agent-a", &b.api_key, json!({"role": "x"}))
        .await
        .assert_error(403, "forbidden");
    assert_ok(
        &put_profile(
            &api,
            "agent-a",
            OWNER_TOKEN,
            json!({"name": "에이전트 에이"}),
        )
        .await,
    );
    assert_invalid_field(
        &put_profile(&api, "agent-a", &a.api_key, json!({"age_band": "40s"})).await,
        "age_band",
    );
    assert_invalid_field(
        &put_profile(
            &api,
            "agent-a",
            &a.api_key,
            json!({"intro": chars('a', MAX_INTRO_CHARS + 1)}),
        )
        .await,
        "intro",
    );
    assert_invalid_field(
        &put_profile(&api, "agent-a", OWNER_TOKEN, json!({"name": null})).await,
        "name",
    );
    let cleared = put_profile(&api, "agent-a", &a.api_key, json!({"role": null})).await;
    assert_eq!(cleared.body["profile"]["role"], Value::Null);
    let again = put_profile(&api, "agent-a", &a.api_key, json!({"role": null})).await;
    assert_eq!(updated_at(&again.body), updated_at(&cleared.body));

    let item = directory_item(&api, "agent-a", &b.api_key).await;
    assert_eq!(item["name"], "에이전트 에이");
    assert_eq!(item["profile"]["gender_form"], "none");
    assert_eq!(item["profile"]["role"], Value::Null);

    server.shutdown().await.unwrap();
}
