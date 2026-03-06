//! Managed (push-based) consumer threads.
//!
//! The disruptor owns the consumer thread. You supply a closure and it gets
//! called for every event on a dedicated thread — you don't manage the thread
//! yourself.
//!
//! Two flavours:
//!
//! - [`start_processor`]: Stateless handler `FnMut(&E, Sequence, bool)`.
//! - [`start_processor_with_state`]: Stateful handler `FnMut(&mut S, &E, Sequence, bool)`.
//!   The state is initialized on the consumer thread via a `FnOnce() -> S` closure,
//!   so it doesn't need to be `Send` — useful for types like `Rc<RefCell<T>>`.
//!
//! Both functions return `(Arc<Cursor>, ConsumerHandle)`:
//! - The [`Cursor`] is registered with the consumer barrier so producers can
//!   track this consumer's progress.
//! - The [`ConsumerHandle`] is stored by the producer and joined at shutdown.

use crate::sync::{
    Arc,
    atomic::{AtomicI64, Ordering, fence},
    thread,
};

use crossbeam_utils::CachePadded;

use crate::{
    Sequence,
    affinity::set_affinity_if_defined,
    barrier::{Barrier, NONE},
    builder::BuilderContext,
    consumer::ConsumerHandle,
    cursor::Cursor,
    wait_strategies::WaitStrategy,
};

/// Spawns a managed consumer thread with a stateless event handler.
///
/// The consumer thread runs a loop:
/// 1. Wait for new events via the barrier (using the given wait strategy).
/// 2. Process all available events in a batch — the handler receives `end_of_batch`
///    as `true` on the last event of each batch.
/// 3. Advance the consumer cursor so the producer (or downstream consumers)
///    know this consumer is done with those slots.
/// 4. Repeat until shutdown.
pub(crate) fn start_processor<E, EP, W, B>(
    mut event_handler: EP,
    builder: &mut BuilderContext<E, W>,
    barrier: Arc<B>,
) -> (Arc<Cursor>, ConsumerHandle)
where
    E: 'static + Send + Sync,
    EP: 'static + Send + FnMut(&E, Sequence, bool),
    W: 'static + WaitStrategy,
    B: 'static + Barrier + Send + Sync,
{
    let consumer_cursor = Arc::new(Cursor::new(NONE)); // Initially, the consumer has not read slot 0 yet.
    let wait_strategy = builder.wait_strategy;
    let ring_buffer = Arc::clone(&builder.ring_buffer);
    let shutdown_at_sequence = Arc::clone(&builder.shutdown_at_sequence);
    let thread_name = builder.thread_context.pop_name();
    let affinity = builder.thread_context.pop_affinity();
    let thread_builder = thread::Builder::new().name(thread_name.clone());

    let join_handle = {
        let consumer_cursor = Arc::clone(&consumer_cursor);
        thread_builder
            .spawn(move || {
                set_affinity_if_defined(affinity, thread_name.as_str());
                let mut sequence = 0;
                while let Some(available) = wait_for_events(
                    sequence,
                    &shutdown_at_sequence,
                    barrier.as_ref(),
                    &wait_strategy,
                ) {
                    while available >= sequence {
                        let end_of_batch = available == sequence;
                        // SAFETY: Shared read access — the sequence protocol guarantees the
                        // producer has finished writing before this sequence becomes available.
                        let event_ptr = ring_buffer.get_ref(sequence);
                        let event = unsafe { &*event_ptr };
                        event_handler(event, sequence, end_of_batch);
                        sequence += 1;
                    }
                    // Signal to producers or downstream consumers that we've processed up to here.
                    consumer_cursor.store(sequence - 1);
                }
            })
            .expect("Should spawn thread.")
    };

    let consumer_handle = ConsumerHandle::new(join_handle);
    (consumer_cursor, consumer_handle)
}

/// Spawns a managed consumer thread with a stateful event handler.
///
/// Identical to [`start_processor`] except:
/// - `initialize_state` is a `FnOnce() -> S` that runs on the consumer thread
///   to create thread-local state. Because it runs on the consumer thread,
///   `S` doesn't need to be `Send`.
/// - The event handler receives `&mut S` as its first argument on every event.
pub(crate) fn start_processor_with_state<E, EP, W, B, S, IS>(
    mut event_handler: EP,
    builder: &mut BuilderContext<E, W>,
    barrier: Arc<B>,
    initialize_state: IS,
) -> (Arc<Cursor>, ConsumerHandle)
where
    E: 'static + Send + Sync,
    IS: 'static + Send + FnOnce() -> S,
    EP: 'static + Send + FnMut(&mut S, &E, Sequence, bool),
    W: 'static + WaitStrategy,
    B: 'static + Barrier + Send + Sync,
{
    let consumer_cursor = Arc::new(Cursor::new(-1)); // Initially, the consumer has not read slot 0 yet.
    let wait_strategy = builder.wait_strategy;
    let ring_buffer = Arc::clone(&builder.ring_buffer);
    let shutdown_at_sequence = Arc::clone(&builder.shutdown_at_sequence);
    let thread_name = builder.thread_context.pop_name();
    let affinity = builder.thread_context.pop_affinity();
    let thread_builder = thread::Builder::new().name(thread_name.clone());
    let join_handle = {
        let consumer_cursor = Arc::clone(&consumer_cursor);
        thread_builder
            .spawn(move || {
                set_affinity_if_defined(affinity, thread_name.as_str());
                let mut sequence = 0;
                let mut state = initialize_state();
                while let Some(available_sequence) = wait_for_events(
                    sequence,
                    &shutdown_at_sequence,
                    barrier.as_ref(),
                    &wait_strategy,
                ) {
                    while available_sequence >= sequence {
                        let end_of_batch = available_sequence == sequence;
                        // SAFETY: Shared read access — the sequence protocol guarantees the
                        // producer has finished writing before this sequence becomes available.
                        let event_ptr = ring_buffer.get_ref(sequence);
                        let event = unsafe { &*event_ptr };
                        event_handler(&mut state, event, sequence, end_of_batch);
                        sequence += 1;
                    }
                    // Signal to producers or downstream consumers that we've processed up to here.
                    consumer_cursor.store(sequence - 1);
                }
            })
            .expect("Should spawn thread.")
    };

    let consumer = ConsumerHandle::new(join_handle);
    (consumer_cursor, consumer)
}

/// Spins until new events are available or the disruptor shuts down.
///
/// Returns `Some(highest_available_sequence)` when events are ready to process,
/// or `None` if the producer has shut down and this consumer has caught up.
///
/// The `Acquire` fence after the barrier load establishes a happens-before
/// relationship with the producer's `Release` store/fence at publish time,
/// guaranteeing that the consumer sees all data the producer wrote to the
/// ring buffer slots.
#[inline]
fn wait_for_events<B, W>(
    sequence: Sequence,
    shutdown_at_sequence: &CachePadded<AtomicI64>,
    barrier: &B,
    wait_strategy: &W,
) -> Option<Sequence>
where
    B: Barrier + Send + Sync,
    W: WaitStrategy,
{
    let mut available = barrier.get_after(sequence);
    while available < sequence {
        // If publisher(s) are done publishing events we're done when we've seen the last event.
        if shutdown_at_sequence.load(Ordering::Relaxed) == sequence {
            return None;
        }
        wait_strategy.wait_for(sequence);
        available = barrier.get_after(sequence);
    }
    fence(Ordering::Acquire);
    Some(available)
}
