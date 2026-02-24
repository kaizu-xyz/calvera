use std::cell::UnsafeCell;

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
            panic!("Ring buffer size must be power of 2.")
        }

        let slots: Box<[UnsafeCell<T>]> =
            (0..size).map(|_| UnsafeCell::new(event_source())).collect();
        let index_mask = (size - 1) as i64;

        RingBuffer { slots, index_mask }
    }
}
