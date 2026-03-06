use crate::Sequence;

/// As in none, not zero. This is used to indicate that a producer has not yet produced any items.
pub const NONE: Sequence = -1;

/// A barrier tracks how far a producer or consumer has progressed.
///
/// Consumers read the producer barrier to know which sequences are available.
/// Producers read the consumer barrier to know which slots are safe to rewrite.
///
/// Requires `Send + Sync` because barrier implementations (e.g. `UniProducerBarrier`,
/// `MultiProducerBarrier`, `UniConsumerBarrier`, `MultiConsumerBarrier`) are shared
/// across threads via `Arc`. The producer thread writes to the barrier, consumer
/// threads read from it — all concurrently. Without `Send`, the barrier couldn't
/// be moved into a spawned thread. Without `Sync`, multiple threads couldn't hold
/// `&self` references simultaneously.
///
/// All implementations use atomics internally (no locks on the hot path), so
/// `Sync` is safe — concurrent `get_after` calls are lock-free loads.
pub trait Barrier: Send + Sync {
    /// Gets the sequence number of the barrier with relaxed memory ordering.
    ///
    /// `prev` must be the last sequence returned from this barrier (or the
    /// initial sequence the caller wants to read from). Most implementations
    /// ignore it, but `MultiProducerBarrier` uses it as the starting scan
    /// position in the availability bitfield — passing a wrong value would
    /// skip or re-scan slots.
    ///
    /// Returns the highest contiguous available sequence starting from `prev`,
    /// or [`NONE`] (-1) if nothing has been published yet.
    ///
    /// Note: to establish proper happens-before relationships, the caller must
    /// issue a [`std::sync::atomic::fence`] with [`Ordering::Acquire`] after
    /// this returns.
    fn get_after(&self, prev: Sequence) -> Sequence;
}
