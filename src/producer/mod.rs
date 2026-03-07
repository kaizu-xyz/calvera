//! Producer-side types for the disruptor.
//!
//! Defines the [`Producer`] trait for publishing events into the ring buffer,
//! [`ProducerBarrier`] for signaling availability to consumers, and
//! [`MutBatchIter`] for batch writes.
//!
//! Two concrete implementations are provided:
//! - [`uni`] — single-producer (no CAS, just a local counter increment).
//! - [`multi`] — multi-producer (CAS-based sequence claiming with an availability bitfield).

use crate::{
    Sequence,
    barrier::Barrier,
    errors::{EMissingFreeSlots, ERingBufferFull},
    ring_buffer::RingBuffer,
};

pub mod multi;
pub mod uni;

/// Extension of [`Barrier`] that adds a `publish` method to make a sequence
/// visible to consumers. The consumer cannot read past the producer barrier;
/// it blocks progress until the producer has published further.
///
/// The uni variant (`UniProducerBarrier`) uses a simple `Release` store on
/// a single cursor. The multi variant (`MultiProducerBarrier`) XORs a bit
/// in an availability bitfield to encode even/odd round publication.
pub trait ProducerBarrier: Barrier {
    fn publish(&self, sequence: Sequence);
}

/// Trait for publishing events into a disruptor's ring buffer.
///
/// Provides two pairs of methods — single-event and batch — each with a
/// fallible (`try_`) and an infallible (busy-spin) variant:
///
/// | Mode   | Fallible              | Infallible       |
/// |--------|-----------------------|------------------|
/// | Single | [`try_publish`]       | [`publish`]      |
/// | Batch  | [`try_batch_publish`] | [`batch_publish`] |
///
/// The `try_` variants return an error immediately if the ring buffer does
/// not have enough free slots:
/// - `ERingBufferFull` — no slot available for a single publish. This means
///   consumers cannot keep up and the producer is experiencing backpressure.
/// - `EMissingFreeSlots` — not enough contiguous slots for a batch publish.
///
/// The non-`try` variants busy-spin until space becomes available, so they
/// never fail but will block the calling thread under backpressure.
///
/// Sequences are monotonically increasing `i64` values, giving a theoretical
/// limit of 2^63 - 1 events before overflow.
///
/// [`try_publish`]: Producer::try_publish
/// [`publish`]: Producer::publish
/// [`try_batch_publish`]: Producer::try_batch_publish
/// [`batch_publish`]: Producer::batch_publish
pub trait Producer<E> {
    /// Attempts to publish a single event. Returns the published sequence on
    /// success, or `ERingBufferFull` if no slot is available.
    fn try_publish<F>(&mut self, update: F) -> Result<Sequence, ERingBufferFull>
    where
        F: FnOnce(&mut E);

    /// Publishes a single event, busy-spinning until a slot is free.
    /// Never fails, but blocks under backpressure.
    fn publish<F>(&mut self, update: F)
    where
        F: FnOnce(&mut E);

    /// Attempts to publish `n` events as a batch. Returns the highest published
    /// sequence on success, or `EMissingFreeSlots` if not enough contiguous
    /// slots are available.
    fn try_batch_publish<'a, F>(
        &'a mut self,
        n: usize,
        update: F,
    ) -> Result<Sequence, EMissingFreeSlots>
    where
        E: 'a,
        F: FnOnce(MutBatchIter<'a, E>);

    /// Publishes `n` events as a batch, busy-spinning until enough contiguous
    /// slots are free. Never fails, but blocks under backpressure.
    fn batch_publish<'a, F>(&'a mut self, n: usize, update: F)
    where
        E: 'a,
        F: FnOnce(MutBatchIter<'a, E>);
}

/// Iterator that yields `&mut E` references to a batch of consecutive ring
/// buffer slots. Instead of claiming and publishing one slot at a time, batch
/// publishing claims N slots in a single `next_sequences(n)` call, then walks
/// through them via this iterator — amortizing the cost of slot acquisition.
///
/// Each `.next()` advances `current`, gets the raw pointer via
/// `ring_buffer.get(current)`, and returns `&mut E`. When `current > last`,
/// iteration is done. The `'a` lifetime ties the mutable references to the
/// ring buffer — you can't hold onto them after the batch is published.
pub struct MutBatchIter<'a, E> {
    ring_buffer: &'a RingBuffer<E>,
    current: Sequence, // Inclusive
    last: Sequence,    // Inclusive
}

impl<'a, E> MutBatchIter<'a, E> {
    fn new(start: Sequence, end: Sequence, ring_buffer: &'a RingBuffer<E>) -> Self {
        Self {
            ring_buffer,
            current: start,
            last: end,
        }
    }

    fn remaining(&self) -> usize {
        (self.last - self.current + 1) as usize
    }
}

impl<'a, E> Iterator for MutBatchIter<'a, E> {
    type Item = &'a mut E;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current > self.last {
            None
        } else {
            let event_ptr = self.ring_buffer.get(self.current);
            // SAFE: Iterator has exclusive access to event.
            let event = unsafe { &mut *event_ptr };
            self.current += 1;
            Some(event)
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.remaining();
        (remaining, Some(remaining))
    }
}
