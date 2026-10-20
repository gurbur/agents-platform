//! Human-readable rendering. Every function returns the full text so the
//! commands only decide *what* to print; `--json` bypasses this module and
//! prints the server's raw body.

use std::fmt::Display;
use std::io::Write;

use platform_wire::{
    AgentEntry, AgentId, AgentProfile, AvatarResponse, DeliveryRetryResponse, DeliveryState,
    DeliveryStatus, HealthStatus, HistoryCursor, HistoryPage, IdentityTokenResponse, InboxItem,
    InboxPage, MembershipResponse, Message, MessageResponse, OwnerAgentEntry,
    ParticipationResponse, Presence, PresenceResponse, ProfileResponse, ReactResponse,
    ReactionEvent, ReactionKindEntry, ReactionSummary, ReadMarkerResponse, ReceiverResponse,
    RegisterAgentResponse, RoomSummary, RotateTokensResponse, Seq, ThreadParticipant, ThreadView,
};

use crate::error::CliError;

/// Shown for `null` values.
const NONE: &str = "-";
/// Separation between table columns and between a key and its value.
const GAP: &str = "  ";

/// Writes `text` to stdout.
pub fn emit(text: &str) -> Result<(), CliError> {
    write_stdout(text.as_bytes())
}

/// Writes a raw response body followed by a newline.
pub fn emit_raw(body: &[u8]) -> Result<(), CliError> {
    write_stdout(body)?;
    write_stdout(b"\n")
}

/// One line on stderr, prefixed like every other CLI diagnostic.
pub fn warn(line: &str) {
    eprintln!("platform: {line}");
}

fn write_stdout(bytes: &[u8]) -> Result<(), CliError> {
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(bytes)
        .and_then(|()| stdout.flush())
        .map_err(|err| CliError::io("cannot write to stdout", err))
}

// ---- building blocks -------------------------------------------------------------

/// Makes a message body fit on one line: `\` and control characters are
/// escaped, so `\n` in the output always means an escaped newline.
pub fn escape_body(body: &str) -> String {
    let mut escaped = String::with_capacity(body.len());
    for c in body.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c if c.is_control() => escaped.push_str(&format!("\\u{{{:x}}}", u32::from(c))),
            c => escaped.push(c),
        }
    }
    escaped
}

fn or_none<T: Display>(value: Option<T>) -> String {
    value.map_or_else(|| NONE.to_owned(), |value| value.to_string())
}

/// Agent ids joined by `,`, or `-` for none.
fn id_list(ids: &[AgentId]) -> String {
    if ids.is_empty() {
        return NONE.to_owned();
    }
    let ids: Vec<&str> = ids.iter().map(AgentId::as_str).collect();
    ids.join(",")
}

/// Reactions in one cell: `kind×count(agent,agent)` per kind joined by a
/// space, or `-` for none (`IF-CLI-001` S16).
fn reaction_cell(reactions: &[ReactionSummary]) -> String {
    if reactions.is_empty() {
        return NONE.to_owned();
    }
    let kinds: Vec<String> = reactions
        .iter()
        .map(|summary| {
            format!(
                "{}×{}({})",
                summary.kind,
                summary.count,
                id_list(&summary.agents)
            )
        })
        .collect();
    kinds.join(" ")
}

/// The agents with an active reaction of `kind`, joined by `,`, or `-`.
fn reactors(reactions: &[ReactionSummary], kind: &str) -> String {
    reactions
        .iter()
        .find(|summary| summary.kind.as_str() == kind)
        .map_or_else(|| NONE.to_owned(), |summary| id_list(&summary.agents))
}

/// The sender's `wake` in a table cell.
fn wake_cell(wake: bool) -> &'static str {
    if wake { "wake" } else { "quiet" }
}

fn state_name(state: DeliveryState) -> &'static str {
    match state {
        DeliveryState::Idle => "idle",
        DeliveryState::NoReceiver => "no_receiver",
        DeliveryState::Pending => "pending",
        DeliveryState::Blocked => "blocked",
    }
}

/// Rows of cells under a header; every column but the last is padded to its
/// widest cell so the last one (usually a body) can run free.
struct Table {
    header: Vec<&'static str>,
    rows: Vec<Vec<String>>,
}

impl Table {
    fn new(header: Vec<&'static str>) -> Self {
        Self {
            header,
            rows: Vec::new(),
        }
    }

    fn push(&mut self, row: Vec<String>) {
        self.rows.push(row);
    }

    fn render(&self) -> String {
        let header: Vec<String> = self.header.iter().map(|cell| (*cell).to_owned()).collect();
        let lines: Vec<&Vec<String>> = std::iter::once(&header).chain(&self.rows).collect();
        let columns = self.header.len();
        let widths: Vec<usize> = (0..columns)
            .map(|column| {
                lines
                    .iter()
                    .map(|line| line[column].chars().count())
                    .max()
                    .unwrap_or_default()
            })
            .collect();
        let mut out = String::new();
        for line in lines {
            for (column, cell) in line.iter().enumerate() {
                if column + 1 == columns {
                    out.push_str(cell);
                    break;
                }
                let padding = widths[column] - cell.chars().count();
                out.push_str(cell);
                out.push_str(&" ".repeat(padding));
                out.push_str(GAP);
            }
            out.push('\n');
        }
        out
    }
}

/// `key  value` lines with the values aligned.
fn key_values(pairs: &[(&str, String)]) -> String {
    let width = pairs.iter().map(|(key, _)| key.len()).max().unwrap_or(0);
    pairs
        .iter()
        .map(|(key, value)| format!("{key:<width$}{GAP}{value}\n"))
        .collect()
}

/// `key value  key value ...` on one line, for pagination state.
fn summary(pairs: &[(&str, String)]) -> String {
    let fields: Vec<String> = pairs
        .iter()
        .map(|(key, value)| format!("{key} {value}"))
        .collect();
    format!("{}\n", fields.join(GAP))
}

// ---- per-response renderers ------------------------------------------------------

pub fn health(status: &HealthStatus) -> String {
    format!(
        "{} (schema_version {})\n",
        status.status, status.schema_version
    )
}

/// The token alone on one line, for `token=$(…)` capture.
pub fn identity_token(issued: &IdentityTokenResponse) -> String {
    format!("{}\n", issued.token)
}

pub fn registration(agent: &RegisterAgentResponse) -> String {
    key_values(&[
        ("agent_id", agent.agent_id.to_string()),
        ("name", one_line(agent.name.as_deref())),
        ("joined_all", agent.joined_all.to_string()),
        ("receiver_url", or_none(agent.receiver_url.as_deref())),
        ("route_version", agent.route_version.to_string()),
        ("agent_api_key", agent.agent_api_key.clone()),
        ("receiver_secret", agent.receiver_secret.clone()),
    ])
}

pub fn rotation(rotated: &RotateTokensResponse) -> String {
    key_values(&[
        ("agent_id", rotated.agent_id.to_string()),
        ("route_version", rotated.route_version.to_string()),
        ("agent_api_key", rotated.agent_api_key.clone()),
        ("receiver_secret", rotated.receiver_secret.clone()),
        (
            "previous_keys_revoked",
            rotated.previous_keys_revoked.to_string(),
        ),
    ])
}

/// A free-text profile value on one table or key-value line. Validation
/// keeps control characters out of `name` and `role`; escaping is cheap
/// insurance for the layout.
fn one_line(value: Option<&str>) -> String {
    value.map_or_else(|| NONE.to_owned(), escape_body)
}

pub fn owner_directory(agents: &[OwnerAgentEntry]) -> String {
    let mut table = Table::new(vec![
        "AGENT_ID",
        "NAME",
        "JOINED_ALL",
        "PRESENCE",
        "LAST_SEEN",
        "DELIVERY",
        "ROUTE_VERSION",
        "CREATED_AT",
        "RECEIVER_URL",
        "ROLE",
    ]);
    for agent in agents {
        table.push(vec![
            agent.agent_id.to_string(),
            one_line(agent.name.as_deref()),
            agent.joined_all.to_string(),
            agent.presence.state.to_string(),
            or_none(agent.last_seen_at.as_deref()),
            state_name(agent.delivery.state).to_owned(),
            agent.route_version.to_string(),
            agent.created_at.clone(),
            or_none(agent.receiver_url.as_deref()),
            one_line(agent.profile.role.as_deref()),
        ]);
    }
    table.render()
}

pub fn agent_directory(agents: &[AgentEntry]) -> String {
    let mut table = Table::new(vec![
        "AGENT_ID",
        "NAME",
        "JOINED_ALL",
        "PRESENCE",
        "LAST_SEEN",
        "ROLE",
    ]);
    for agent in agents {
        table.push(vec![
            agent.agent_id.to_string(),
            one_line(agent.name.as_deref()),
            agent.joined_all.to_string(),
            agent.presence.state.to_string(),
            or_none(agent.last_seen_at.as_deref()),
            one_line(agent.profile.role.as_deref()),
        ]);
    }
    table.render()
}

/// `profile set|set-name`: key-value lines, then `intro` as written (it
/// may span lines), or `intro  -` when undeclared.
pub fn profile(entry: &ProfileResponse) -> String {
    profile_with(entry, Vec::new())
}

/// `profile show`: [`profile`] with the directory's presence lines after
/// `updated_at` (S14).
pub fn directory_profile(
    entry: &ProfileResponse,
    presence: &Presence,
    last_seen_at: Option<&str>,
) -> String {
    profile_with(
        entry,
        vec![
            ("presence", presence.state.to_string()),
            ("presence_note", one_line(presence.note.as_deref())),
            ("last_seen_at", or_none(last_seen_at)),
        ],
    )
}

fn profile_with(entry: &ProfileResponse, extra: Vec<(&'static str, String)>) -> String {
    let AgentProfile {
        gender_form,
        age_band,
        role,
        intro,
        updated_at,
    } = &entry.profile;
    let mut pairs = vec![
        ("agent_id", entry.agent_id.to_string()),
        ("name", one_line(entry.name.as_deref())),
        ("gender_form", or_none(*gender_form)),
        ("age_band", or_none(*age_band)),
        ("role", one_line(role.as_deref())),
        ("updated_at", or_none(updated_at.as_deref())),
    ];
    pairs.extend(extra);
    let Some(intro) = intro else {
        pairs.push(("intro", NONE.to_owned()));
        return key_values(&pairs);
    };
    let mut text = key_values(&pairs);
    text.push_str("intro\n");
    text.push_str(intro);
    text.push('\n');
    text
}

/// `presence set`: the declaration as the hub stored it.
pub fn presence(declared: &PresenceResponse) -> String {
    let presence = &declared.presence;
    key_values(&[
        ("agent_id", declared.agent_id.to_string()),
        ("state", presence.state.to_string()),
        ("note", one_line(presence.note.as_deref())),
        ("updated_at", or_none(presence.updated_at.as_deref())),
        ("expires_at", or_none(presence.expires_at.as_deref())),
        ("last_seen_at", or_none(declared.last_seen_at.as_deref())),
    ])
}

pub fn avatar(stored: &AvatarResponse) -> String {
    key_values(&[
        ("agent_id", stored.agent_id.to_string()),
        ("content_type", stored.avatar.content_type.to_string()),
        ("bytes", stored.avatar.bytes.to_string()),
        ("sha256", stored.avatar.sha256.clone()),
        ("updated_at", stored.avatar.updated_at.clone()),
    ])
}

/// `profile avatar get`: what was saved and where.
pub fn avatar_saved(saved: &SavedAvatar) -> String {
    key_values(&[
        ("agent_id", saved.agent_id.clone()),
        ("content_type", saved.content_type.clone()),
        ("bytes", saved.bytes.to_string()),
        ("sha256", saved.sha256.clone()),
        ("out", saved.out.clone()),
    ])
}

/// The result of `profile avatar get`, also printed as JSON under `--json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SavedAvatar {
    pub agent_id: String,
    pub content_type: String,
    pub bytes: usize,
    pub sha256: String,
    pub out: String,
}

pub fn receiver(route: &ReceiverResponse) -> String {
    key_values(&[
        ("agent_id", route.agent_id.to_string()),
        ("receiver_url", or_none(route.receiver_url.as_deref())),
        ("route_version", route.route_version.to_string()),
        ("wake_policy", route.wake_policy.to_string()),
        ("state", state_name(route.state).to_owned()),
    ])
}

pub fn membership(membership: &MembershipResponse) -> String {
    key_values(&[
        ("agent_id", membership.agent_id.to_string()),
        ("joined_all", membership.joined_all.to_string()),
        ("membership_epoch", membership.membership_epoch.to_string()),
        (
            "effective_event_seq",
            membership.effective_event_seq.to_string(),
        ),
    ])
}

/// `room list`: one row per room in the server's order.
pub fn rooms(rooms: &[RoomSummary]) -> String {
    let mut table = Table::new(vec![
        "ROOM",
        "KIND",
        "TITLE",
        "MEMBERS",
        "LAST_AT",
        "LAST_SENDER",
        "COUNT",
        "ARCHIVED_AT",
    ]);
    for room in rooms {
        table.push(vec![
            room.room_id.to_string(),
            room.kind.as_str().to_owned(),
            one_line(room.title.as_deref()),
            id_list(&room.members),
            or_none(room.last_message_at.as_deref()),
            or_none(room.last_sender_id.as_ref()),
            room.message_count.to_string(),
            or_none(room.archived_at.as_deref()),
        ]);
    }
    table.render()
}

/// `room create|invite|archive`: one room as key-value lines.
pub fn room(room: &RoomSummary) -> String {
    key_values(&[
        ("room_id", room.room_id.to_string()),
        ("kind", room.kind.as_str().to_owned()),
        ("title", one_line(room.title.as_deref())),
        ("members", id_list(&room.members)),
        ("created_by", room.created_by.clone()),
        ("created_at", room.created_at.clone()),
        ("archived_at", or_none(room.archived_at.as_deref())),
    ])
}

/// The commit result plus the `client_key` to reuse for a retry.
pub fn sent(response: &MessageResponse, client_key: &str) -> String {
    let message = &response.message;
    let mut text = key_values(&[
        ("message_id", message.message_id.to_string()),
        ("event_seq", message.event_seq.to_string()),
        ("room_id", message.room_id.to_string()),
        ("room_kind", message.room_kind.as_str().to_owned()),
        ("sender_id", message.sender_id.to_string()),
        ("reply_to", or_none(message.reply_to.as_ref())),
        ("thread_id", or_none(message.thread_id.as_ref())),
        ("corrects", or_none(message.corrects.as_ref())),
        ("wake", message.wake.to_string()),
        ("mentions", id_list(&message.mentions)),
        ("created_at", message.created_at.clone()),
        ("client_key", client_key.to_owned()),
    ]);
    if response.deduplicated {
        text.push_str("(deduplicated — same client_key)\n");
    }
    text
}

/// One row per inbox event. `WAKE` is the sender's choice, `WOKE` whether
/// the sender's rule woke this recipient. A reaction event names the
/// reacted-to message and who reacted, with `+<kind>` in place of a body
/// (`IF-CLI-001` S16). An event of a kind this CLI does not know shows only
/// its sequence and kind.
fn inbox_table(items: &[InboxItem]) -> String {
    let mut table = Table::new(vec![
        "INBOX_SEQ",
        "KIND",
        "ROOM_ID",
        "EVENT_SEQ",
        "MESSAGE_ID",
        "CREATED_AT",
        "SENDER_ID",
        "WAKE",
        "WOKE",
        "REPLY_TO",
        "MENTIONS",
        "THREAD_ID",
        "CORRECTS",
        "BODY",
    ]);
    for item in items {
        let mut row = vec![item.inbox_seq.to_string(), item.kind().to_owned()];
        if let Some(reaction) = item.as_reaction() {
            row.extend(reaction_row(reaction));
            table.push(row);
            continue;
        }
        match item.as_message() {
            Some(message) => row.extend([
                message.room_id.to_string(),
                message.event_seq.to_string(),
                message.message_id.to_string(),
                message.created_at.clone(),
                message.sender_id.to_string(),
                wake_cell(message.wake).to_owned(),
                or_none(item.woke()),
                or_none(message.reply_to.as_ref()),
                id_list(&message.mentions),
                or_none(message.thread_id.as_ref()),
                or_none(message.corrects.as_ref()),
                escape_body(&message.body),
            ]),
            None => row.extend(std::iter::repeat_n(NONE.to_owned(), 12)),
        }
        table.push(row);
    }
    table.render()
}

/// The cells after `INBOX_SEQ` and `KIND` of a reaction event: the room,
/// the message reacted to, when and by whom, the reacting agent's `wake`,
/// the message's thread and `+<kind>` as the body; `-` elsewhere.
fn reaction_row(reaction: &ReactionEvent) -> [String; 12] {
    [
        reaction.room_id.to_string(),
        NONE.to_owned(),
        reaction.message_id.to_string(),
        reaction.created_at.clone(),
        reaction.sender_id.to_string(),
        wake_cell(reaction.wake).to_owned(),
        NONE.to_owned(),
        NONE.to_owned(),
        NONE.to_owned(),
        or_none(reaction.thread_id.as_ref()),
        NONE.to_owned(),
        format!("+{}", reaction.reaction),
    ]
}

pub fn inbox_page(page: &InboxPage) -> String {
    let mut text = inbox_table(&page.items);
    text.push_str(&summary(&[
        ("agent_id", page.agent_id.to_string()),
        ("read_marker", page.read_marker.to_string()),
        ("snapshot_head_seq", page.snapshot_head_seq.to_string()),
        ("next_after_seq", page.next_after_seq.to_string()),
        ("has_more", page.has_more.to_string()),
    ]));
    text
}

/// A whole snapshot collected by `inbox --all-pages`.
pub fn inbox_snapshot(
    agent_id: &str,
    read_marker: Seq,
    snapshot_head_seq: Seq,
    items: &[InboxItem],
) -> String {
    let mut text = inbox_table(items);
    text.push_str(&summary(&[
        ("agent_id", agent_id.to_owned()),
        ("read_marker", read_marker.to_string()),
        ("snapshot_head_seq", snapshot_head_seq.to_string()),
        ("items", items.len().to_string()),
    ]));
    text
}

pub fn read_marker(marker: &ReadMarkerResponse) -> String {
    key_values(&[
        ("agent_id", marker.agent_id.to_string()),
        ("read_marker", marker.read_marker.to_string()),
    ])
}

pub fn history_page(page: &HistoryPage) -> String {
    let mut table = Table::new(vec![
        "EVENT_SEQ",
        "MESSAGE_ID",
        "CREATED_AT",
        "SENDER_ID",
        "WAKE",
        "REPLY_TO",
        "MENTIONS",
        "THREAD_ID",
        "CORRECTS",
        "REPLIES",
        "REACTIONS",
        "BODY",
    ]);
    for message in &page.items {
        table.push(vec![
            message.event_seq.to_string(),
            message.message_id.to_string(),
            message.created_at.clone(),
            message.sender_id.to_string(),
            wake_cell(message.wake).to_owned(),
            or_none(message.reply_to.as_ref()),
            id_list(&message.mentions),
            or_none(message.thread_id.as_ref()),
            or_none(message.corrects.as_ref()),
            or_none(message.thread_summary.map(|summary| summary.reply_count)),
            reaction_cell(&message.reactions),
            escape_body(&message.body),
        ]);
    }
    let mut text = table.render();
    let cursor = match page.next {
        HistoryCursor::Before {
            next_before_event_seq,
        } => ("next_before_event_seq", or_none(next_before_event_seq)),
        HistoryCursor::After {
            next_after_event_seq,
        } => ("next_after_event_seq", or_none(next_after_event_seq)),
    };
    text.push_str(&summary(&[cursor, ("has_more", page.has_more.to_string())]));
    text
}

pub fn message(message: &Message) -> String {
    let summary = message.thread_summary;
    key_values(&[
        ("message_id", message.message_id.to_string()),
        ("event_seq", message.event_seq.to_string()),
        ("room_id", message.room_id.to_string()),
        ("room_kind", message.room_kind.as_str().to_owned()),
        ("sender_id", message.sender_id.to_string()),
        ("reply_to", or_none(message.reply_to.as_ref())),
        ("thread_id", or_none(message.thread_id.as_ref())),
        ("corrects", or_none(message.corrects.as_ref())),
        (
            "reply_count",
            or_none(summary.map(|summary| summary.reply_count)),
        ),
        (
            "last_reply_seq",
            or_none(summary.map(|summary| summary.last_event_seq)),
        ),
        ("wake", message.wake.to_string()),
        ("mentions", id_list(&message.mentions)),
        ("reactions", reaction_cell(&message.reactions)),
        ("created_at", message.created_at.clone()),
        ("body", escape_body(&message.body)),
    ])
}

/// The reaction kinds `thread show` reads from the root: who took the
/// work on and who marked it done (advisory, ADR-0014).
const CLAIM_KIND: &str = "claim";
const DONE_KIND: &str = "done";

/// `thread show`: the thread as key-value lines with who claimed the root
/// and who marked it done (`IF-CLI-001` S16), then its participants.
pub fn thread(view: &ThreadView, root: &Message) -> String {
    let mut text = key_values(&[
        ("thread_id", view.thread_id.to_string()),
        ("room_id", view.room_id.to_string()),
        ("root_sender_id", view.root_sender_id.to_string()),
        ("reply_count", view.reply_count.to_string()),
        ("last_event_seq", view.last_event_seq.to_string()),
        ("last_message_at", view.last_message_at.clone()),
        ("claimed_by", reactors(&root.reactions, CLAIM_KIND)),
        ("done_by", reactors(&root.reactions, DONE_KIND)),
    ]);
    let mut table = Table::new(vec!["AGENT_ID", "JOINED_VIA", "MUTED", "JOINED_AT"]);
    for participant in &view.participants {
        table.push(vec![
            participant.agent_id.to_string(),
            participant.joined_via.as_str().to_owned(),
            participant.muted.to_string(),
            participant.joined_at.clone(),
        ]);
    }
    text.push_str(&table.render());
    text
}

/// `thread mute|unmute`: the participant after the change.
pub fn participation(changed: &ParticipationResponse) -> String {
    let ThreadParticipant {
        agent_id,
        joined_via,
        muted,
        joined_at,
    } = &changed.participant;
    key_values(&[
        ("thread_id", changed.thread_id.to_string()),
        ("agent_id", agent_id.to_string()),
        ("joined_via", joined_via.as_str().to_owned()),
        ("muted", muted.to_string()),
        ("joined_at", joined_at.clone()),
    ])
}

/// `react`: the reaction, whether it is new (`created`) or was there
/// (`existing`), and the message's reactions now.
pub fn reacted(reacted: &ReactResponse, status: &str) -> String {
    key_values(&[
        ("message_id", reacted.message_id.to_string()),
        ("agent_id", reacted.agent_id.to_string()),
        ("kind", reacted.kind.to_string()),
        ("change_seq", reacted.change_seq.to_string()),
        ("created_at", reacted.created_at.clone()),
        ("status", status.to_owned()),
        ("reactions", reaction_cell(&reacted.reactions)),
    ])
}

/// `react --list`: one row per kind in the hub's order.
pub fn reaction_kinds(kinds: &[ReactionKindEntry]) -> String {
    let mut table = Table::new(vec!["SLUG", "GLYPH", "LABEL", "SORT", "ENABLED"]);
    for kind in kinds {
        table.push(vec![
            kind.slug.to_string(),
            kind.glyph.clone(),
            escape_body(&kind.label),
            kind.sort.to_string(),
            kind.enabled.to_string(),
        ]);
    }
    table.render()
}

/// `reaction-kind set`: the kind as stored and whether it is new.
pub fn reaction_kind(kind: &ReactionKindEntry, status: &str) -> String {
    key_values(&[
        ("slug", kind.slug.to_string()),
        ("glyph", kind.glyph.clone()),
        ("label", escape_body(&kind.label)),
        ("sort", kind.sort.to_string()),
        ("enabled", kind.enabled.to_string()),
        ("status", status.to_owned()),
    ])
}

pub fn delivery_status(status: &DeliveryStatus) -> String {
    key_values(&[
        ("state", state_name(status.state).to_owned()),
        ("read_marker", status.read_marker.to_string()),
        ("inbox_head_seq", status.inbox_head_seq.to_string()),
        ("pending_seq", or_none(status.pending_seq)),
        ("last_accepted_seq", or_none(status.last_accepted_seq)),
        ("route_version", status.route_version.to_string()),
        (
            "receiver_url_present",
            status.receiver_url_present.to_string(),
        ),
        (
            "last_attempt_at",
            or_none(status.last_attempt_at.as_deref()),
        ),
        ("last_error", or_none(status.last_error.as_deref())),
    ])
}

pub fn delivery_retry(retry: &DeliveryRetryResponse) -> String {
    key_values(&[
        ("state", state_name(retry.state).to_owned()),
        ("route_version", retry.route_version.to_string()),
        ("pending_seq", or_none(retry.pending_seq)),
        ("last_accepted_seq", or_none(retry.last_accepted_seq)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_is_escaped_onto_one_line() {
        assert_eq!(escape_body("a\nb"), "a\\nb");
        assert_eq!(escape_body("tab\there\r"), "tab\\there\\r");
        assert_eq!(escape_body("back\\slash"), "back\\\\slash");
        assert_eq!(escape_body("bell\u{7}"), "bell\\u{7}");
        assert_eq!(escape_body("한글 그대로"), "한글 그대로");
    }

    #[test]
    fn table_pads_every_column_but_the_last() {
        let mut table = Table::new(vec!["A", "LONG_HEADER", "BODY"]);
        table.push(vec!["wide-cell".into(), "x".into(), "free text".into()]);
        assert_eq!(
            table.render(),
            "A          LONG_HEADER  BODY\nwide-cell  x            free text\n"
        );
    }

    #[test]
    fn key_values_align_values() {
        assert_eq!(
            key_values(&[("a", "1".into()), ("longer", "-".into())]),
            "a       1\nlonger  -\n"
        );
    }

    #[test]
    fn profile_puts_intro_last_and_dashes_for_null() {
        let mut entry = ProfileResponse {
            agent_id: "agent-a".parse().unwrap(),
            name: None,
            profile: AgentProfile {
                gender_form: Some(platform_wire::GenderForm::None),
                ..AgentProfile::default()
            },
        };
        assert_eq!(
            profile(&entry),
            "agent_id     agent-a\nname         -\ngender_form  none\nage_band     -\n\
             role         -\nupdated_at   -\nintro        -\n"
        );
        entry.profile.intro = Some("첫 줄\n둘째 줄".into());
        assert!(profile(&entry).ends_with("updated_at   -\nintro\n첫 줄\n둘째 줄\n"));
    }

    #[test]
    fn missing_values_render_as_a_dash() {
        assert_eq!(or_none(None::<Seq>), NONE);
        assert_eq!(or_none(Some(Seq::ZERO)), "0");
    }
}
