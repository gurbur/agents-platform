//! Wire types shared by `platformd` and the `platform` CLI.
//!
//! This crate is the single validation point for identifiers and sequence
//! representations (`IF-ID-001`). It deliberately depends only on serde,
//! serde_json and thiserror.

pub mod agent_id;
pub mod error;
pub mod health;
pub mod seq;

pub use agent_id::{AgentId, AgentIdError};
pub use error::{ErrorBody, ErrorDetail, codes};
pub use health::HealthStatus;
pub use seq::{Seq, SeqError};
