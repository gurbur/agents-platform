//! Receiver notification delivery (`IF-DLV-001`, RECEIVER_WAKE_CONVENTION).
//!
//! - [`signal`]: the commit wake-up shared with the API handlers.
//! - [`transitions`]: the transition table as pure functions over one row.
//! - [`classify`]: what a response or transport failure means.
//! - [`client`]: the dedicated callback HTTP client.
//! - [`worker`]: the loop that schedules attempts and records outcomes.
//!
//! The SQL lives in `store::queries::delivery`.

pub mod classify;
pub mod client;
pub mod signal;
pub mod transitions;
pub mod worker;

pub use client::CallbackClient;
pub use signal::DeliverySignal;
pub use worker::{Worker, WorkerHandle};
