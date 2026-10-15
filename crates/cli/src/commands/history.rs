//! `history` → `GET /v1/messages`.

use clap::Args;
use platform_wire::{HistoryPage, RoomId, Seq};

use super::Context;
use crate::client::Query;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Args)]
pub struct HistoryArgs {
    /// `all` or a canonical `dm:<a>:<b>` room (a < b)
    #[arg(long, value_name = "ROOM")]
    room: RoomId,

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
