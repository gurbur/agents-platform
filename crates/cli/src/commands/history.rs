//! `history` → `GET /v1/messages` (`IF-CLI-001` S15: `--thread` or
//! `--top-level`, never both).

use clap::Args;
use platform_wire::{HistoryPage, MessageId, RoomId, Seq};

use super::Context;
use crate::client::Query;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Args)]
pub struct HistoryArgs {
    /// `all`, a canonical `dm:<a>:<b>` room (a < b) or `room:<slug>`
    #[arg(long, value_name = "ROOM")]
    room: RoomId,

    /// Only this thread: its root and replies
    #[arg(long, value_name = "ROOT_ID", conflicts_with = "top_level")]
    thread: Option<MessageId>,

    /// Only messages outside threads (roots included)
    #[arg(long)]
    top_level: bool,

    /// Oldest first, strictly after event_seq N
    #[arg(long, value_name = "N", conflicts_with = "before")]
    after: Option<Seq>,

    /// Newest first, strictly before event_seq N (the default direction)
    #[arg(long, value_name = "N")]
    before: Option<Seq>,

    /// Page size (server default and maximum apply)
    #[arg(long, value_name = "N")]
    limit: Option<u32>,

    /// Literal substring of the body
    #[arg(long, value_name = "TEXT")]
    q: Option<String>,
}

impl HistoryArgs {
    fn query(&self) -> Query {
        let mut query: Query = vec![("room_id", self.room.to_string())];
        if let Some(thread) = &self.thread {
            query.push(("thread_id", thread.to_string()));
        }
        if self.top_level {
            query.push(("top_level", "true".to_owned()));
        }
        if let Some(after) = self.after {
            query.push(("after_event_seq", after.to_string()));
        }
        if let Some(before) = self.before {
            query.push(("before_event_seq", before.to_string()));
        }
        if let Some(limit) = self.limit {
            query.push(("limit", limit.to_string()));
        }
        if let Some(q) = &self.q {
            query.push(("q", q.clone()));
        }
        query
    }
}

pub async fn run(args: HistoryArgs, context: &Context) -> Result<(), CliError> {
    let response = context
        .client
        .get::<HistoryPage>("/v1/messages", &args.query())
        .await?;
    context.show(&response, output::history_page)
}
