use std::{
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering, fence},
    },
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
                        // Potentiel batch processing.
                        let end_of_batch = available == sequence;
                        // SAFETY: Now, we have (shared) read access to the event at `sequence`.
                        let event_ptr = ring_buffer.get(sequence);
                        let event = unsafe { &*event_ptr };
                        event_handler(event, sequence, end_of_batch);
                        // Update next sequence to read.
                        sequence += 1;
                    }
                    // Signal to producers or later consumers that we're done processing `sequence - 1`.
                    consumer_cursor.store(sequence - 1);
                }
            })
            .expect("Should spawn thread.")
    };

    let consumer_handle = ConsumerHandle::new(join_handle);
    (consumer_cursor, consumer_handle)
}

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
                        // Potentiel batch processing.
                        let end_of_batch = available_sequence == sequence;
                        // SAFETY: Now, we have (shared) read access to the event at `sequence`.
                        let event_ptr = ring_buffer.get(sequence);
                        let event = unsafe { &*event_ptr };
                        event_handler(&mut state, event, sequence, end_of_batch);
                        // Update next sequence to read.
                        sequence += 1;
                    }
                    // Signal to producers or later consumers that we're done processing `sequence - 1`.
                    consumer_cursor.store(sequence - 1);
                }
            })
            .expect("Should spawn thread.")
    };

    let consumer = ConsumerHandle::new(join_handle);
    (consumer_cursor, consumer)
}

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
