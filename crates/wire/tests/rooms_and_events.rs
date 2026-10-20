//! S13 wire (`IF-ID-001`, `IF-WIRE-010`, `IF-WIRE-011`, `IF-WIRE-012`):
//! the three room id forms, the one send body, message and inbox event
//! shapes, the room summary, `wake_policy`, and the unchanged wake body.

use platform_wire::{
    CreateRoomRequest, InboxItem, MAX_ROOM_TITLE_CHARS, Message, RoomId, RoomIdError, RoomKind,
    RoomSlug, RoomSummary, SendMessageRequest, Seq, UpdateReceiverRequest, WakeNotification,
    WakePolicy, codes, validate_room_title,
};
use serde_json::{Value, json};

fn agent(id: &str) -> platform_wire::AgentId {
    id.parse().unwrap()
}

#[test]
fn room_ids_parse_and_print_in_three_forms() {
    assert_eq!("all".parse::<RoomId>(), Ok(RoomId::All));
    let dm: RoomId = "dm:agent-a:agent-b".parse().unwrap();
    assert_eq!(dm, RoomId::Dm(agent("agent-a"), agent("agent-b")));
    let topic: RoomId = "room:launch_1".parse().unwrap();
    assert_eq!(
        topic,
        RoomId::Topic("launch_1".parse::<RoomSlug>().unwrap())
    );
    for (room, text, kind) in [
        (RoomId::All, "all", RoomKind::All),
        (dm.clone(), "dm:agent-a:agent-b", RoomKind::Dm),
        (topic.clone(), "room:launch_1", RoomKind::Topic),
    ] {
        assert_eq!(room.to_string(), text);
        assert_eq!(room.kind(), kind);
        assert_eq!(serde_json::to_value(&room).unwrap(), json!(text));
    }
    assert_eq!(
        dm.dm_members(),
        Some((&agent("agent-a"), &agent("agent-b")))
    );
    assert_eq!(topic.dm_members(), None);
    assert_eq!(RoomId::All.dm_members(), None);
    // A digit may start a slug, unlike an agent_id.
    assert!("room:0".parse::<RoomId>().is_ok());
    assert!(format!("room:{}", "a".repeat(64)).parse::<RoomId>().is_ok());
}

#[test]
fn room_ids_reject_everything_else() {
    for invalid in [
        "room:".to_owned(),
        "room:A".to_owned(),
        "room:-x".to_owned(),
        "room:_x".to_owned(),
        "room:a:b".to_owned(),
        "room:a b".to_owned(),
        format!("room:{}", "a".repeat(65)),
        "Room:a".to_owned(),
        "launch".to_owned(),
        "".to_owned(),
    ] {
        assert!(invalid.parse::<RoomId>().is_err(), "{invalid:?}");
    }
    assert_eq!(
        "dm:agent-b:agent-a".parse::<RoomId>(),
        Err(RoomIdError::NotCanonical)
    );
    assert!(serde_json::from_value::<RoomId>(json!("room:A")).is_err());
}

#[test]
fn room_kind_has_three_snake_case_values() {
    for (kind, text) in [
        (RoomKind::All, "all"),
        (RoomKind::Dm, "dm"),
        (RoomKind::Topic, "topic"),
    ] {
        assert_eq!(serde_json::to_value(kind).unwrap(), json!(text));
        assert_eq!(kind.as_str(), text);
    }
}

#[test]
fn the_send_body_is_strict_and_keeps_its_destinations_as_text() {
    let full: SendMessageRequest = serde_json::from_value(json!({
        "room_id": "all", "body": "hi", "client_key": "k", "wake": false,
        "mentions": ["agent-b"], "sender_id": "human"
    }))
    .unwrap();
    assert_eq!(full.room_id.as_deref(), Some("all"));
    assert_eq!(full.wake, Some(false));
    assert_eq!(full.mentions, Some(vec!["agent-b".to_owned()]));
    assert_eq!(full.sender_id.as_deref(), Some("human"));
    assert_eq!((full.to, full.reply_to), (None, None));

    let minimal: SendMessageRequest =
        serde_json::from_value(json!({"to": "all", "body": "hi", "client_key": "k"})).unwrap();
    assert_eq!(
        minimal.to.as_deref(),
        Some("all"),
        "format is checked later"
    );
    assert_eq!((minimal.wake, minimal.mentions), (None, None));

    let nulls: SendMessageRequest = serde_json::from_value(json!({
        "room_id": null, "reply_to": null, "to": "agent-b", "body": "", "client_key": "k",
        "wake": null, "mentions": null, "sender_id": null
    }))
    .unwrap();
    assert_eq!(nulls.room_id, None);
    assert_eq!(
        (nulls.wake, nulls.mentions, nulls.sender_id),
        (None, None, None),
        "null = omitted"
    );

    for (body, needle) in [
        (
            json!({"room_id": "all", "body": "x", "client_key": "k", "thread_id": "t"}),
            "unknown field `thread_id`",
        ),
        (
            json!({"room_id": "all", "body": "x", "client_key": "k", "wake": "no"}),
            "invalid type",
        ),
        (
            json!({"room_id": "all", "client_key": "k"}),
            "missing field `body`",
        ),
    ] {
        let err = serde_json::from_value::<SendMessageRequest>(body).unwrap_err();
        assert!(err.to_string().contains(needle), "{err}");
    }
}

fn sample_message() -> Message {
    Message {
        message_id: "msg-01J00000000000000000000000".parse().unwrap(),
        event_seq: Seq::new(57).unwrap(),
        room_id: "dm:agent-a:agent-b".parse().unwrap(),
        room_kind: RoomKind::Dm,
        sender_id: agent("agent-a"),
        body: "…".into(),
        reply_to: None,
        wake: false,
        mentions: vec![agent("human")],
        created_at: "2026-10-20T09:00:00.000Z".into(),
    }
}

/// The top-level keys of a JSON object text in document order (`Value`
/// sorts them: serde_json is built without `preserve_order`).
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

#[test]
fn a_send_body_omits_every_absent_option() {
    let request = SendMessageRequest {
        reply_to: Some("msg-01J00000000000000000000000".into()),
        body: "x".into(),
        client_key: "k".into(),
        ..SendMessageRequest::default()
    };
    let text = serde_json::to_string(&request).unwrap();
    assert_eq!(ordered_keys(&text), ["reply_to", "body", "client_key"]);
    assert!(!text.contains("null"), "{text}");
}

#[test]
fn a_message_has_ten_keys_in_order_and_keeps_nulls_and_empty_mentions() {
    let mut message = sample_message();
    message.mentions.clear();
    let value = serde_json::to_value(&message).unwrap();
    assert_eq!(
        ordered_keys(&serde_json::to_string(&message).unwrap()),
        [
            "message_id",
            "event_seq",
            "room_id",
            "room_kind",
            "sender_id",
            "body",
            "reply_to",
            "wake",
            "mentions",
            "created_at"
        ]
    );
    assert_eq!(value["reply_to"], Value::Null);
    assert_eq!(value["mentions"], json!([]));
    assert_eq!(serde_json::from_value::<Message>(value).unwrap(), message);
}

#[test]
fn a_message_event_is_flat_and_round_trips() {
    let item = InboxItem::message(Seq::new(42).unwrap(), true, sample_message());
    let value = serde_json::to_value(&item).unwrap();
    assert_eq!(
        value,
        json!({"inbox_seq": "42", "kind": "message", "woke": true, "message_id": "msg-01J00000000000000000000000",
               "event_seq": "57", "room_id": "dm:agent-a:agent-b", "room_kind": "dm",
               "sender_id": "agent-a", "body": "…", "reply_to": null, "wake": false,
               "mentions": ["human"], "created_at": "2026-10-20T09:00:00.000Z"})
    );
    let order = ordered_keys(&serde_json::to_string(&item).unwrap());
    assert_eq!(order[..4], ["inbox_seq", "kind", "woke", "message_id"]);
    assert_eq!(order.len(), 13);
    let back: InboxItem = serde_json::from_value(value).unwrap();
    assert_eq!(back, item);
    assert_eq!(back.kind(), "message");
    assert_eq!(back.woke(), Some(true));
    // `woke` is per recipient: a history message never carries it.
    let history = serde_json::to_value(sample_message()).unwrap();
    assert!(history.get("woke").is_none());
    // A message event without `woke` is malformed.
    let mut missing = serde_json::to_value(&item).unwrap();
    missing.as_object_mut().unwrap().remove("woke");
    assert!(serde_json::from_value::<InboxItem>(missing).is_err());
    assert_eq!(back.as_message(), Some(&sample_message()));
}

#[test]
fn an_unknown_event_kind_is_accepted_not_an_error() {
    let item: InboxItem = serde_json::from_value(json!({
        "inbox_seq": "43", "kind": "reaction", "message_id": "msg-01J00000000000000000000000",
        "emoji": "👍"
    }))
    .unwrap();
    assert_eq!(item.inbox_seq, Seq::new(43).unwrap());
    assert_eq!(item.kind(), "reaction");
    assert_eq!(item.as_message(), None);
    // A known kind with an extra field still reads.
    let mut extended = serde_json::to_value(InboxItem::message(
        Seq::new(1).unwrap(),
        false,
        sample_message(),
    ))
    .unwrap();
    extended["thread_id"] = json!("t-1");
    assert!(serde_json::from_value::<InboxItem>(extended).is_ok());
    // Without its sequence or kind an item is still malformed.
    assert!(serde_json::from_value::<InboxItem>(json!({"kind": "message"})).is_err());
    assert!(serde_json::from_value::<InboxItem>(json!({"inbox_seq": "1"})).is_err());
}

#[test]
fn a_room_summary_has_eleven_keys_and_keeps_nulls() {
    let summary = RoomSummary {
        room_id: "room:launch".parse().unwrap(),
        kind: RoomKind::Topic,
        title: None,
        members: vec![],
        created_by: "owner".into(),
        created_at: "2026-10-20T09:00:00.000Z".into(),
        archived_at: None,
        last_event_seq: None,
        last_message_at: None,
        last_sender_id: None,
        message_count: 0,
    };
    let value = serde_json::to_value(&summary).unwrap();
    assert_eq!(
        value,
        json!({"room_id": "room:launch", "kind": "topic", "title": null, "members": [],
               "created_by": "owner", "created_at": "2026-10-20T09:00:00.000Z",
               "archived_at": null, "last_event_seq": null, "last_message_at": null,
               "last_sender_id": null, "message_count": 0})
    );
    assert_eq!(
        serde_json::from_value::<RoomSummary>(value).unwrap(),
        summary
    );
}

#[test]
fn room_creation_bodies_and_titles() {
    let request: CreateRoomRequest =
        serde_json::from_value(json!({"room_id": "room:a", "title": null})).unwrap();
    assert_eq!((request.room_id.as_str(), request.title), ("room:a", None));
    assert!(
        serde_json::from_value::<CreateRoomRequest>(json!({"room_id": "room:a", "x": 1})).is_err()
    );

    assert_eq!(validate_room_title("  출시  ").unwrap(), "출시");
    assert!(validate_room_title(&"가".repeat(MAX_ROOM_TITLE_CHARS)).is_ok());
    for invalid in [
        String::new(),
        "  ".to_owned(),
        "a\nb".to_owned(),
        "가".repeat(MAX_ROOM_TITLE_CHARS + 1),
    ] {
        assert!(validate_room_title(&invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn wake_policy_and_the_receiver_patch() {
    assert_eq!(
        serde_json::to_value(WakePolicy::Sender).unwrap(),
        json!("sender")
    );
    assert_eq!(
        serde_json::to_value(WakePolicy::Always).unwrap(),
        json!("always")
    );
    assert_eq!("always".parse::<WakePolicy>(), Ok(WakePolicy::Always));
    assert!("Always".parse::<WakePolicy>().is_err());

    let policy_only: UpdateReceiverRequest =
        serde_json::from_value(json!({"wake_policy": "always"})).unwrap();
    assert!(policy_only.receiver_url.is_keep());
    assert_eq!(policy_only.wake_policy.as_deref(), Some("always"));
    let clear: UpdateReceiverRequest =
        serde_json::from_value(json!({"receiver_url": null})).unwrap();
    assert!(!clear.receiver_url.is_keep());
    assert_eq!(clear.wake_policy, None);
    let empty: UpdateReceiverRequest = serde_json::from_value(json!({})).unwrap();
    assert!(empty.receiver_url.is_keep() && empty.wake_policy.is_none());
    let null_policy: UpdateReceiverRequest =
        serde_json::from_value(json!({"wake_policy": null})).unwrap();
    assert_eq!(null_policy.wake_policy, None, "null = omitted");
    assert!(serde_json::from_value::<UpdateReceiverRequest>(json!({"x": 1})).is_err());
}

#[test]
fn the_wake_body_is_unchanged() {
    let body = WakeNotification::new(agent("agent-a"), Seq::new(42).unwrap());
    assert_eq!(
        serde_json::to_value(&body).unwrap(),
        json!({"v": 1, "agent_id": "agent-a", "inbox_seq": "42"})
    );
}

#[test]
fn s13_error_codes() {
    assert_eq!(codes::NOT_ROOM_MEMBER, "not_room_member");
    assert_eq!(codes::ROOM_ARCHIVED, "room_archived");
    assert_eq!(codes::ROOM_ID_TAKEN, "room_id_taken");
    assert_eq!(codes::INVALID_DESTINATION, "invalid_destination");
    assert_eq!(codes::MENTION_NOT_MEMBER, "mention_not_member");
}
