//! Strategies for waiting when no new events are available on the ring buffer.
//!
//! When a consumer polls the producer barrier and finds no new events, it
//! must decide what to do before retrying. That decision is the wait strategy.
//!
//! This is the classic latency vs CPU trade-off:
//!
//! | Strategy                     | Latency    | CPU usage |
//! |------------------------------|------------|-----------|
//! | [`BusySpin`]                 | Lowest     | 100% core |
//! | [`BusySpinWithSpinLoopHint`] | Slightly higher | Lower (allows HT / power saving) |
//!
//! For latency-critical systems (trading, real-time processing), use [`BusySpin`]
//! on isolated cores. For everything else, [`BusySpinWithSpinLoopHint`] is a
//! reasonable default.

use std::hint;

use crate::Sequence;

/// Strategy that a managed consumer thread uses to wait when no new events
/// are available.
///
/// Called in the consumer's spin loop between `barrier.get_after()` calls.
/// The strategy receives the sequence number the consumer is waiting for,
/// though current implementations don't use it (it's there for future
/// strategies that might back off based on how long they've been waiting).
///
/// Requires `Copy + Send` because each consumer thread gets its own copy
/// of the strategy.
pub trait WaitStrategy: Copy + Send {
    /// Called once per spin iteration while the consumer waits for `sequence`
    /// to become available.
    fn wait_for(&self, sequence: Sequence);
}

/// True busy spin — the consumer retries immediately without yielding.
///
/// Achieves the lowest possible latency because the consumer thread never
/// voluntarily gives up the CPU. The trade-off is that it burns 100% of
/// a core while waiting.
///
/// Best used on dedicated, isolated cores (see `docs/linux-core-isolation.md`)
/// where no other work competes for the core.
#[derive(Copy, Clone)]
pub struct BusySpin;

impl WaitStrategy for BusySpin {
    #[inline]
    fn wait_for(&self, _sequence: Sequence) {
        // Do nothing, true busy spin.
    }
}

/// Busy spin with a [`hint::spin_loop`] call on each iteration.
///
/// On x86 this emits a `PAUSE` instruction, which:
/// - Tells the CPU this is a spin-wait loop, avoiding memory order violations
///   that would otherwise cause a pipeline flush on loop exit.
/// - On hyper-threaded cores, yields execution resources to the sibling thread.
/// - May reduce power consumption.
///
/// The cost is slightly higher latency per iteration compared to [`BusySpin`],
/// but with meaningfully lower power draw and better behavior on shared cores.
#[derive(Copy, Clone)]
pub struct BusySpinWithSpinLoopHint;

impl WaitStrategy for BusySpinWithSpinLoopHint {
    fn wait_for(&self, _sequence: Sequence) {
        hint::spin_loop();
    }
}
