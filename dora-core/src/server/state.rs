//! Server state. Used to count how many live messages are processing in the
//! system right now, and keep track of message id's
use tokio::sync::Semaphore;

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use crate::metrics::IN_FLIGHT;

/// Represents the current Server state
#[derive(Debug)]
pub struct State {
    /// current live message count
    live_msgs: Arc<Semaphore>,
    /// max live message count
    live_limit: usize,
    /// id to assign incoming messages
    next_id: AtomicU64,
}

impl State {
    /// Create new state with a set max live message count
    pub fn new(max_live: usize) -> State {
        State {
            live_msgs: Arc::new(Semaphore::new(max_live)),
            live_limit: max_live,
            next_id: AtomicU64::new(0),
        }
    }

    /// Increments the count of live in-flight messages
    pub async fn inc_live_msgs(&self) {
        // forget() must be used on the semaphore after acquire otherwise
        // it will add the permit back when the semaphore is dropped,
        // and we don't actually want to do that, we want to add it back
        //  when MsgContext is dropped
        //
        // SAFETY: acquire returns an Err when the semaphore is closed, which we never
        // do
        self.live_msgs.acquire().await.unwrap().forget();
        IN_FLIGHT.inc();
    }

    /// Decrements the count of live in-flight messages
    #[inline]
    pub fn dec_live_msgs(&self) {
        self.live_msgs.add_permits(1);
        IN_FLIGHT.dec();
    }

    /// Return the current number of live queries
    #[inline]
    pub fn live_msgs(&self) -> usize {
        self.live_limit - self.live_msgs.available_permits()
    }

    /// Increment the context id
    #[inline]
    pub fn inc_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Acquire)
    }

    /// Reset msgs count
    #[inline]
    pub fn reset_live(&self) {
        IN_FLIGHT.set(0);
    }
}
