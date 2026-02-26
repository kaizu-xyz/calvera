use crate::Sequence;
use crate::affinity::cpu_has_core_else_panic;
use crate::barrier::{Barrier, NONE};
use crate::consumer::ConsumerHandle;
use crate::consumer::managed::{start_processor, start_processor_with_state};
use crate::consumer::unmanaged::EventPoller;
use crate::wait_strategies::WaitStrategy;
use crate::{cursor::Cursor, ring_buffer::RingBuffer};
use core_affinity::CoreId;
use crossbeam_utils::CachePadded;
use std::sync::{Arc, atomic::AtomicI64};

pub mod uni;

// We use markers instead of enums to avoid runtime check

/// State: No consumers (yet).
pub struct NC;
/// State: Single consumer.
pub struct SC;
/// State: Multiple consumers.
pub struct MC;

pub trait ProcessorSettings<E, W>: Sized {
    fn context(&mut self) -> &mut BuilderContext<E, W>;
}

// builder's accumulator — the shared mutable state that gets built up as you chain builder methods, then consumed when you call .build()
pub struct BuilderContext<E, W> {
    pub(crate) shutdown_at_sequence: Arc<CachePadded<AtomicI64>>,
    pub(crate) ring_buffer: Arc<RingBuffer<E>>,
    pub(crate) consumer_handles: Vec<ConsumerHandle>,
    current_consumer_cursors: Option<Vec<Arc<Cursor>>>,
    pub(crate) wait_strategy: W,
    pub(crate) thread_context: ThreadContext,
}

impl<E, W> BuilderContext<E, W> {
    fn new<F>(size: usize, event_factory: F, wait_strategy: W) -> Self
    where
        F: FnMut() -> E,
    {
        let ring_buffer = Arc::new(RingBuffer::new(size, event_factory));
        let shutdown_at_sequence = Arc::new(CachePadded::new(AtomicI64::new(NONE)));
        let current_consumer_cursors = Some(vec![]);

        Self {
            ring_buffer,
            wait_strategy,
            shutdown_at_sequence,
            current_consumer_cursors,
            consumer_handles: vec![],
            thread_context: ThreadContext::default(),
        }
    }

    fn add_consumer_and_cursor(&mut self, consumer_handle: ConsumerHandle, cursor: Arc<Cursor>) {
        self.consumer_handles.push(consumer_handle);
        self.add_cursor(cursor);
    }

    fn add_cursor(&mut self, cursor: Arc<Cursor>) {
        self.current_consumer_cursors.as_mut().unwrap().push(cursor);
    }

    fn pin_at_core(&mut self, id: usize) {
        cpu_has_core_else_panic(id);
        self.thread_context.affinity = Some(CoreId { id });
    }

    fn thread_named(&mut self, name: &'static str) {
        self.thread_context.name = Some(name.to_owned());
    }
}

#[derive(Default)]
pub(crate) struct ThreadContext {
    affinity: Option<CoreId>,
    name: Option<String>,
    id: usize,
}

impl ThreadContext {
    pub(crate) fn pop_name(&mut self) -> String {
        self.name
            .take()
            .or_else(|| {
                self.id += 1;
                Some(format!("processor-{}", self.id))
            })
            .unwrap()
    }

    pub(crate) fn pop_affinity(&mut self) -> Option<CoreId> {
        self.affinity.take()
    }

    pub(crate) fn id(&self) -> usize {
        self.id
    }
}

/// Shared builder behavior for all producer types (uni, multi).
///
/// Consumer threads are spawned eagerly during build — each call to
/// `add_event_handler` spawns the thread immediately. By the time
/// `.build()` returns, all consumer threads are already running and
/// waiting for events. `.build()` just assembles the producer from
/// the accumulated cursors, join handles, and barriers.
trait Builder<E, W, B>: ProcessorSettings<E, W>
where
    E: 'static + Send + Sync,
    B: 'static + Barrier,
    W: 'static + WaitStrategy,
{
    fn add_event_handler<EH>(&mut self, event_handler: EH)
    where
        EH: 'static + Send + FnMut(&E, Sequence, bool),
    {
        let barrier = self.dependent_barrier();
        let (cursor, consumer) = start_processor(event_handler, self.context(), barrier);
        self.context().add_consumer_and_cursor(consumer, cursor);
    }

    fn get_event_poller(&mut self) -> EventPoller<E, B> {
        let cursor = Arc::new(Cursor::new(NONE)); // Initially, the consumer has not read slot 0 yet.
        self.context().add_cursor(Arc::clone(&cursor));

        EventPoller::new(
            Arc::clone(&self.context().ring_buffer),
            Arc::clone(&self.dependent_barrier()),
            Arc::clone(&self.context().shutdown_at_sequence),
            cursor,
        )
    }

    fn add_event_handler_with_state<EH, S, IS>(&mut self, event_handler: EH, initialize_state: IS)
    where
        EH: 'static + Send + FnMut(&mut S, &E, Sequence, bool),
        IS: 'static + Send + FnOnce() -> S,
    {
        let barrier = self.dependent_barrier();
        let (cursor, consumer) =
            start_processor_with_state(event_handler, self.context(), barrier, initialize_state);
        self.context().add_consumer_and_cursor(consumer, cursor);
    }

    fn dependent_barrier(&self) -> Arc<B>;
}
