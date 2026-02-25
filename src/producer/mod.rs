use crate::{Sequence, barrier::Barrier};

pub mod multi;
pub mod uni;

// Producer barrier. The consumer can't read past the producer barrier.
// Tt blocks progress until the producer has published further.
pub trait ProducerBarrier: Barrier {
    fn publish(&self, sequence: Sequence);
}
