//! Unmanaged (pull-based) consumer via [`EventPoller`].
//!
//! Unlike the managed consumer (where the disruptor owns the thread and pushes
//! events to your closure), the `EventPoller` lets you own the thread and pull
//! events when you're ready. This is useful when:
//!
//! - You need to integrate with an existing event loop or scheduler.
//! - You want to interleave polling with other work.
//! - You need non-`Send` state that can't be moved to a managed thread.
//!
//! # Usage
//!
//! ```no_run
//! # use calvera::*;
//! # let factory = || 0i64;
//! # let builder = build_uni_producer_unchecked(8, factory, BusySpin);
//! # let (mut poller, builder) = builder.event_poller();
//! # let mut producer = builder.build();
//! loop {
//!     match poller.poll() {
//!         Ok(mut events) => {
//!             for event in &mut events {
//!                 // process event
//!             }
//!         } // EventGuard dropped here → signals progress to the disruptor
//!         Err(EPolling::NoEvents) => { /* do other work or retry */ }
//!         Err(EPolling::Shutdown) => break,
//!     }
//! }
//! ```
//!
//! # How it works
//!
//! 1. [`EventPoller::poll`] (or [`poll_take`](EventPoller::poll_take)) checks the
//!    dependent barrier for available sequences.
//! 2. If events are available, it returns an [`EventGuard`] — a bounded, RAII
//!    iterator over the available events.
//! 3. When the `EventGuard` is dropped, it advances the consumer's cursor to
//!    signal that those slots can be reused. **Not all events need to be read** —
//!    dropping the guard early still advances the cursor to the highest available
//!    sequence, effectively skipping unread events.

use crate::sync::{
    Arc,
    atomic::{AtomicI64, Ordering, fence},
};

use crossbeam_utils::CachePadded;

use crate::{
    Sequence, barrier::Barrier, cursor::Cursor, errors::EPolling, ring_buffer::RingBuffer,
};

/// Pull-based event consumer.
///
/// Created via the builder's `.event_poller()` method. You own the `EventPoller`
/// and call [`poll`](Self::poll) or [`poll_take`](Self::poll_take) from any
/// thread at any time.
///
/// Internally holds:
/// - A reference to the shared ring buffer (read-only access via `get_ref`).
/// - The dependent barrier (producer or upstream consumer) to check for new events.
/// - Its own cursor, which it advances when an [`EventGuard`] is dropped.
/// - The shutdown sentinel to detect when the producer is done.
pub struct EventPoller<E, B> {
    ring_buffer: Arc<RingBuffer<E>>,
    dependent_barrier: Arc<B>,
    shutdown_at_sequence: Arc<CachePadded<AtomicI64>>,
    cursor: Arc<Cursor>,
}

/// RAII guard over a batch of available events.
///
/// Obtained from [`EventPoller::poll`] or [`EventPoller::poll_take`].
/// Iterate over events via `for event in &mut guard { ... }`.
///
/// **On drop**, the guard advances the consumer cursor to `available`,
/// signaling to producers (or downstream consumers) that these slots
/// can be reused. This happens even if not all events were read —
/// unread events are effectively skipped.
///
/// # Lifetime
///
/// The iterator is implemented on `&'g mut EventGuard` (not `EventGuard` directly)
/// so that returned `&E` references are tied to the borrow of the guard,
/// not to the `EventPoller`. This prevents holding event references past the
/// guard's drop — which would be unsound because the producer could overwrite
/// the slot once the cursor advances.
pub struct EventGuard<'p, E, B> {
    parent: &'p mut EventPoller<E, B>,
    sequence: Sequence,
    available: Sequence,
}

impl<'g, E, B> Iterator for &'g mut EventGuard<'_, E, B> {
    type Item = &'g E;

    fn next(&mut self) -> Option<Self::Item> {
        if self.sequence > self.available {
            return None;
        }

        // SAFETY: The guard is authorized to read up to and including `available`.
        // The Acquire fence in poll_take guarantees visibility of the producer's writes.
        let event_ptr = self.parent.ring_buffer.get_ref(self.sequence);
        let event = unsafe { &*event_ptr };
        self.sequence += 1;
        Some(event)
    }
}

impl<E, B> ExactSizeIterator for &mut EventGuard<'_, E, B> {
    /// Returns the number of remaining events available to read.
    fn len(&self) -> usize {
        (self.available - self.sequence + 1) as usize
    }
}

impl<E, B> Drop for EventGuard<'_, E, B> {
    fn drop(&mut self) {
        // Advance the cursor to `available` regardless of how many events were read.
        // This allows client code to skip events without stalling the disruptor.
        self.parent.cursor.store(self.available);
    }
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

    /// Polls the ring buffer and returns an [`EventGuard`] if any events are available.
    /// The guard can be used like an iterator and yields all available events at the time of polling.
    /// Dropping the guard will signal to the Disruptor that the events have been processed.
    ///
    /// This method does not block; it will return immediately.
    ///
    /// This method is equivalent to calling [`EventPoller::poll_take`] with `u64::MAX` as the limit.
    ///
    /// # Errors
    ///
    /// This method returns an error if:
    /// 1. No events are available: [`EPolling::NoEvents`]
    /// 2. The Disruptor is shut down: [`EPolling::Shutdown`]
    ///
    /// # Examples
    ///
    /// ```
    ///# use calvera::*;
    ///#
    ///# #[derive(Debug)]
    ///# struct Event {
    ///#     price: f64
    ///# }
    ///# let factory = || Event { price: 0.0 };
    ///# let builder = build_uni_producer_unchecked(8, factory, BusySpin);
    ///# let (mut event_poller, builder) = builder.event_poller();
    ///# let mut producer = builder.build();
    ///# producer.publish(|e| { e.price = 42.0; });
    ///# drop(producer);
    /// match event_poller.poll() {
    ///     Ok(mut events) => {
    ///         for event in &mut events {
    ///             // ...
    ///         }
    ///     },
    ///     Err(EPolling::NoEvents) => { /* ... */ },
    ///     Err(EPolling::Shutdown) => { /* ... */ },
    /// };
    /// ```
    pub fn poll(&mut self) -> Result<EventGuard<'_, E, B>, EPolling> {
        self.poll_take(u64::MAX)
    }

    /// Polls for available events, yielding at most `limit` events.
    ///
    /// This method behaves like [`EventPoller::poll`], but caps the number of events yielded by the returned
    /// [`EventGuard`]. Fewer events may be yielded if less are available at the time of polling.
    ///
    /// Note: A `limit` of zero returns an [`EventGuard`] that yields no events (and not an [EPolling::NoEvents] error).
    ///
    /// # Examples
    ///
    /// ```
    ///# use calvera::*;
    ///#
    ///# #[derive(Debug)]
    ///# struct Event {
    ///#     price: f64
    ///# }
    ///# let factory = || Event { price: 0.0 };
    ///# let builder = build_uni_producer_unchecked(8, factory, BusySpin);
    ///# let (mut event_poller, builder) = builder.event_poller();
    ///# let mut producer = builder.build();
    ///# producer.publish(|e| { e.price = 42.0; });
    ///# drop(producer);
    /// match event_poller.poll_take(64) {
    ///     Ok(mut events) => {
    ///         // Process events same as above but yielding at most 64 events.
    ///         for event in &mut events {
    ///             // ...
    ///         }
    ///     },
    ///     Err(EPolling::NoEvents) => { /* ... */ },
    ///     Err(EPolling::Shutdown) => { /* ... */ },
    /// };
    /// ```
    pub fn poll_take(&mut self, limit: u64) -> Result<EventGuard<'_, E, B>, EPolling> {
        let cursor_at = self.cursor.relaxed_value();
        let sequence = cursor_at + 1;

        if sequence == self.shutdown_at_sequence.load(Ordering::Relaxed) {
            return Err(EPolling::Shutdown);
        }

        let available = self.dependent_barrier.get_after(sequence);
        if available < sequence {
            return Err(EPolling::NoEvents);
        }
        fence(Ordering::Acquire);

        let max_sequence = (cursor_at).saturating_add_unsigned(limit);
        let available = std::cmp::min(available, max_sequence);

        Ok(EventGuard {
            parent: self,
            sequence,
            available,
        })
    }
}
