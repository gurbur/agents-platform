//! Domain queries. Each function takes a connection or transaction from
//! [`super::Store::run`] / [`super::Store::transaction`] and performs no I/O
//! besides SQLite.

pub mod agents;
pub mod avatars;
pub mod delivery;
pub mod inbox;
pub mod messages;
pub mod reaction_kinds;
pub mod reactions;
pub mod rooms;
pub mod threads;
