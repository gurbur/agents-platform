//! Receiver notification delivery. S2 provides only the commit signal; the
//! worker that consumes it arrives in S4.

pub mod signal;

pub use signal::DeliverySignal;
