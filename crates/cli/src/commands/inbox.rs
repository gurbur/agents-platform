//! `inbox` → `GET /v1/inbox`, `inbox --all-pages` (snapshot traversal) and
//! `inbox read` → `PUT /v1/inbox/read-marker`.

use clap::{Args, Subcommand};
use platform_wire::{AgentId, InboxItem, InboxPage, ReadMarkerRequest, ReadMarkerResponse, Seq};
use reqwest::{Method, StatusCode};
use serde::Serialize;

use super::Context;
use crate::client::Query;
use crate::error::CliError;
use crate::output;

const INBOX_PATH: &str = "/v1/inbox";

/// Page selection shared by `inbox` and `agent inbox`.
#[derive(Debug, Clone, Copy, Args)]
pub struct PageArgs {
    /// Exclusive lower bound; defaults to the read marker
    #[arg(long, value_name = "N")]
    after_seq: Option<Seq>,

    /// Inclusive upper bound; pass a previous snapshot_head_seq to continue
    #[arg(long, value_name = "N")]
    until_seq: Option<Seq>,

    /// Page size (server default and maximum apply)
    #[arg(long, value_name = "N")]
    limit: Option<u32>,
}

impl PageArgs {
    pub fn query(&self) -> Query {
        let mut query = Query::new();
        if let Some(after_seq) = self.after_seq {
            query.push(("after_seq", after_seq.to_string()));
        }
        if let Some(until_seq) = self.until_seq {
            query.push(("until_seq", until_seq.to_string()));
        }
        if let Some(limit) = self.limit {
            query.push(("limit", limit.to_string()));
        }
        query
    }
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct InboxArgs {
    #[command(subcommand)]
    command: Option<InboxCommand>,

    #[command(flatten)]
    page: PageArgs,

    /// Read the whole snapshot: fix until_seq to the first snapshot_head_seq
    /// and follow next_after_seq until has_more is false. Never moves the
    /// read marker.
    #[arg(long)]
    all_pages: bool,
}

#[derive(Debug, Subcommand)]
pub enum InboxCommand {
    /// Confirm the contiguous prefix through N — PUT /v1/inbox/read-marker
    Read {
        #[arg(long, value_name = "N")]
        through_seq: Seq,
    },
}

/// `--all-pages --json` output: every item of one snapshot in one document.
#[derive(Serialize)]
struct Snapshot {
    agent_id: AgentId,
    read_marker: Seq,
    snapshot_head_seq: Seq,
    items: Vec<InboxItem>,
}

pub async fn run(args: InboxArgs, context: &Context) -> Result<(), CliError> {
    if let Some(InboxCommand::Read { through_seq }) = args.command {
        return read(through_seq, context).await;
    }
    if args.all_pages {
        return all_pages(args.page, context).await;
    }
    let response = context
        .client
        .get::<InboxPage>(INBOX_PATH, &args.page.query())
        .await?;
    context.show(&response, output::inbox_page)
}

async fn read(through_seq: Seq, context: &Context) -> Result<(), CliError> {
    let request = ReadMarkerRequest { through_seq };
    let response = context
        .client
        .send_json::<ReadMarkerResponse, _>(Method::PUT, "/v1/inbox/read-marker", &request)
        .await?;
    context.show(&response, output::read_marker)
}

/// PLATFORM_MESSAGING_SPEC "Inbox pagination snapshot": the first page's
/// `snapshot_head_seq` becomes `until_seq` for every later page, so rows
/// committed during the traversal are left for the next snapshot.
async fn all_pages(first_page: PageArgs, context: &Context) -> Result<(), CliError> {
    let first = fetch(&first_page, context).await?;
    let mut snapshot = Snapshot {
        agent_id: first.agent_id,
        read_marker: first.read_marker,
        snapshot_head_seq: first.snapshot_head_seq,
        items: first.items,
    };
    let mut after_seq = first.next_after_seq;
    let mut has_more = first.has_more;
    while has_more {
        let next_page = PageArgs {
            after_seq: Some(after_seq),
            until_seq: Some(snapshot.snapshot_head_seq),
            limit: first_page.limit,
        };
        let page = fetch(&next_page, context).await?;
        if page.has_more && page.next_after_seq <= after_seq {
            return Err(CliError::UnexpectedResponse {
                status: StatusCode::OK,
                detail: format!("inbox page after {after_seq} has more rows but did not advance"),
            });
        }
        snapshot.items.extend(page.items);
        after_seq = page.next_after_seq;
        has_more = page.has_more;
    }

    if context.json {
        // Serializing wire DTOs with plain fields cannot fail.
        let body = serde_json::to_vec(&snapshot).expect("snapshot serializes");
        return output::emit_raw(&body);
    }
    output::emit(&output::inbox_snapshot(
        snapshot.agent_id.as_str(),
        snapshot.read_marker,
        snapshot.snapshot_head_seq,
        &snapshot.items,
    ))
}

async fn fetch(page: &PageArgs, context: &Context) -> Result<InboxPage, CliError> {
    let response = context
        .client
        .get::<InboxPage>(INBOX_PATH, &page.query())
        .await?;
    Ok(response.value)
}
