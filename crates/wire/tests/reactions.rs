//! S16 wire shapes (`IF-WIRE-012`, `IF-WIRE-016`, `IF-WIRE-017`): the
//! slug newtype, glyph and label rules, the registry bodies, the reaction
//! request and responses, the change feed, the `reactions` summary on a
//! message and the `reaction` inbox event.

use platform_wire::reactions::{
    GlyphError, MAX_GLYPH_CHARS, MAX_REACTION_LABEL_CHARS, ReactionLabelError,
};
use platform_wire::{
    InboxEvent, InboxItem, Message, PutReactionKindRequest, ReactRequest, ReactResponse,
    ReactionChange, ReactionEvent, ReactionFeedPage, ReactionKind, ReactionKindEntry,
    ReactionKindFieldError, ReactionKindList, ReactionSummary, RoomId, RoomKind, Seq, codes,
    validate_glyph, validate_reaction_label,
};
use serde_json::{Value, json};

fn agent(id: &str) -> platform_wire::AgentId {
    id.parse().unwrap()
}

fn kind(slug: &str) -> ReactionKind {
    slug.parse().unwrap()
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

#[test]
fn a_reaction_kind_is_a_two_to_thirty_two_byte_slug() {
    for valid in ["ab", "ack", "thumbsup", "a_1", "z9", &"a".repeat(32)] {
        let parsed: ReactionKind = valid.parse().unwrap();
        assert_eq!(parsed.as_str(), valid);
        assert_eq!(serde_json::to_value(&parsed).unwrap(), json!(valid));
    }
    for invalid in [
        "",
        "a",
        "1a",
        "_a",
        "Ab",
        "aB",
        "a-b",
        "a b",
        "a:b",
        "👍",
        &"a".repeat(33),
    ] {
        assert!(invalid.parse::<ReactionKind>().is_err(), "{invalid:?}");
        assert!(
            serde_json::from_value::<ReactionKind>(json!(invalid)).is_err(),
            "{invalid:?}"
        );
    }
}

#[test]
fn a_glyph_is_one_to_four_scalars_without_space_or_control() {
    // The seeds: a variation selector counts as one scalar value.
    for seeded in ["👌", "👍", "❤️", "👀", "❓", "☑️", "🙋"] {
        assert_eq!(validate_glyph(seeded), Ok(()), "{seeded}");
    }
    assert_eq!("❤️".chars().count(), 2);
    assert_eq!(validate_glyph(&"🎉".repeat(MAX_GLYPH_CHARS)), Ok(()));
    assert_eq!(
        validate_glyph(&"🎉".repeat(MAX_GLYPH_CHARS + 1)),
        Err(GlyphError::TooLong)
    );
    assert_eq!(validate_glyph(""), Err(GlyphError::Empty));
    for spaced in [" 🎉", "🎉 ", "a b", "\u{3000}", "🎉\u{a0}"] {
        assert_eq!(
            validate_glyph(spaced),
            Err(GlyphError::Whitespace),
            "{spaced:?}"
        );
    }
    for control in ["\u{7}", "🎉\n", "\u{7f}"] {
        assert!(validate_glyph(control).is_err(), "{control:?}");
    }
}

#[test]
fn a_label_is_trimmed_and_bounded() {
    assert_eq!(validate_reaction_label("  축하 ").as_deref(), Ok("축하"));
    let max = "가".repeat(MAX_REACTION_LABEL_CHARS);
    assert_eq!(validate_reaction_label(&format!(" {max} ")), Ok(max));
    assert_eq!(
        validate_reaction_label(&"가".repeat(MAX_REACTION_LABEL_CHARS + 1)),
        Err(ReactionLabelError::TooLong)
    );
    assert_eq!(
        validate_reaction_label(" \t "),
        Err(ReactionLabelError::Empty)
    );
    assert_eq!(
        validate_reaction_label("a\nb"),
        Err(ReactionLabelError::ControlCharacter)
    );
}

#[test]
fn a_put_kind_request_validates_glyph_then_label_then_sort() {
    let request: PutReactionKindRequest =
        serde_json::from_value(json!({"glyph": "🎉", "label": " 축하 "})).unwrap();
    let update = request.validate().unwrap();
    assert_eq!(update.glyph, "🎉");
    assert_eq!(update.label, "축하");
    assert_eq!((update.sort, update.enabled), (None, None));
    let full: PutReactionKindRequest =
        serde_json::from_value(json!({"glyph": "🎉", "label": "x", "sort": 0, "enabled": false}))
            .unwrap();
    let update = full.validate().unwrap();
    assert_eq!((update.sort, update.enabled), (Some(0), Some(false)));
    for (body, field) in [
        (json!({"glyph": "", "label": "", "sort": -1}), "glyph"),
        (json!({"glyph": "🎉", "label": "", "sort": -1}), "label"),
        (json!({"glyph": "🎉", "label": "x", "sort": -1}), "sort"),
    ] {
        let request: PutReactionKindRequest = serde_json::from_value(body.clone()).unwrap();
        let err: ReactionKindFieldError = request.validate().unwrap_err();
        assert_eq!(err.field(), field, "{body}");
    }
    for invalid in [
        json!({"glyph": "🎉"}),
        json!({"label": "x"}),
        json!({"glyph": "🎉", "label": "x", "slug": "y"}),
        json!({"glyph": "🎉", "label": "x", "sort": 1.5}),
    ] {
        assert!(
            serde_json::from_value::<PutReactionKindRequest>(invalid.clone()).is_err(),
            "{invalid}"
        );
    }
    // Serializing leaves out what was not given.
    let request = PutReactionKindRequest {
        glyph: "🎉".into(),
        label: "축하".into(),
        sort: None,
        enabled: Some(false),
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        json!({"glyph": "🎉", "label": "축하", "enabled": false})
    );
}

#[test]
fn a_registry_entry_and_list_keep_their_key_order() {
    let entry = ReactionKindEntry {
        slug: kind("ack"),
        glyph: "👌".into(),
        label: "확인했다".into(),
        sort: 10,
        enabled: true,
    };
    let text = serde_json::to_string(&entry).unwrap();
    assert_eq!(
        ordered_keys(&text),
        ["slug", "glyph", "label", "sort", "enabled"]
    );
    let list = ReactionKindList {
        kinds: vec![entry.clone()],
    };
    let value = serde_json::to_value(&list).unwrap();
    assert_eq!(
        value,
        json!({"kinds": [{"slug": "ack", "glyph": "👌", "label": "확인했다", "sort": 10, "enabled": true}]})
    );
    assert_eq!(
        serde_json::from_value::<ReactionKindList>(value).unwrap(),
        list
    );
}

#[test]
fn a_react_request_omits_absent_keys_and_refuses_others() {
    assert_eq!(
        serde_json::to_value(ReactRequest::default()).unwrap(),
        json!({})
    );
    let owner = ReactRequest {
        wake: Some(true),
        sender_id: Some("human".into()),
    };
    assert_eq!(
        serde_json::to_value(&owner).unwrap(),
        json!({"wake": true, "sender_id": "human"})
    );
    assert!(serde_json::from_value::<ReactRequest>(json!({"emoji": "x"})).is_err());
    assert!(serde_json::from_value::<ReactRequest>(json!({"wake": "yes"})).is_err());
    let empty: ReactRequest = serde_json::from_value(json!({})).unwrap();
    assert_eq!(empty, ReactRequest::default());
}

fn summary() -> Vec<ReactionSummary> {
    vec![ReactionSummary {
        kind: kind("thumbsup"),
        count: 2,
        agents: vec![agent("agent-b"), agent("human")],
    }]
}

#[test]
fn a_react_response_and_a_summary_keep_their_key_order() {
    let response = ReactResponse {
        message_id: "msg-01J00000000000000000000001".parse().unwrap(),
        agent_id: agent("agent-b"),
        kind: kind("thumbsup"),
        change_seq: Seq::new(7).unwrap(),
        created_at: "2026-10-20T09:01:00.000Z".into(),
        reactions: summary(),
    };
    let text = serde_json::to_string(&response).unwrap();
    assert_eq!(
        ordered_keys(&text),
        [
            "message_id",
            "agent_id",
            "kind",
            "change_seq",
            "created_at",
            "reactions"
        ]
    );
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["change_seq"], "7");
    assert_eq!(
        value["reactions"],
        json!([{"kind": "thumbsup", "count": 2, "agents": ["agent-b", "human"]}])
    );
    let summary_text = serde_json::to_string(&summary()[0]).unwrap();
    assert_eq!(ordered_keys(&summary_text), ["kind", "count", "agents"]);
    assert_eq!(
        serde_json::from_value::<ReactResponse>(value).unwrap(),
        response
    );
}

#[test]
fn a_feed_page_carries_changes_and_a_cursor() {
    let page = ReactionFeedPage {
        items: vec![ReactionChange {
            change_seq: Seq::new(7).unwrap(),
            message_id: "msg-01J00000000000000000000001".parse().unwrap(),
            agent_id: agent("agent-b"),
            kind: kind("thumbsup"),
            removed: false,
            created_at: "2026-10-20T09:01:00.000Z".into(),
        }],
        next_after_change_seq: Seq::new(7).unwrap(),
        has_more: false,
    };
    let text = serde_json::to_string(&page).unwrap();
    assert_eq!(
        ordered_keys(&text),
        ["items", "next_after_change_seq", "has_more"]
    );
    let item = serde_json::to_string(&page.items[0]).unwrap();
    assert_eq!(
        ordered_keys(&item),
        [
            "change_seq",
            "message_id",
            "agent_id",
            "kind",
            "removed",
            "created_at"
        ]
    );
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["next_after_change_seq"], "7");
    assert_eq!(
        serde_json::from_value::<ReactionFeedPage>(value).unwrap(),
        page
    );
}

fn sample_message() -> Message {
    Message {
        message_id: "msg-01J00000000000000000000001".parse().unwrap(),
        event_seq: Seq::new(57).unwrap(),
        room_id: RoomId::All,
        room_kind: RoomKind::All,
        sender_id: agent("agent-a"),
        body: "hi".into(),
        reply_to: None,
        thread_id: None,
        corrects: None,
        wake: true,
        mentions: vec![],
        thread_summary: None,
        reactions: summary(),
        created_at: "2026-10-20T09:00:00.000Z".into(),
    }
}

#[test]
fn a_message_has_fourteen_keys_with_reactions_before_created_at() {
    let message = sample_message();
    let text = serde_json::to_string(&message).unwrap();
    let keys = ordered_keys(&text);
    assert_eq!(keys.len(), 14);
    assert_eq!(keys[11..], ["thread_summary", "reactions", "created_at"]);
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        serde_json::from_value::<Message>(value.clone()).unwrap(),
        message
    );
    // A message from a hub before S16 reads with no reactions.
    let mut old = value;
    old.as_object_mut().unwrap().remove("reactions");
    let read: Message = serde_json::from_value(old).unwrap();
    assert!(read.reactions.is_empty());
}

fn sample_event() -> ReactionEvent {
    ReactionEvent {
        change_seq: Seq::new(7).unwrap(),
        message_id: "msg-01J00000000000000000000001".parse().unwrap(),
        room_id: RoomId::All,
        room_kind: RoomKind::All,
        thread_id: None,
        sender_id: agent("agent-b"),
        reaction: kind("thumbsup"),
        wake: false,
        created_at: "2026-10-20T09:01:00.000Z".into(),
    }
}

#[test]
fn a_reaction_event_has_twelve_keys_in_order_and_round_trips() {
    let item = InboxItem::reaction(Seq::new(43).unwrap(), false, sample_event());
    let text = serde_json::to_string(&item).unwrap();
    assert_eq!(
        ordered_keys(&text),
        [
            "inbox_seq",
            "kind",
            "woke",
            "change_seq",
            "message_id",
            "room_id",
            "room_kind",
            "thread_id",
            "sender_id",
            "reaction",
            "wake",
            "created_at"
        ]
    );
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        value,
        json!({"inbox_seq": "43", "kind": "reaction", "woke": false, "change_seq": "7",
               "message_id": "msg-01J00000000000000000000001", "room_id": "all",
               "room_kind": "all", "thread_id": null, "sender_id": "agent-b",
               "reaction": "thumbsup", "wake": false, "created_at": "2026-10-20T09:01:00.000Z"})
    );
    let back: InboxItem = serde_json::from_value(value).unwrap();
    assert_eq!(back, item);
    assert_eq!(back.kind(), "reaction");
    assert_eq!(back.woke(), Some(false));
    assert_eq!(back.as_message(), None);
    assert_eq!(back.as_reaction(), Some(&sample_event()));
    assert!(matches!(back.event, InboxEvent::Reaction { .. }));
    // A reaction event missing a key is malformed; an extra key is not.
    let mut missing = serde_json::to_value(&item).unwrap();
    missing.as_object_mut().unwrap().remove("change_seq");
    assert!(serde_json::from_value::<InboxItem>(missing).is_err());
    let mut extended = serde_json::to_value(&item).unwrap();
    extended["later"] = json!(1);
    assert!(serde_json::from_value::<InboxItem>(extended).is_ok());
}

#[test]
fn the_two_new_error_codes_are_named() {
    assert_eq!(codes::UNKNOWN_REACTION_KIND, "unknown_reaction_kind");
    assert_eq!(codes::GLYPH_TAKEN, "glyph_taken");
}
