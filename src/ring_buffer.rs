use crate::sync::UnsafeCell;

use crate::Sequence;

unsafe impl<T> Sync for RingBuffer<T> {}

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

    /// The oldest sequence that the producer would overwrite if it were to publish at the given sequence.
    #[inline]
    fn wrap_point(&self, sequence: Sequence) -> Sequence {
        sequence - self.size()
    }

    #[inline]
    pub(crate) fn free_slots(
        &self,
        producer: Sequence,
        highest_read_by_consumers: Sequence,
    ) -> i64 {
        let wrap_point = self.wrap_point(producer);
        highest_read_by_consumers - wrap_point
    }

    #[inline]
    pub fn get(&self, sequence: Sequence) -> *mut T {
        let index = (sequence & self.index_mask) as usize;
        // SAFE: Index is within bounds - guaranteed by invariant and index mask.
        let slot = unsafe { self.slots.get_unchecked(index) };
        slot.get()
    }

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
