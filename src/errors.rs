use thiserror::Error;

/// Error indicating that the ring buffer is full.
///
/// Client code can then take appropriate action, e.g. discard data or even panic as this indicates
/// that the consumers cannot keep up - i.e. latency.
#[derive(Debug, Error, PartialEq)]
#[error("Ring Buffer is full.")]
pub struct ERingBufferFull;

/// The Ring Buffer was missing a number of free slots for doing the batch publication.
#[derive(Debug, Error, PartialEq)]
#[error("Missing free slots in Ring Buffer: {0}")]
pub struct EMissingFreeSlots(pub u64);

/// Error types that can occur if polling is unsuccessful.
#[derive(Debug, Error, PartialEq)]
pub enum EPolling {
    /// Indicates that there are no events available to process.
    #[error("No available events.")]
    NoEvents,
    /// Indicates that the Disruptor has been shut down.
    #[error("Disruptor is shut down.")]
    Shutdown,
}
