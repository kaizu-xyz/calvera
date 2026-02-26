use crossbeam_utils::CachePadded;

use crate::sync::{
    Arc,
    atomic::{AtomicI64, Ordering, fence},
};

use crate::{
    Sequence,
    barrier::{Barrier, NONE},
    consumer::ConsumerHandle,
    cursor::Cursor,
    errors::{EMissingFreeSlots, ERingBufferFull},
    producer::{MutBatchIter, Producer, ProducerBarrier},
    ring_buffer::RingBuffer,
};

pub struct UniProducer<E, C> {
    /// Shared with consumer threads. When the producer drops, it writes its
    /// current sequence here. Consumers check this to know when to exit:
    /// "if I've processed up to this sequence, I'm done."
    /// CachePadded prevents false sharing with adjacent atomics.
    shutdown_at_sequence: Arc<CachePadded<AtomicI64>>,
    /// The shared ring buffer. Arc since both consumers and producers
    /// hold a reference to it, but under the hood it has interior mutability
    ring_buffer: Arc<RingBuffer<E>>,
    /// Cursor that tells consumers up to what what sequence the producer has published.
    /// Shared via Arc because consumers read it. The producer writes to it after every publish.
    producer_barrier: Arc<UniProducerBarrier>,
    /// Owns the consumer thread handles (JoinHandle).
    /// When the producer drops, it joins all consumer threads to ensure clean shutdown.
    ///
    /// The producer owns the entire disruptor lifecycle.
    consumer_handles: Vec<ConsumerHandle>,
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

impl<E, C> Producer<E> for UniProducer<E, C>
where
    C: Barrier,
{
    /// Attempts to claim one slot and publish the event. Returns
    /// `Err(RingBufferFull)` immediately if no slot is available,
    /// giving the caller control over backpressure (retry, drop, log, etc.).
    #[inline]
    fn try_publish<F>(&mut self, update: F) -> Result<Sequence, ERingBufferFull>
    where
        F: FnOnce(&mut E),
    {
        self.next_sequences(1).map_err(|_| ERingBufferFull)?;
        let sequence = self.apply_update(update);
        Ok(sequence)
    }

    /// Claims one slot and publishes the event, busy-spinning until a slot is
    /// free. Guarantees the event will be published — never fails — but blocks
    /// the calling thread under backpressure.
    #[inline]
    fn publish<F>(&mut self, update: F)
    where
        F: FnOnce(&mut E),
    {
        while self.next_sequences(1).is_err() { /* Empty. */ }
        self.apply_update(update);
    }

    #[inline]
    fn try_batch_publish<'a, F>(
        &'a mut self,
        n: usize,
        update: F,
    ) -> Result<Sequence, EMissingFreeSlots>
    where
        E: 'a,
        F: FnOnce(MutBatchIter<'a, E>),
    {
        self.next_sequences(n)?;
        let sequence = self.apply_updates(n, update);
        Ok(sequence)
    }

    #[inline]
    fn batch_publish<'a, F>(&'a mut self, n: usize, update: F)
    where
        E: 'a,
        F: FnOnce(MutBatchIter<'a, E>),
    {
        while self.next_sequences(n).is_err() { /* Empty. */ }
        self.apply_updates(n, update);
    }
}

/// Stops the processor thread and drops the Disruptor, the processor thread and the [Producer].
impl<E, C> Drop for UniProducer<E, C> {
    fn drop(&mut self) {
        self.shutdown_at_sequence
            .store(self.sequence, Ordering::Relaxed);
        self.consumer_handles.iter_mut().for_each(|c| c.join());
    }
}

impl<E, C> UniProducer<E, C>
where
    C: Barrier,
{
    pub(crate) fn new(
        shutdown_at_sequence: Arc<CachePadded<AtomicI64>>,
        ring_buffer: Arc<RingBuffer<E>>,
        producer_barrier: Arc<UniProducerBarrier>,
        consumer_handles: Vec<ConsumerHandle>,
        consumer_barrier: C,
    ) -> Self {
        let sequence_clear_of_consumers = ring_buffer.size() - 1;
        Self {
            shutdown_at_sequence,
            ring_buffer,
            producer_barrier,
            consumer_handles,
            consumer_barrier,
            sequence: 0,
            sequence_clear_of_consumers,
        }
    }

    /// Tries to claim `n` consecutive sequences for publishing.
    ///
    /// Fast path: if `sequence_clear_of_consumers` says we're clear, no atomic
    /// load is needed. Slow path (cache miss): queries the real consumer barrier.
    ///
    /// Returns `Ok(highest_claimed_sequence)` or `Err` if consumers haven't
    /// caught up and there aren't enough free slots.
    #[inline]
    fn next_sequences(&mut self, n: usize) -> Result<Sequence, EMissingFreeSlots> {
        // number of events the producer wants to publish
        let n = n as i64;
        // highest sequence we'd need to write to
        let n_next = self.sequence - 1 + n;

        // we need `n_next` if `sequence_clear_of_consumers` is not
        // enough then we have a cache miss, thus we need to check the actual
        // consumers

        // Fast path: cached value says we're clear — skip the atomic load.
        // Slow path: cache miss — query the real consumer barrier.
        if self.sequence_clear_of_consumers < n_next {
            let last_published = self.sequence - 1;
            let rear_sequence_read = self.consumer_barrier.get_after(last_published);
            let free_slots = self
                .ring_buffer
                .free_slots(last_published, rear_sequence_read);
            if free_slots < n {
                return Err(EMissingFreeSlots((n - free_slots) as u64));
            }

            fence(Ordering::Acquire);

            // We can now continue until we get right behind the slowest consumer's current
            // position without checking where it actually is.
            self.sequence_clear_of_consumers = last_published + free_slots;
        }

        Ok(n_next)
    }

    /// Writes into the claimed slot and publishes the sequence.
    ///
    /// Gets a raw pointer to the slot via `ring_buffer.get()`, converts it to
    /// `&mut E` (safe because the sequence protocol guarantees exclusive access),
    /// then calls the `update` closure to write in-place. Finally publishes the
    /// sequence so consumers can see it.
    #[inline]
    fn apply_update<F>(&mut self, update: F) -> Sequence
    where
        F: FnOnce(&mut E),
    {
        let sequence = self.sequence;
        let event_ptr = self.ring_buffer.get(sequence);

        // SAFE: the sequence protocol that nobody else holds a
        // reference to these slots right now. We have exclusive access
        // to the events between `lower` and `upper` and a producer can update the data.
        let event = unsafe { &mut *event_ptr };

        // We therefore give exclusive mutable access to the `update` fn
        update(event);

        self.producer_barrier.publish(sequence);
        self.sequence += 1;
        sequence
    }

    #[inline]
    fn apply_updates<'a, F>(&'a mut self, n: usize, updates: F) -> Sequence
    where
        E: 'a,
        F: FnOnce(MutBatchIter<'a, E>),
    {
        let n = n as i64;
        let lower = self.sequence;
        let upper = lower + n - 1;
        // SAFE: WE have exclusive access to the events between `lower` and `upper`
        // and a producer can update the data.
        let iter = MutBatchIter::new(lower, upper, &self.ring_buffer);
        updates(iter);
        // Publish batch by publishing `upper`.
        self.producer_barrier.publish(upper);
        // Update sequence that will be published the next time.
        self.sequence += n;
        upper
    }
}

pub struct UniProducerBarrier {
    cursor: Cursor,
}

impl UniProducerBarrier {
    pub fn new() -> Self {
        Self {
            cursor: Cursor::new(NONE),
        }
    }
}

impl Barrier for UniProducerBarrier {
    /// Gets the `Sequence` of the last published event.
    #[inline]
    fn get_after(&self, _sequence: Sequence) -> i64 {
        self.cursor.relaxed_value()
    }
}

impl ProducerBarrier for UniProducerBarrier {
    #[inline]
    fn publish(&self, sequence: Sequence) {
        self.cursor.store(sequence);
    }
}
