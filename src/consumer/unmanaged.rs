use std::sync::{Arc, atomic::AtomicI64};

use crossbeam_utils::CachePadded;

use crate::{barrier::Barrier, cursor::Cursor, ring_buffer::RingBuffer};

pub struct EventPoller<E, B> {
    ring_buffer: Arc<RingBuffer<E>>,
    dependent_barrier: Arc<B>,
    shutdown_at_sequence: Arc<CachePadded<AtomicI64>>,
    cursor: Arc<Cursor>,
}

impl<E, B> EventPoller<E, B>
where
    B: Barrier,
{
    pub(crate) fn new(
        ring_buffer: Arc<RingBuffer<E>>,
        dependent_barrier: Arc<B>,
        shutdown_at_sequence: Arc<CachePadded<AtomicI64>>,
        cursor: Arc<Cursor>,
    ) -> Self {
        Self {
            ring_buffer,
            dependent_barrier,
            shutdown_at_sequence,
            cursor,
        }
    }
}
