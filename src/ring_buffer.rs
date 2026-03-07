//! Core data structure of the Disruptor: a pre-allocated, fixed-size ring buffer.
//!
//! The buffer size must be a power of two so that sequence-to-slot indexing
//! is a single bitwise AND rather than a modulo division. Each slot uses
//! [`UnsafeCell`] for interior mutability, and the buffer is
//! `unsafe impl Sync` because the sequence protocol in the producer/consumer
//! machinery guarantees exclusive write access and shared read access at the
//! type level — no runtime locks are needed.

use crate::sync::UnsafeCell;

use crate::Sequence;

// SAFETY: The Disruptor's sequence protocol ensures that a slot is only ever
// written by exactly one producer at a time (the producer must claim and then
// publish a sequence), and consumers only read a slot after the producer has
// published the corresponding sequence. This guarantees no data races on the
// `UnsafeCell` contents without requiring runtime synchronization.
unsafe impl<T> Sync for RingBuffer<T> {}

/// Fixed-size, pre-allocated ring buffer indexed by sequence number.
///
/// Slots are allocated once at construction and reused indefinitely as the
/// sequence wraps around, avoiding any per-event allocation.
pub struct RingBuffer<T> {
    /// Heap allocated contiguous slice
    /// of slots with interior mutability.
    slots: Box<[UnsafeCell<T>]>,
    /// Converts a monotonically increasing sequence number
    /// into a recurring slot index. The mask chops off
    /// everything above the buffer size, leaving only the
    /// wraparound index. This only works when size is a power of 2,
    /// so we avoid slow module division `sequence % size` in facor of
    /// a bitwise AND `sequence & index_mask`.
    index_mask: i64,
}

impl<T> RingBuffer<T> {
    /// Creates a ring buffer with `size` slots, each initialized by calling `event_source`.
    ///
    /// # Panics
    ///
    /// Panics if `size` is not a power of two.
    pub fn new<F>(size: usize, mut event_source: F) -> Self
    where
        F: FnMut() -> T,
    {
        if !size.is_power_of_two() {
            // TODO: err should be handled gracefully
            panic!("Ring buffer size must be power of 2.")
        }

        let slots: Box<[UnsafeCell<T>]> =
            (0..size).map(|_| UnsafeCell::new(event_source())).collect();
        let index_mask = (size - 1) as i64;

        RingBuffer { slots, index_mask }
    }

    /// Returns the oldest sequence that would be overwritten if the producer
    /// publishes at `sequence`. Used to check whether all consumers have moved
    /// past that point before the producer may proceed.
    #[inline]
    fn wrap_point(&self, sequence: Sequence) -> Sequence {
        sequence - self.size()
    }

    /// Returns the number of slots available for the producer to write into,
    /// given the producer's current `producer` sequence and the slowest
    /// consumer's `highest_read_by_consumers` sequence.
    #[inline]
    pub(crate) fn free_slots(
        &self,
        producer: Sequence,
        highest_read_by_consumers: Sequence,
    ) -> i64 {
        let wrap_point = self.wrap_point(producer);
        highest_read_by_consumers - wrap_point
    }

    /// Returns a raw mutable pointer to the slot at `sequence`.
    ///
    /// Used by the producer to write into the slot. Under loom, this
    /// registers an exclusive write access for data-race detection.
    #[inline]
    pub fn get(&self, sequence: Sequence) -> *mut T {
        let index = (sequence & self.index_mask) as usize;
        // SAFE: Index is within bounds - guaranteed by invariant and index mask.
        let slot = unsafe { self.slots.get_unchecked(index) };
        slot.get()
    }

    /// Returns a raw const pointer to the slot at `sequence` (read-only access).
    ///
    /// Consumers should use this instead of [`get`](Self::get). Under loom,
    /// this registers a shared read (via `with()`) rather than an exclusive
    /// write (via `with_mut()`), so multiple concurrent consumers can read the
    /// same slot without triggering a causality violation.
    #[inline]
    pub fn get_ref(&self, sequence: Sequence) -> *const T {
        let index = (sequence & self.index_mask) as usize;
        let slot = unsafe { self.slots.get_unchecked(index) };
        slot.get_ref()
    }

    /// Returns the number of slots in the ring buffer.
    #[inline]
    pub(crate) fn size(&self) -> i64 {
        self.slots.len() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_slots() {
        let ring_buffer = RingBuffer::new(8, || 0);

        // Round 1:
        // Publisher has just published 7 and consumer has read 0.
        assert_eq!(1, ring_buffer.free_slots(7, 0));
        // Consumer had read 0 and the publisher has just published 8.
        assert_eq!(0, ring_buffer.free_slots(8, 0));
        // Producer has published 0 and comsumer read 0.
        assert_eq!(8, ring_buffer.free_slots(0, 0));
        // Publisher has just published 3 and consumer is (still) reading 0.
        assert_eq!(4, ring_buffer.free_slots(3, -1));
        // Publisher has just published 7 and consumer is (still) reading 0.
        assert_eq!(0, ring_buffer.free_slots(7, -1));
        // Publisher has just published 7 and consumer has read 0.
        assert_eq!(1, ring_buffer.free_slots(7, 0));
        // Publisher has just released 5 and consumer has read 2.
        assert_eq!(5, ring_buffer.free_slots(5, 2));
        // Publisher has just released 5 and consumer has read 3.
        assert_eq!(6, ring_buffer.free_slots(5, 3));
        // Publisher has just released 5 and consumer has read 4.
        assert_eq!(7, ring_buffer.free_slots(5, 4));
        // Publisher has just released 5 and consumer has read 5.
        assert_eq!(8, ring_buffer.free_slots(5, 5));

        // Round 2:
        // Publisher has just published 11 and consumer has read 9.
        assert_eq!(6, ring_buffer.free_slots(11, 9));
        // Publisher has just published 12 and consumer has read 12.
        assert_eq!(8, ring_buffer.free_slots(12, 12));
        // Consumer has read 7 and the publisher has just published 15.
        assert_eq!(0, ring_buffer.free_slots(15, 7));
    }
}
