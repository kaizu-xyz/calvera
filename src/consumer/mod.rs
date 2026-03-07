//! Consumer-side types and barriers.
//!
//! There are two consumer models:
//!
//! - **Managed** ([`managed`]): The disruptor owns the consumer thread. You provide
//!   a closure and the disruptor calls it for each event on a dedicated thread.
//!   This is the push model — events are pushed to your handler.
//!
//! - **Unmanaged** ([`unmanaged`]): You own the thread and call [`EventPoller::poll`]
//!   whenever you're ready. This is the pull model — you pull events at your own pace.
//!
//! Both models use **consumer barriers** to communicate progress back to the producer.
//! The producer reads the consumer barrier to know which ring buffer slots are safe
//! to overwrite. Without this backpressure signal, the producer could lap a slow
//! consumer and corrupt events that haven't been read yet.

use crate::sync::{Arc, thread::JoinHandle};

use crate::{Sequence, barrier::Barrier, cursor::Cursor};

pub mod managed;
pub mod unmanaged;

/// Handle to a managed consumer thread.
///
/// Created by [`managed::start_processor`] and owned by the producer.
/// The handle is inert while the consumer is running — it stores the
/// `JoinHandle` and only does work at shutdown when [`join`](Self::join)
/// is called to block until the consumer thread exits cleanly.
pub struct ConsumerHandle {
    join_handle: Option<JoinHandle<()>>,
}

impl ConsumerHandle {
    pub(crate) fn new(join_handle: JoinHandle<()>) -> Self {
        Self {
            join_handle: Some(join_handle),
        }
    }

    /// Blocks the calling thread until the consumer thread exits.
    /// Called during disruptor shutdown (when the producer is dropped).
    pub(crate) fn join(&mut self) {
        if let Some(handle) = self.join_handle.take() {
            handle.join().expect("Consumer should not panic.")
        }
    }
}

/// Consumer barrier for a single consumer (SPSC or single-consumer SPMC stage).
///
/// Wraps a single consumer's `Cursor` via `Arc`. The producer reads this
/// barrier to check how far the consumer has progressed. Since there's only
/// one consumer, `get_after` is a single relaxed atomic load.
pub struct UniConsumerBarrier {
    cursor: Arc<Cursor>,
}

/// Consumer barrier for multiple independent consumers.
///
/// Wraps the `Cursor` of every consumer in the group. The producer reads
/// this barrier to find the **slowest** consumer — because it can only
/// overwrite a slot once all consumers have finished reading it.
///
/// `get_after` scans all cursors and returns the minimum sequence. This is
/// O(n) in the number of consumers, but n is typically small (2-4) and
/// each cursor load is a single relaxed atomic — no locks.
pub struct MultiConsumerBarrier {
    cursors: Vec<Arc<Cursor>>,
}

impl UniConsumerBarrier {
    pub(crate) fn new(cursor: Arc<Cursor>) -> Self {
        Self { cursor }
    }
}

impl Barrier for UniConsumerBarrier {
    #[inline]
    fn get_after(&self, _lower_bound: Sequence) -> Sequence {
        self.cursor.relaxed_value()
    }
}

impl MultiConsumerBarrier {
    pub(crate) fn new(cursors: Vec<Arc<Cursor>>) -> Self {
        Self { cursors }
    }
}

impl Barrier for MultiConsumerBarrier {
    /// Returns the sequence of the slowest consumer in the group.
    ///
    /// The producer uses this to determine backpressure: it cannot publish
    /// into a slot until the slowest consumer has moved past it.
    #[inline]
    fn get_after(&self, _lower_bound: Sequence) -> Sequence {
        self.cursors.iter().fold(i64::MAX, |min_sequence, cursor| {
            let sequence = cursor.relaxed_value();
            std::cmp::min(sequence, min_sequence)
        })
    }
}
