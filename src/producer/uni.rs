use std::sync::{Arc, atomic::AtomicI64};

use crossbeam_utils::CachePadded;

use crate::{
    Sequence,
    barrier::{Barrier, NONE},
    consumer::Consumer,
    cursor::Cursor,
    ring_buffer::RingBuffer,
};

pub struct UniProducer<E, C> {
    /// Shared with consumer threads. When the producer drops, it writes its
    /// current sequence here. Consumers check this to know when to exit:
    /// "if I've processed up to this sequence, I'm done."
    /// CachePadded prevents false sharing with adjacent atomics.
    shutdow_at_sequence: Arc<CachePadded<AtomicI64>>,
    /// The shared ring buffer. Arc since both consumers and producers
    /// hold a reference to it, but under the hood it has interior mutability
    ring_buffer: Arc<RingBuffer<E>>,
    /// Cursor that tells consumers up to what what sequence the producer has published.
    /// Shared via Arc because consumers read it. The producer writes to it after every publish.
    producer_barrier: Arc<SingleProducerBarrier>,
    /// Owns the consumer thread handles (JoinHandle).
    /// When the producer drops, it joins all consumer threads to ensure clean shutdown.
    ///
    /// The producer owns the entire disruptor lifecycle.
    consumers: Vec<Consumer>,
    /// The producer reads this to check the slowest consumer's position.
    /// It's generic over it since it can be single or multi consumer.
    ///
    /// It holds references (Arc<Cursor>) to the consumer cursors internally.
    consumer_barrier: C,
    /// Next sequence to publish.
    sequence: Sequence,
    /// A cache optimization. Instead of checking the consumer barrier on every publish,
    /// the producer remembers. The producer re-checks the consumer barrier when
    /// it's about to exceed this cached value.
    ///
    /// This avoids an atomic load on most publishes.
    ///
    /// Under heavy backpressure this cache becomes ineffective: free_slots comes
    /// back as 0-1, the cached value barely advances, and the producer re-checks
    /// the consumer barrier on every publish. That's acceptable — if backpressure
    /// is that severe, one extra atomic load is the least of your problems.
    /// The optimization targets the happy path where consumers keep up and the
    /// cache gives hundreds/thousands of publishes per atomic read.
    sequence_clear_of_consumers: Sequence,
}

// re cache padding: When two unrelated atomic variables happen to sit on the same 64-byte cache line, a write to one invalidates the cache line for all cores reading the other. This is false sharing — threads that   aren't logically sharing data are forced to synchronize because of physical memory layout.

pub struct SingleProducerBarrier {
    cursor: Cursor,
}

impl SingleProducerBarrier {
    pub(crate) fn new() -> Self {
        Self {
            cursor: Cursor::new(NONE),
        }
    }
}

impl Barrier for SingleProducerBarrier {
    fn get_after(&self, _sequence: Sequence) -> i64 {
        self.cursor.relaxed_value()
    }
}
