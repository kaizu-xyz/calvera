use std::sync::atomic::{AtomicI64, Ordering};

use crossbeam_utils::CachePadded;

use crate::Sequence;

pub(crate) struct Cursor {
    counter: CachePadded<AtomicI64>,
}

impl Cursor {
    pub(crate) fn new(start: i64) -> Self {
        Self {
            counter: CachePadded::new(AtomicI64::new(start)),
        }
    }

    /// Stores `sequence` to the cursor with `Ordering::Release` semantics.
    #[inline]
    pub(crate) fn store(&self, sequence: Sequence) {
        self.counter.store(sequence, Ordering::Release);
    }

    /// Retrieves the cursor value with `Ordering::Relaxed` semantics.
    #[inline]
    pub(crate) fn relaxed_value(&self) -> Sequence {
        self.counter.load(Ordering::Relaxed)
    }
}
