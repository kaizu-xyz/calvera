use crate::affinity::cpu_has_core_else_panic;
use crate::barrier::{Barrier, NONE};
use crate::builder::multi::MPBuilder;
use crate::builder::uni::UPBuilder;
use crate::consumer::ConsumerHandle;
use crate::consumer::managed::{start_processor, start_processor_with_state};
use crate::consumer::unmanaged::EventPoller;
use crate::wait_strategies::WaitStrategy;
use crate::{MultiProducerBarrier, Sequence, UniProducerBarrier};
use crate::{cursor::Cursor, ring_buffer::RingBuffer};
use core_affinity::CoreId;
use crossbeam_utils::CachePadded;

use crate::sync::{Arc, atomic::AtomicI64};

pub mod multi;
pub mod uni;

// We use markers instead of enums to avoid runtime check

/// State: No consumers (yet).
pub struct NC;
/// State: Single consumer.
pub struct SC;
/// State: Multiple consumers.
pub struct MC;

/// Build a single producer Disruptor. Use this if you only need to publish events from one thread.
///
/// For using a producer see [Producer](crate::producer::Producer).
///
/// # Examples
///
/// ```
///# use disruptor::*;
///#
/// // The example data entity on the ring buffer.
/// struct Event {
///     price: f64
/// }
/// let factory = || { Event { price: 0.0 }};
///# let processor1 = |e: &Event, _, _| {};
///# let processor2 = |e: &Event, _, _| {};
///# let processor3 = |e: &Event, _, _| {};
/// let mut producer = build_uni_producer(8, factory, BusySpin)
///    .handle_events_with(processor1)
///    .handle_events_with(processor2)
///    .and_then()
///        // `processor3` only reads events after the other two processors are done reading.
///        .handle_events_with(processor3)
///    .build();
///
/// // Now use the `producer` to publish events.
/// ```
pub fn build_uni_producer_unchecked<E, W, F>(
    size: usize,
    event_factory: F,
    wait_strategy: W,
) -> UPBuilder<NC, E, W, UniProducerBarrier>
where
    F: FnMut() -> E,
    E: 'static + Send + Sync,
    W: 'static + WaitStrategy,
{
    let producer_barrier = Arc::new(UniProducerBarrier::new());
    let dependent_barrier = Arc::clone(&producer_barrier);
    UPBuilder::new(
        size,
        event_factory,
        wait_strategy,
        producer_barrier,
        dependent_barrier,
    )
}

/// Build a multi producer Disruptor. Use this if you need to publish events from many threads.
///
/// For using a producer see [Producer](crate::producer::Producer).
///
/// # Examples
///
/// ```
///# use disruptor::*;
///#
/// // The example data entity on the ring buffer.
/// struct Event {
///     price: f64
/// }
/// let factory = || { Event { price: 0.0 }};
///# let processor1 = |e: &Event, _, _| {};
///# let processor2 = |e: &Event, _, _| {};
///# let processor3 = |e: &Event, _, _| {};
/// let mut producer1 = build_multi_producer(64, factory, BusySpin)
///    .handle_events_with(processor1)
///    .handle_events_with(processor2)
///    .and_then()
///        // `processor3` only reads events after the other two processors are done reading.
///        .handle_events_with(processor3)
///    .build();
///
/// let mut producer2 = producer1.clone();
///
/// // Now two threads can get a Producer each.
/// ```
pub fn build_multi_producer_unchecked<E, W, F>(
    size: usize,
    event_factory: F,
    wait_strategy: W,
) -> MPBuilder<NC, E, W, MultiProducerBarrier>
where
    F: FnMut() -> E,
    E: 'static + Send + Sync,
    W: 'static + WaitStrategy,
{
    let producer_barrier = Arc::new(MultiProducerBarrier::new(size));
    let dependent_barrier = Arc::clone(&producer_barrier);
    MPBuilder::new(
        size,
        event_factory,
        wait_strategy,
        producer_barrier,
        dependent_barrier,
    )
}

pub trait ProcessorSettings<E, W>: Sized {
    fn context(&mut self) -> &mut BuilderContext<E, W>;

    /// Pin processor thread on the core with `id` for the next added event handler.
    /// Outputs an error on stderr if the thread could not be pinned.
    fn pin_at_core(mut self, id: usize) -> Self {
        self.context().pin_at_core(id);
        self
    }

    /// Set a name for the processor thread for the next added event handler.
    fn thread_name(mut self, name: &'static str) -> Self {
        self.context().thread_named(name);
        self
    }
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
