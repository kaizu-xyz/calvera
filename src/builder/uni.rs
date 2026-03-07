//! Builder for single-producer disruptors.
//!
//! Created via [`build_uni_producer_unchecked`](super::build_uni_producer_unchecked) or
//! `Disruptor::with_capacity().build_uni_producer()`.

use std::marker::PhantomData;

use crate::sync::Arc;

use crate::{
    Sequence,
    barrier::Barrier,
    builder::{Builder, BuilderContext, MC, NC, ProcessorSettings, SC},
    consumer::{MultiConsumerBarrier, UniConsumerBarrier, unmanaged::EventPoller},
    producer::uni::{UniProducer, UniProducerBarrier},
    wait_strategies::WaitStrategy,
};

/// Typestate builder for [`UniProducer`].
///
/// `State` tracks how many consumers have been added ([`NC`] -> [`SC`] -> [`MC`]).
/// `B` is the barrier type for the current consumer group's dependency — starts as the producer
/// barrier, then shifts to a consumer barrier after [`.and_then()`](UPBuilder::and_then).
pub struct UPBuilder<State, E, W, B> {
    state: PhantomData<State>,
    context: BuilderContext<E, W>,
    producer_barrier: Arc<UniProducerBarrier>,
    /// The barrier the next consumer must wait on before reading.
    /// Starts as the producer barrier (first consumer waits on the producer).
    /// After `.and_then()`, shifts to the previous consumer's cursor
    /// (sequential consumers wait on each other).
    /// Parallel consumers in the same group share the same dependent barrier.
    dependent_barrier: Arc<B>,
}

impl<E, W, B, S> ProcessorSettings<E, W> for UPBuilder<S, E, W, B> {
    fn context(&mut self) -> &mut BuilderContext<E, W> {
        &mut self.context
    }
}

impl<E, W, B, S> Builder<E, W, B> for UPBuilder<S, E, W, B>
where
    E: 'static + Send + Sync,
    W: 'static + WaitStrategy,
    B: 'static + Barrier,
{
    fn dependent_barrier(&self) -> Arc<B> {
        Arc::clone(&self.dependent_barrier)
    }
}

impl<E, W, B> UPBuilder<NC, E, W, B>
where
    E: 'static + Send + Sync,
    W: 'static + WaitStrategy,
    B: 'static + Barrier,
{
    pub(super) fn new<F>(
        size: usize,
        event_factory: F,
        wait_strategy: W,
        producer_barrier: Arc<UniProducerBarrier>,
        dependent_barrier: Arc<B>,
    ) -> Self
    where
        F: FnMut() -> E,
    {
        let context = BuilderContext::new(size, event_factory, wait_strategy);
        Self {
            state: PhantomData,
            context,
            producer_barrier,
            dependent_barrier,
        }
    }

    /// Get an EventPoller.
    /// method consumes self (takes ownership, destroying the old builder), and returns a new builder with the updated type parameter.
    pub fn event_poller(mut self) -> (EventPoller<E, B>, UPBuilder<SC, E, W, B>) {
        let event_poller = self.get_event_poller();

        (
            event_poller,
            UPBuilder {
                state: PhantomData,
                context: self.context,
                producer_barrier: self.producer_barrier,
                dependent_barrier: self.dependent_barrier,
            },
        )
    }

    /// Add an event handler.
    pub fn handle_events_with<EH>(mut self, event_handler: EH) -> UPBuilder<SC, E, W, B>
    where
        EH: 'static + Send + FnMut(&E, Sequence, bool),
    {
        self.add_event_handler(event_handler);
        UPBuilder {
            state: PhantomData,
            context: self.context,
            producer_barrier: self.producer_barrier,
            dependent_barrier: self.dependent_barrier,
        }
    }

    /// Add an event handler with state.
    pub fn handle_events_and_state_with<EH, S, IS>(
        mut self,
        event_handler: EH,
        initialize_state: IS,
    ) -> UPBuilder<SC, E, W, B>
    where
        EH: 'static + Send + FnMut(&mut S, &E, Sequence, bool),
        IS: 'static + Send + FnOnce() -> S,
    {
        self.add_event_handler_with_state(event_handler, initialize_state);
        UPBuilder {
            state: PhantomData,
            context: self.context,
            producer_barrier: self.producer_barrier,
            dependent_barrier: self.dependent_barrier,
        }
    }
}

impl<E, W, B> UPBuilder<SC, E, W, B>
where
    E: 'static + Send + Sync,
    W: 'static + WaitStrategy,
    B: 'static + Barrier,
{
    /// Finish the build and get a [`UniProducer`].
    pub fn build(mut self) -> UniProducer<E, UniConsumerBarrier> {
        let mut consumer_cursors = self.context().current_consumer_cursors.take().unwrap();
        // Guaranteed to be present by construction.
        let consumer_barrier = UniConsumerBarrier::new(consumer_cursors.remove(0));
        UniProducer::new(
            self.context.shutdown_at_sequence,
            self.context.ring_buffer,
            self.producer_barrier,
            self.context.consumer_handles,
            consumer_barrier,
        )
    }

    /// Get an EventPoller.
    pub fn event_poller(mut self) -> (EventPoller<E, B>, UPBuilder<MC, E, W, B>) {
        let event_poller = self.get_event_poller();

        (
            event_poller,
            UPBuilder {
                state: PhantomData,
                context: self.context,
                producer_barrier: self.producer_barrier,
                dependent_barrier: self.dependent_barrier,
            },
        )
    }

    /// Completes the current consumer group and starts a new pipeline stage.
    ///
    /// Consumers added after `.and_then()` will only see events after all consumers in the
    /// previous group have finished reading them. This is how you build sequential (diamond)
    /// consumer topologies.
    pub fn and_then(mut self) -> UPBuilder<NC, E, W, UniConsumerBarrier> {
        // Guaranteed to be present by construction.
        let consumer_cursors = self.context().current_consumer_cursors.as_mut().unwrap();
        let dependent_barrier = Arc::new(UniConsumerBarrier::new(consumer_cursors.remove(0)));

        UPBuilder {
            state: PhantomData,
            context: self.context,
            producer_barrier: self.producer_barrier,
            dependent_barrier,
        }
    }

    /// Add an event handler.
    pub fn handle_events_with<EH>(mut self, event_handler: EH) -> UPBuilder<MC, E, W, B>
    where
        EH: 'static + Send + FnMut(&E, Sequence, bool),
    {
        self.add_event_handler(event_handler);
        UPBuilder {
            state: PhantomData,
            context: self.context,
            producer_barrier: self.producer_barrier,
            dependent_barrier: self.dependent_barrier,
        }
    }

    /// Add an event handler with state.
    pub fn handle_events_and_state_with<EH, S, IS>(
        mut self,
        event_handler: EH,
        initalize_state: IS,
    ) -> UPBuilder<MC, E, W, B>
    where
        EH: 'static + Send + FnMut(&mut S, &E, Sequence, bool),
        IS: 'static + Send + FnOnce() -> S,
    {
        self.add_event_handler_with_state(event_handler, initalize_state);
        UPBuilder {
            state: PhantomData,
            context: self.context,
            producer_barrier: self.producer_barrier,
            dependent_barrier: self.dependent_barrier,
        }
    }
}

impl<E, W, B> UPBuilder<MC, E, W, B>
where
    E: 'static + Send + Sync,
    W: 'static + WaitStrategy,
    B: 'static + Barrier,
{
    /// Get an EventPoller.
    pub fn event_poller(mut self) -> (EventPoller<E, B>, UPBuilder<MC, E, W, B>) {
        let event_poller = self.get_event_poller();

        (
            event_poller,
            UPBuilder {
                state: PhantomData,
                context: self.context,
                producer_barrier: self.producer_barrier,
                dependent_barrier: self.dependent_barrier,
            },
        )
    }

    /// Add an event handler.
    pub fn handle_events_with<EH>(mut self, event_handler: EH) -> UPBuilder<MC, E, W, B>
    where
        EH: 'static + Send + FnMut(&E, Sequence, bool),
    {
        self.add_event_handler(event_handler);
        self
    }

    /// Add an event handler with state.
    pub fn handle_events_and_state_with<EH, S, IS>(
        mut self,
        event_handler: EH,
        initialize_state: IS,
    ) -> UPBuilder<MC, E, W, B>
    where
        EH: 'static + Send + FnMut(&mut S, &E, Sequence, bool),
        IS: 'static + Send + FnOnce() -> S,
    {
        self.add_event_handler_with_state(event_handler, initialize_state);
        self
    }

    /// Completes the current consumer group and starts a new pipeline stage.
    ///
    /// Consumers added after `.and_then()` will only see events after all consumers in the
    /// previous group have finished reading them. This is how you build sequential (diamond)
    /// consumer topologies.
    pub fn and_then(mut self) -> UPBuilder<NC, E, W, MultiConsumerBarrier> {
        let consumer_cursors = self
            .context()
            .current_consumer_cursors
            .replace(vec![])
            .unwrap();
        let dependent_barrier = Arc::new(MultiConsumerBarrier::new(consumer_cursors));

        UPBuilder {
            dependent_barrier,
            state: PhantomData,
            context: self.context,
            producer_barrier: self.producer_barrier,
        }
    }

    /// Finish the build and get a [`UniProducer`].
    pub fn build(mut self) -> UniProducer<E, MultiConsumerBarrier> {
        let consumer_cursors = self.context().current_consumer_cursors.take().unwrap();
        let consumer_barrier = MultiConsumerBarrier::new(consumer_cursors);
        UniProducer::new(
            self.context.shutdown_at_sequence,
            self.context.ring_buffer,
            self.producer_barrier,
            self.context.consumer_handles,
            consumer_barrier,
        )
    }
}
