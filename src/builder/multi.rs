//! Module for structs for building a Multi Producer Disruptor in a type safe way.
//!
//! To get started building a Multi Producer Disruptor, invoke [super::build_multi_producer_unchecked].

use std::marker::PhantomData;

use crate::sync::Arc;

use crate::{
    Sequence,
    barrier::Barrier,
    builder::ProcessorSettings,
    consumer::{MultiConsumerBarrier, UniConsumerBarrier, unmanaged::EventPoller},
    producer::multi::{MultiProducer, MultiProducerBarrier},
    wait_strategies::WaitStrategy,
};

use super::{Builder, BuilderContext, MC, NC, SC};

/// First step in building a Disruptor with a [MultiProducer].
pub struct MPBuilder<State, E, W, B> {
    state: PhantomData<State>,
    context: BuilderContext<E, W>,
    producer_barrier: Arc<MultiProducerBarrier>,
    dependent_barrier: Arc<B>,
}

impl<S, E, W, B> ProcessorSettings<E, W> for MPBuilder<S, E, W, B> {
    fn context(&mut self) -> &mut BuilderContext<E, W> {
        &mut self.context
    }
}

impl<S, E, W, B> Builder<E, W, B> for MPBuilder<S, E, W, B>
where
    E: 'static + Send + Sync,
    W: 'static + WaitStrategy,
    B: 'static + Barrier,
{
    fn dependent_barrier(&self) -> Arc<B> {
        Arc::clone(&self.dependent_barrier)
    }
}

impl<E, W, B> MPBuilder<NC, E, W, B>
where
    E: 'static + Send + Sync,
    W: 'static + WaitStrategy,
    B: 'static + Barrier,
{
    pub(super) fn new<F>(
        size: usize,
        event_factory: F,
        wait_strategy: W,
        producer_barrier: Arc<MultiProducerBarrier>,
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
    pub fn event_poller(mut self) -> (EventPoller<E, B>, MPBuilder<SC, E, W, B>) {
        let event_poller = self.get_event_poller();

        (
            event_poller,
            MPBuilder {
                state: PhantomData,
                context: self.context,
                producer_barrier: self.producer_barrier,
                dependent_barrier: self.dependent_barrier,
            },
        )
    }

    /// Add an event handler.
    pub fn handle_events_with<EH>(mut self, event_handler: EH) -> MPBuilder<SC, E, W, B>
    where
        EH: 'static + Send + FnMut(&E, Sequence, bool),
    {
        self.add_event_handler(event_handler);
        MPBuilder {
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
    ) -> MPBuilder<SC, E, W, B>
    where
        EH: 'static + Send + FnMut(&mut S, &E, Sequence, bool),
        IS: 'static + Send + FnOnce() -> S,
    {
        self.add_event_handler_with_state(event_handler, initialize_state);
        MPBuilder {
            state: PhantomData,
            context: self.context,
            producer_barrier: self.producer_barrier,
            dependent_barrier: self.dependent_barrier,
        }
    }
}

impl<E, W, B> MPBuilder<SC, E, W, B>
where
    E: 'static + Send + Sync,
    W: 'static + WaitStrategy,
    B: 'static + Barrier,
{
    /// Get an EventPoller.
    pub fn event_poller(mut self) -> (EventPoller<E, B>, MPBuilder<MC, E, W, B>) {
        let event_poller = self.get_event_poller();

        (
            event_poller,
            MPBuilder {
                state: PhantomData,
                context: self.context,
                producer_barrier: self.producer_barrier,
                dependent_barrier: self.dependent_barrier,
            },
        )
    }

    /// Add an event handler.
    pub fn handle_events_with<EH>(mut self, event_handler: EH) -> MPBuilder<MC, E, W, B>
    where
        EH: 'static + Send + FnMut(&E, Sequence, bool),
    {
        self.add_event_handler(event_handler);
        MPBuilder {
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
    ) -> MPBuilder<MC, E, W, B>
    where
        EH: 'static + Send + FnMut(&mut S, &E, Sequence, bool),
        IS: 'static + Send + FnOnce() -> S,
    {
        self.add_event_handler_with_state(event_handler, initialize_state);
        MPBuilder {
            state: PhantomData,
            context: self.context,
            producer_barrier: self.producer_barrier,
            dependent_barrier: self.dependent_barrier,
        }
    }

    /// Complete the (concurrent) consumption of events so far and let new consumers process
    /// events after all previous consumers have read them.
    pub fn and_then(mut self) -> MPBuilder<NC, E, W, UniConsumerBarrier> {
        // Guaranteed to be present by construction.
        let consumer_cursors = self.context().current_consumer_cursors.as_mut().unwrap();
        let dependent_barrier = Arc::new(UniConsumerBarrier::new(consumer_cursors.remove(0)));

        MPBuilder {
            state: PhantomData,
            context: self.context,
            producer_barrier: self.producer_barrier,
            dependent_barrier,
        }
    }

    /// Finish the build and get a [`MultiProducer`].
    pub fn build(mut self) -> MultiProducer<E, UniConsumerBarrier> {
        let mut consumer_cursors = self.context().current_consumer_cursors.take().unwrap();
        // Guaranteed to be present by construction.
        let consumer_barrier = UniConsumerBarrier::new(consumer_cursors.remove(0));
        MultiProducer::new(
            self.context.shutdown_at_sequence,
            self.context.ring_buffer,
            self.producer_barrier,
            self.context.consumer_handles,
            consumer_barrier,
        )
    }
}

impl<E, W, B> MPBuilder<MC, E, W, B>
where
    E: 'static + Send + Sync,
    W: 'static + WaitStrategy,
    B: 'static + Barrier,
{
    /// Get an EventPoller.
    pub fn event_poller(mut self) -> (EventPoller<E, B>, MPBuilder<MC, E, W, B>) {
        let event_poller = self.get_event_poller();

        (
            event_poller,
            MPBuilder {
                state: PhantomData,
                context: self.context,
                producer_barrier: self.producer_barrier,
                dependent_barrier: self.dependent_barrier,
            },
        )
    }

    /// Add an event handler.
    pub fn handle_events_with<EH>(mut self, event_handler: EH) -> MPBuilder<MC, E, W, B>
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
    ) -> MPBuilder<MC, E, W, B>
    where
        EH: 'static + Send + FnMut(&mut S, &E, Sequence, bool),
        IS: 'static + Send + FnOnce() -> S,
    {
        self.add_event_handler_with_state(event_handler, initialize_state);
        self
    }

    /// Complete the (concurrent) consumption of events so far and let new consumers process
    /// events after all previous consumers have read them.
    pub fn and_then(mut self) -> MPBuilder<NC, E, W, MultiConsumerBarrier> {
        let consumer_cursors = self
            .context()
            .current_consumer_cursors
            .replace(vec![])
            .unwrap();
        let dependent_barrier = Arc::new(MultiConsumerBarrier::new(consumer_cursors));

        MPBuilder {
            state: PhantomData,
            context: self.context,
            producer_barrier: self.producer_barrier,
            dependent_barrier,
        }
    }

    /// Finish the build and get a [`MultiProducer`].
    pub fn build(mut self) -> MultiProducer<E, MultiConsumerBarrier> {
        let consumer_cursors = self.context().current_consumer_cursors.take().unwrap();
        let consumer_barrier = MultiConsumerBarrier::new(consumer_cursors);
        MultiProducer::new(
            self.context.shutdown_at_sequence,
            self.context.ring_buffer,
            self.producer_barrier,
            self.context.consumer_handles,
            consumer_barrier,
        )
    }
}
