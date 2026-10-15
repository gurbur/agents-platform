//! `GET /healthz` response body (`IF-OPS-001`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthStatus {
    pub status: String,
    pub schema_version: u32,
}

impl HealthStatus {
    pub const OK: &'static str = "ok";

    pub fn ok(schema_version: u32) -> Self {
        Self {
            status: Self::OK.to_owned(),
            schema_version,
        }
    }
}
