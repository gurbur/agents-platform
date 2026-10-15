//! Domain queries. Each function takes a connection or transaction from
//! [`super::Store::run`] / [`super::Store::transaction`] and performs no I/O
//! besides SQLite.

pub mod agents;
