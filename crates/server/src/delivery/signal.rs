//! `DeliverySignal` (`IF-DB-002`): a wake-up raised after a commit that
//! created inbox rows. The delivery worker waits on it or on its next due
//! time, whichever comes first.

use std::sync::Arc;

use tokio::sync::Notify;

/// Cheap to clone; all clones share one [`Notify`].
#[derive(Clone, Default)]
pub struct DeliverySignal(Arc<Notify>);

impl DeliverySignal {
    pub fn new() -> Self {
        Self::default()
    }

    /// Wakes the waiting worker, or stores one permit if none is waiting, so
    /// a commit between two waits is never lost.
    pub fn notify(&self) {
        self.0.notify_one();
    }

    /// Completes at the next [`DeliverySignal::notify`], or at once when a
    /// permit is stored.
    pub async fn notified(&self) {
        self.0.notified().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_notify_before_the_wait_is_kept() {
        let signal = DeliverySignal::new();
        signal.clone().notify();
        // Completes immediately thanks to the stored permit; hangs otherwise.
        signal.notified().await;
    }
}
