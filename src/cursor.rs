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

    pub(crate) fn relaxed_value(&self) -> Sequence {
        self.counter.load(Ordering::Relaxed)
    }
}
