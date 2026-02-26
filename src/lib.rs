mod affinity;
mod barrier;
mod builder;
mod consumer;
mod cursor;
mod disruptor;
mod errors;
mod producer;
mod ring_buffer;
mod wait_strategies;

pub type Sequence = i64;

pub use crate::builder::{
    ProcessorSettings, build_multi_producer_unchecked, build_uni_producer_unchecked,
};
pub use crate::consumer::unmanaged::{EventGuard, EventPoller};
pub use crate::consumer::{MultiConsumerBarrier, UniConsumerBarrier};
pub use crate::disruptor::Disruptor;
pub use crate::producer::{
    multi::{MultiProducer, MultiProducerBarrier},
    uni::{UniProducer, UniProducerBarrier},
};
pub use crate::wait_strategies::{BusySpin, BusySpinWithSpinLoopHint};

#[cfg(test)]
mod tests {
    use crate::disruptor::Disruptor;

    use super::*;
    // use producer::MissingFreeSlots;
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::rc::Rc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering::Relaxed;
    use std::sync::{Arc, mpsc};
    use std::thread;

    #[derive(Debug)]
    struct Event {
        num: i64,
    }

    fn factory() -> impl Fn() -> Event {
        || Event { num: -1 }
    }

    #[test]
    #[should_panic(expected = "Size must be power of 2.")]
    fn size_not_a_factor_of_2() {
        // Disruptor::with_capacity::<3>()
        // Disruptor::with_capacity::<3>().build_uni_producer(|| 0, BusySpin);
        // build_uni_producer::<3>(|| 0, BusySpin);
        // build_uni_producer_unchecked(3, || 0, BusySpin);
    }
}
