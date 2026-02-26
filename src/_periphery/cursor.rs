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

    /// (lock-free CAS) to claim sequences
    /// CAS (compare-and-swap) that multiple producers use to claim sequences without locks.
    /// Two producers racing to claim slot 5: one wins the CAS, the other gets Err(5) and retries for slot 6. No locks, no waiting — just a retry loop.
    /// This is the core of lock-free multi-producer coordination.
    ///
    /// UniProducer doesn't need this — there's only one producer, so it just
    /// increments `self.sequence` directly. No contention, no CAS.
    #[inline]
    pub(crate) fn compare_exchange(&self, current: Sequence, next: Sequence) -> Result<i64, i64> {
        self.counter
            .compare_exchange(current, next, Ordering::AcqRel, Ordering::Relaxed)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_operations() {
        let cursor = Cursor::new(-1);

        assert_eq!(cursor.compare_exchange(-1, 0).ok().unwrap(), -1);
        assert_eq!(cursor.compare_exchange(0, 1).ok().unwrap(), 0);
        // Simulate other thread having updated the cursor.
        assert_eq!(cursor.compare_exchange(0, 1).err().unwrap(), 1);

        cursor.store(100);
        assert_eq!(cursor.relaxed_value(), 100);
    }
}
