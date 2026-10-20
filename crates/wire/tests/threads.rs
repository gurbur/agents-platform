//! S15 wire shapes (`IF-WIRE-012`, `IF-WIRE-014`, `IF-WIRE-015`): the
//! thirteen `Message` keys, the summary of a root, the thread view, the
//! participation body and response, and the send body's `corrects`.

use platform_wire::{
    InboxItem, JoinedVia, Message, ParticipationRequest, ParticipationResponse, RoomKind,
    SendMessageRequest, Seq, ThreadParticipant, ThreadSummary, ThreadView, codes,
};
use serde_json::{Value, json};

fn agent(id: &str) -> platform_wire::AgentId {
    id.parse().unwrap()
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

fn sample() -> Message {
    Message {
        message_id: "msg-01J00000000000000000000001".parse().unwrap(),
        event_seq: Seq::new(57).unwrap(),
        room_id: "dm:agent-a:agent-b".parse().unwrap(),
        room_kind: RoomKind::Dm,
        sender_id: agent("agent-a"),
        body: "…".into(),
        reply_to: Some("msg-01J00000000000000000000002".parse().unwrap()),
        thread_id: Some("msg-01J00000000000000000000003".parse().unwrap()),
        corrects: None,
        wake: false,
        mentions: vec![agent("human")],
        thread_summary: None,
        created_at: "2026-10-20T09:00:00.000Z".into(),
    }
}

const MESSAGE_KEYS: [&str; 13] = [
    "message_id",
    "event_seq",
    "room_id",
    "room_kind",
    "sender_id",
    "body",
    "reply_to",
    "thread_id",
    "corrects",
    "wake",
    "mentions",
    "thread_summary",
    "created_at",
];

#[test]
fn a_message_has_thirteen_keys_in_order_with_nulls_kept() {
    let message = sample();
    let text = serde_json::to_string(&message).unwrap();
    assert_eq!(ordered_keys(&text), MESSAGE_KEYS);
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["corrects"], Value::Null);
    assert_eq!(value["thread_summary"], Value::Null);
    assert_eq!(serde_json::from_value::<Message>(value).unwrap(), message);
}

#[test]
fn a_root_carries_its_summary() {
    let root = Message {
        reply_to: None,
        thread_id: None,
        thread_summary: Some(ThreadSummary {
            reply_count: 3,
            last_event_seq: Seq::new(61).unwrap(),
        }),
        ..sample()
    };
    let value = serde_json::to_value(&root).unwrap();
    assert_eq!(
        value["thread_summary"],
        json!({"reply_count": 3, "last_event_seq": "61"})
    );
    let text = serde_json::to_string(&root.thread_summary).unwrap();
    assert_eq!(ordered_keys(&text), ["reply_count", "last_event_seq"]);
    assert_eq!(serde_json::from_value::<Message>(value).unwrap(), root);
}

#[test]
fn a_message_event_has_sixteen_keys_with_woke_after_kind() {
    let item = InboxItem::message(Seq::new(42).unwrap(), true, sample());
    let text = serde_json::to_string(&item).unwrap();
    let mut expected = vec!["inbox_seq", "kind", "woke"];
    expected.extend(MESSAGE_KEYS);
    assert_eq!(ordered_keys(&text), expected);
    let back: InboxItem = serde_json::from_str(&text).unwrap();
    assert_eq!(back, item);
}

#[test]
fn the_send_body_carries_corrects_only_when_given() {
    let without = SendMessageRequest {
        room_id: Some("all".into()),
        body: "x".into(),
        client_key: "k".into(),
        ..SendMessageRequest::default()
    };
    assert!(
        serde_json::to_value(&without)
            .unwrap()
            .get("corrects")
            .is_none()
    );
    let with = SendMessageRequest {
        corrects: Some("msg-01J00000000000000000000001".into()),
        ..without
    };
    assert_eq!(
        serde_json::to_value(&with).unwrap()["corrects"],
        "msg-01J00000000000000000000001"
    );
    let parsed: SendMessageRequest = serde_json::from_value(
        json!({"room_id": "all", "body": "x", "client_key": "k", "corrects": null}),
    )
    .unwrap();
    assert_eq!(parsed.corrects, None);
    assert!(
        serde_json::from_value::<SendMessageRequest>(
            json!({"room_id": "all", "body": "x", "client_key": "k", "thread_id": "x"})
        )
        .is_err(),
        "no thread_id in the send body"
    );
}

#[test]
fn joined_via_has_five_names() {
    for (via, text) in [
        (JoinedVia::Root, "root"),
        (JoinedVia::Opener, "opener"),
        (JoinedVia::Poster, "poster"),
        (JoinedVia::Mentioned, "mentioned"),
        (JoinedVia::Barged, "barged"),
    ] {
        assert_eq!(via.as_str(), text);
        assert_eq!(serde_json::to_value(via).unwrap(), text);
        assert_eq!(JoinedVia::parse(text), Some(via));
    }
    assert_eq!(JoinedVia::parse("Root"), None);
}

fn participant(id: &str, via: JoinedVia, muted: bool) -> ThreadParticipant {
    ThreadParticipant {
        agent_id: agent(id),
        joined_via: via,
        muted,
        joined_at: "2026-10-20T09:00:00.000Z".into(),
    }
}

#[test]
fn the_thread_view_matches_the_api_shape() {
    let view = ThreadView {
        thread_id: "msg-01J00000000000000000000003".parse().unwrap(),
        room_id: "dm:agent-a:agent-b".parse().unwrap(),
        room_kind: RoomKind::Dm,
        root_sender_id: agent("human"),
        participants: vec![
            participant("agent-a", JoinedVia::Opener, true),
            participant("human", JoinedVia::Root, false),
        ],
        reply_count: 3,
        last_event_seq: Seq::new(61).unwrap(),
        last_message_at: "2026-10-20T09:20:00.000Z".into(),
    };
    let text = serde_json::to_string(&view).unwrap();
    assert_eq!(
        ordered_keys(&text),
        [
            "thread_id",
            "room_id",
            "room_kind",
            "root_sender_id",
            "participants",
            "reply_count",
            "last_event_seq",
            "last_message_at"
        ]
    );
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        value["participants"][0],
        json!({"agent_id": "agent-a", "joined_via": "opener", "muted": true,
               "joined_at": "2026-10-20T09:00:00.000Z"})
    );
    let first = serde_json::to_string(&view.participants[0]).unwrap();
    assert_eq!(
        ordered_keys(&first),
        ["agent_id", "joined_via", "muted", "joined_at"]
    );
    assert_eq!(serde_json::from_value::<ThreadView>(value).unwrap(), view);
}

#[test]
fn participation_takes_wake_and_an_optional_agent_id() {
    let own: ParticipationRequest = serde_json::from_value(json!({"wake": false})).unwrap();
    assert_eq!((own.wake, own.agent_id), (false, None));
    let owner: ParticipationRequest =
        serde_json::from_value(json!({"wake": true, "agent_id": "human"})).unwrap();
    assert_eq!(owner.agent_id.as_deref(), Some("human"));
    for invalid in [
        json!({}),
        json!({"wake": "no"}),
        json!({"wake": null}),
        json!({"wake": true, "extra": 1}),
    ] {
        assert!(
            serde_json::from_value::<ParticipationRequest>(invalid.clone()).is_err(),
            "{invalid}"
        );
    }
    let response = ParticipationResponse {
        thread_id: "msg-01J00000000000000000000003".parse().unwrap(),
        participant: participant("human", JoinedVia::Barged, true),
    };
    let text = serde_json::to_string(&response).unwrap();
    assert_eq!(
        ordered_keys(&text),
        ["thread_id", "agent_id", "joined_via", "muted", "joined_at"]
    );
    assert_eq!(
        serde_json::from_str::<ParticipationResponse>(&text).unwrap(),
        response
    );
}

#[test]
fn the_new_codes_are_named() {
    assert_eq!(codes::THREAD_NOT_FOUND, "thread_not_found");
    assert_eq!(codes::CORRECTS_NOT_FOUND, "corrects_not_found");
    assert_eq!(codes::CORRECTS_NOT_OWN, "corrects_not_own");
    assert_eq!(codes::CORRECTS_ROOM_MISMATCH, "corrects_room_mismatch");
}
