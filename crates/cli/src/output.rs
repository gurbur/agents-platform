//! Human-readable rendering. Every function returns the full text so the
//! commands only decide *what* to print; `--json` bypasses this module and
//! prints the server's raw body.

use std::fmt::Display;
use std::io::Write;

use platform_wire::{
    AgentEntry, DeliveryRetryResponse, DeliveryState, DeliveryStatus, HealthStatus, HistoryCursor,
    HistoryPage, InboxItem, InboxPage, MembershipResponse, Message, MessageResponse,
    OwnerAgentEntry, ReadMarkerResponse, ReceiverResponse, RegisterAgentResponse,
    RotateTokensResponse, Seq,
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

pub fn registration(agent: &RegisterAgentResponse) -> String {
    key_values(&[
        ("agent_id", agent.agent_id.to_string()),
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

pub fn owner_directory(agents: &[OwnerAgentEntry]) -> String {
    let mut table = Table::new(vec![
        "AGENT_ID",
        "JOINED_ALL",
        "DELIVERY",
        "ROUTE_VERSION",
        "CREATED_AT",
        "RECEIVER_URL",
    ]);
    for agent in agents {
        table.push(vec![
            agent.agent_id.to_string(),
            agent.joined_all.to_string(),
            state_name(agent.delivery.state).to_owned(),
            agent.route_version.to_string(),
            agent.created_at.clone(),
            or_none(agent.receiver_url.as_deref()),
        ]);
    }
    table.render()
}

pub fn agent_directory(agents: &[AgentEntry]) -> String {
    let mut table = Table::new(vec!["AGENT_ID", "JOINED_ALL"]);
    for agent in agents {
        table.push(vec![
            agent.agent_id.to_string(),
            agent.joined_all.to_string(),
        ]);
    }
    table.render()
}

pub fn receiver(route: &ReceiverResponse) -> String {
    key_values(&[
        ("agent_id", route.agent_id.to_string()),
        ("receiver_url", or_none(route.receiver_url.as_deref())),
        ("route_version", route.route_version.to_string()),
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

/// The commit result plus the `client_key` to reuse for a retry.
pub fn sent(response: &MessageResponse, client_key: &str) -> String {
    let message = &response.message;
    let mut text = key_values(&[
        ("message_id", message.message_id.to_string()),
        ("event_seq", message.event_seq.to_string()),
        ("room_id", message.room_id.to_string()),
        ("sender_id", message.sender_id.to_string()),
        ("reply_to", or_none(message.reply_to.as_ref())),
        ("created_at", message.created_at.clone()),
        ("client_key", client_key.to_owned()),
    ]);
    if response.deduplicated {
        text.push_str("(deduplicated — same client_key)\n");
    }
    text
}

fn inbox_table(items: &[InboxItem]) -> String {
    let mut table = Table::new(vec![
        "INBOX_SEQ",
        "ROOM_ID",
        "EVENT_SEQ",
        "MESSAGE_ID",
        "CREATED_AT",
        "SENDER_ID",
        "BODY",
    ]);
    for item in items {
        table.push(vec![
            item.inbox_seq.to_string(),
            item.room_id.to_string(),
            item.event_seq.to_string(),
            item.message_id.to_string(),
            item.created_at.clone(),
            item.sender_id.to_string(),
            escape_body(&item.body),
        ]);
    }
    table.render()
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
        "BODY",
    ]);
    for message in &page.items {
        table.push(vec![
            message.event_seq.to_string(),
            message.message_id.to_string(),
            message.created_at.clone(),
            message.sender_id.to_string(),
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
    key_values(&[
        ("message_id", message.message_id.to_string()),
        ("event_seq", message.event_seq.to_string()),
        ("room_id", message.room_id.to_string()),
        ("sender_id", message.sender_id.to_string()),
        ("reply_to", or_none(message.reply_to.as_ref())),
        ("created_at", message.created_at.clone()),
        ("body", escape_body(&message.body)),
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
    fn missing_values_render_as_a_dash() {
        assert_eq!(or_none(None::<Seq>), NONE);
        assert_eq!(or_none(Some(Seq::ZERO)), "0");
    }
}
