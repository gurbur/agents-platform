//! `health` → `GET /healthz` (`IF-OPS-001`).

use super::Context;
use crate::error::CliError;
use crate::output;

pub async fn run(context: &Context) -> Result<(), CliError> {
    let response = context.client.health().await?;
    context.show(&response, output::health)
}
