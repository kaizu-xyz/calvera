use crate::sync::{Arc, thread::JoinHandle};

use crate::{Sequence, barrier::Barrier, cursor::Cursor};

pub mod managed;
pub mod unmanaged;

pub struct ConsumerHandle {
    /// Stores the handle to a thread. It doesn't send messages,
    /// doesn't synchronize, doesn't do anything while the thread is running.
    /// It sits there inert.                                                                                                                                       
    /// It only does work at shutdown, when you call .join()
    join_handle: Option<JoinHandle<()>>,
}

impl ConsumerHandle {
    pub(crate) fn new(join_handle: JoinHandle<()>) -> Self {
        Self {
            join_handle: Some(join_handle),
        }
    }

    /// Blocks the calling thread until the target thread exits.
    /// That's a single syscall, once, at the end of the disruptor's lifetime.
    pub(crate) fn join(&mut self) {
        if let Some(handle) = self.join_handle.take() {
            handle.join().expect("Consumer should not panic.")
        }
    }
}

/// Barrier tracking a single consumer.
pub struct UniConsumerBarrier {
    cursor: Arc<Cursor>,
}

/// Barrier tracking the minimum sequence of a group of consumers.
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
    /// Gets the available `Sequence` of the slowest consumer.
    #[inline]
    fn get_after(&self, _lower_bound: Sequence) -> Sequence {
        self.cursors.iter().fold(i64::MAX, |min_sequence, cursor| {
            let sequence = cursor.relaxed_value();
            std::cmp::min(sequence, min_sequence)
        })
    }
}
