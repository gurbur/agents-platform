//! S14 wire (`IF-WIRE-013`, `IF-WIRE-001`): presence states, the request
//! value rules, the four-key presence object and the directory items whose
//! last two keys are `presence` and `last_seen_at`.

use platform_wire::{
    AgentEntry, AgentProfile, DeliveryState, DeliverySummary, MAX_PRESENCE_NOTE_CHARS,
    OwnerAgentEntry, PRESENCE_TTL_CEILING_SECS, Presence, PresenceDeclaration, PresenceFieldError,
    PresenceNoteError, PresenceRequest, PresenceResponse, PresenceState, WakePolicy,
    validate_presence_note,
};
use serde_json::{Value, json};

fn request(body: Value) -> Result<PresenceRequest, serde_json::Error> {
    serde_json::from_value(body)
}

fn validated(body: Value) -> Result<PresenceDeclaration, PresenceFieldError> {
    request(body).expect("the request parses").validate()
}

fn busy() -> Presence {
    Presence {
        state: PresenceState::Busy,
        note: Some("배포 점검 중".into()),
        updated_at: Some("2026-10-20T09:00:00.000Z".into()),
        expires_at: Some("2026-10-20T09:05:00.000Z".into()),
    }
}

#[test]
fn states_are_lowercase_and_only_two_are_declarable() {
    for (state, text) in [
        (PresenceState::Online, "online"),
        (PresenceState::Busy, "busy"),
        (PresenceState::Offline, "offline"),
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), json!(text));
        assert_eq!(text.parse::<PresenceState>(), Ok(state));
        assert_eq!(state.to_string(), text);
    }
    assert_eq!(PresenceState::declared("online"), Ok(PresenceState::Online));
    assert_eq!(PresenceState::declared("busy"), Ok(PresenceState::Busy));
    for raw in ["offline", "Online", "BUSY", "", " online", "away"] {
        assert!(PresenceState::declared(raw).is_err(), "{raw:?}");
    }
}

#[test]
fn the_contract_constants_bound_note_and_ttl() {
    assert_eq!(MAX_PRESENCE_NOTE_CHARS, 80);
    assert_eq!(PRESENCE_TTL_CEILING_SECS, 86_400);
}

#[test]
fn a_valid_request_keeps_the_state_trims_the_note_and_takes_the_ttl() {
    assert_eq!(
        validated(json!({"state": "busy", "note": "  배포 점검 중 ", "ttl_secs": 300})),
        Ok(PresenceDeclaration {
            state: PresenceState::Busy,
            note: Some("배포 점검 중".into()),
            ttl_secs: Some(300),
        })
    );
    let bare = PresenceDeclaration {
        state: PresenceState::Online,
        note: None,
        ttl_secs: None,
    };
    assert_eq!(validated(json!({"state": "online"})), Ok(bare.clone()));
    assert_eq!(
        validated(json!({"state": "online", "note": null, "ttl_secs": null})),
        Ok(bare),
        "null is the same as an omitted key"
    );
}

#[test]
fn the_state_value_is_checked_first() {
    for state in ["offline", "Online", "", "away"] {
        let err = validated(json!({"state": state, "note": "", "ttl_secs": 0})).unwrap_err();
        assert_eq!(err, PresenceFieldError::State, "{state:?}");
        assert_eq!(err.field(), "state");
    }
}

#[test]
fn the_note_follows_the_name_rules_with_its_own_limit() {
    let at_limit = "가".repeat(MAX_PRESENCE_NOTE_CHARS);
    assert_eq!(
        validate_presence_note(&format!(" {at_limit}\t")),
        Ok(at_limit)
    );
    let cases = [
        ("", PresenceNoteError::Empty),
        ("   ", PresenceNoteError::Empty),
        ("a\nb", PresenceNoteError::ControlCharacter),
        ("a\rb", PresenceNoteError::ControlCharacter),
        ("a\tb", PresenceNoteError::ControlCharacter),
        ("a\u{7}b", PresenceNoteError::ControlCharacter),
    ];
    for (note, expected) in cases {
        assert_eq!(validate_presence_note(note), Err(expected), "{note:?}");
        let err = validated(json!({"state": "busy", "note": note, "ttl_secs": 0})).unwrap_err();
        assert_eq!(err, PresenceFieldError::Note(expected));
        assert_eq!(err.field(), "note");
    }
    let too_long = "a".repeat(MAX_PRESENCE_NOTE_CHARS + 1);
    assert_eq!(
        validate_presence_note(&too_long),
        Err(PresenceNoteError::TooLong)
    );
    // A Cc-free format character is not filtered, like `name`.
    assert!(validate_presence_note("a\u{200b}b").is_ok());
}

#[test]
fn the_ttl_must_be_a_positive_integer() {
    for ttl in [0, -1, i64::MIN] {
        let err = validated(json!({"state": "online", "ttl_secs": ttl})).unwrap_err();
        assert_eq!(err, PresenceFieldError::TtlTooSmall, "{ttl}");
        assert_eq!(err.field(), "ttl_secs");
    }
    assert_eq!(
        validated(json!({"state": "online", "ttl_secs": 1}))
            .unwrap()
            .ttl_secs,
        Some(1)
    );
    for ttl in [json!(1.5), json!(1.0), json!("60"), json!(true)] {
        assert!(
            request(json!({"state": "online", "ttl_secs": ttl})).is_err(),
            "{ttl} is not a JSON integer"
        );
    }
}

#[test]
fn the_request_rejects_unknown_keys_and_wrong_types() {
    let unknown = request(json!({"state": "online", "agent_id": "agent-a"})).unwrap_err();
    assert!(
        unknown.to_string().contains("unknown field `agent_id`"),
        "{unknown}"
    );
    let missing = request(json!({"note": "x"})).unwrap_err();
    assert!(
        missing.to_string().contains("missing field `state`"),
        "{missing}"
    );
    for body in [
        json!({"state": 1}),
        json!({"state": null}),
        json!({"state": "busy", "note": 7}),
    ] {
        assert!(request(body.clone()).is_err(), "{body}");
    }
}

#[test]
fn a_request_serializes_only_the_given_keys() {
    let bare = PresenceRequest {
        state: "online".into(),
        note: None,
        ttl_secs: None,
    };
    assert_eq!(
        serde_json::to_value(&bare).unwrap(),
        json!({"state": "online"})
    );
    let full = PresenceRequest {
        state: "busy".into(),
        note: Some("x".into()),
        ttl_secs: Some(30),
    };
    assert_eq!(
        serde_json::to_value(&full).unwrap(),
        json!({"state": "busy", "note": "x", "ttl_secs": 30})
    );
}

#[test]
fn presence_always_has_four_keys_and_offline_is_all_null() {
    assert_eq!(
        serde_json::to_value(Presence::offline()).unwrap(),
        json!({"state": "offline", "note": null, "updated_at": null, "expires_at": null})
    );
    assert_eq!(Presence::default(), Presence::offline());
    let text = serde_json::to_string(&busy()).unwrap();
    assert_eq!(
        ordered_keys(&text),
        ["state", "note", "updated_at", "expires_at"]
    );
    let back: Presence = serde_json::from_str(&text).unwrap();
    assert_eq!(back, busy());
}

#[test]
fn the_put_response_has_three_keys_in_order() {
    let response = PresenceResponse {
        agent_id: "agent-a".parse().unwrap(),
        presence: busy(),
        last_seen_at: None,
    };
    let text = serde_json::to_string(&response).unwrap();
    assert_eq!(
        ordered_keys(&text),
        ["agent_id", "presence", "last_seen_at"]
    );
    assert_eq!(
        serde_json::to_value(&response).unwrap()["last_seen_at"],
        Value::Null
    );
}

#[test]
fn directory_items_end_with_presence_then_last_seen_at() {
    let agent = AgentEntry {
        agent_id: "agent-a".parse().unwrap(),
        name: Some("A".into()),
        joined_all: true,
        profile: AgentProfile::default(),
        avatar: None,
        presence: busy(),
        last_seen_at: Some("2026-10-20T09:00:00.000Z".into()),
    };
    assert_eq!(
        ordered_keys(&serde_json::to_string(&agent).unwrap()),
        [
            "agent_id",
            "name",
            "joined_all",
            "profile",
            "avatar",
            "presence",
            "last_seen_at"
        ]
    );
    let owner = OwnerAgentEntry {
        agent_id: "agent-a".parse().unwrap(),
        name: None,
        joined_all: true,
        profile: AgentProfile::default(),
        receiver_url: None,
        wake_policy: WakePolicy::Sender,
        route_version: 1,
        created_at: "2026-10-20T08:00:00.000Z".into(),
        delivery: DeliverySummary {
            state: DeliveryState::NoReceiver,
        },
        avatar: None,
        presence: Presence::offline(),
        last_seen_at: None,
    };
    let text = serde_json::to_string(&owner).unwrap();
    let keys = ordered_keys(&text);
    assert_eq!(
        keys[keys.len() - 3..],
        ["avatar", "presence", "last_seen_at"]
    );
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        value["last_seen_at"],
        Value::Null,
        "the key is never omitted"
    );
    assert_eq!(value["presence"]["state"], "offline");
}

#[test]
fn an_item_from_a_hub_before_s14_reads_as_offline_never_seen() {
    let old = json!({"agent_id": "agent-a", "name": "A", "joined_all": true,
                     "profile": {"gender_form": null, "age_band": null, "role": null, "intro": null, "updated_at": null},
                     "avatar": null});
    let entry: AgentEntry = serde_json::from_value(old).unwrap();
    assert_eq!(entry.presence, Presence::offline());
    assert_eq!(entry.last_seen_at, None);
}

/// The top-level keys of a JSON object text in document order.
fn ordered_keys(text: &str) -> Vec<String> {
    struct Keys(Vec<String>);
    impl<'de> serde::Deserialize<'de> for Keys {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Keys;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("a JSON object")
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> Result<Keys, A::Error> {
                    let mut keys = Vec::new();
                    while let Some(key) = map.next_key::<String>()? {
                        map.next_value::<serde::de::IgnoredAny>()?;
                        keys.push(key);
                    }
                    Ok(Keys(keys))
                }
            }
            deserializer.deserialize_map(Visitor)
        }
    }
    serde_json::from_str::<Keys>(text).expect("a JSON object").0
}
