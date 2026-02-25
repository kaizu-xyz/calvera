use crate::{
    Sequence,
    barrier::Barrier,
    errors::{EMissingFreeSlots, ERingBufferFull},
    ring_buffer::RingBuffer,
};

pub mod multi;
pub mod uni;

// Producer barrier. The consumer can't read past the producer barrier.
// Tt blocks progress until the producer has published further.
pub trait ProducerBarrier: Barrier {
    fn publish(&self, sequence: Sequence);
}

pub trait Producer<E> {
    fn try_publish<F>(&mut self, update: F) -> Result<Sequence, ERingBufferFull>
    where
        F: FnOnce(&mut E);

    fn publish<F>(&mut self, update: F)
    where
        F: FnOnce(&mut E);

    fn try_batch_publish<'a, F>(
        &'a mut self,
        n: usize,
        update: F,
    ) -> Result<Sequence, EMissingFreeSlots>
    where
        E: 'a,
        F: FnOnce(MutBatchIter<'a, E>);

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
