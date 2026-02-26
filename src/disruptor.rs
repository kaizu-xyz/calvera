use crate::{
    MultiProducerBarrier, UniProducerBarrier,
    builder::{NC, build_multi_producer_unchecked, build_uni_producer_unchecked, multi::MPBuilder, uni::UPBuilder},
    wait_strategies::WaitStrategy,
};

/// Compile-time checked disruptor constructor.
///
/// Ring buffers require a power-of-two size so that slot indexing reduces to a
/// bitmask (`sequence & (size - 1)`) instead of a modulo — a property the hot
/// path depends on for correctness, not just performance.
///
/// The free-standing [`build_uni_producer_unchecked`] / [`build_multi_producer_unchecked`]
/// functions skip this check entirely — the caller is responsible for passing a
/// valid size. That's useful for dynamic sizes or when the check has already
/// been done elsewhere, but in latency-critical services where we want to
/// eliminate *all* runtime unpredictability, we want the guarantee to be checked
/// during type-checking — before any code is generated or executed.
///
/// `Disruptor::with_capacity::<N>()` provides that stronger guarantee. The
/// `PowerOfTwo<N>: Verified` trait bound is only satisfiable for valid sizes,
/// so an invalid `N` is a **type error** caught by `cargo check` — no codegen,
/// no runtime panic, no surprises in production.
///
/// ```compile_fail
/// use calvera::{BusySpin, Disruptor};
/// // This fails at type-check time — 3 is not a power of two.
/// Disruptor::with_capacity::<3>().build_uni_producer(|| 0, BusySpin);
/// ```
///
/// ```
/// use calvera::{BusySpin, Disruptor};
/// // This compiles — 8 is a valid power of two.
/// Disruptor::with_capacity::<8>();
/// ```
pub struct Disruptor;

impl Disruptor {
    pub fn with_capacity<const N: usize>() -> DisruptorBuilder<N>
    where
        PowerOfTwo<N>: Verified,
    {
        DisruptorBuilder
    }
}

// ---------------------------------------------------------------------------
// Compile-time power-of-two enforcement
//
// We cannot use `N.is_power_of_two()` in a where clause on stable Rust because
// const generic expressions (`generic_const_exprs`) are nightly-only. Instead
// we use a sealed trait: `Verified` is only implemented for `PowerOfTwo<N>`
// where N is a known power of two. The macro enumerates every valid size up to
// 1M slots — more than enough for any practical ring buffer.
// ---------------------------------------------------------------------------

pub struct PowerOfTwo<const N: usize>;

pub trait Verified {}

macro_rules! impl_power_of_two {
    ($($n:expr),*) => {
        $(impl Verified for PowerOfTwo<$n> {})*
    };
}

impl_power_of_two!(
    1, 2, 4, 8, 16, 32, 64, 128, 256, 512,
    1024, 2048, 4096, 8192, 16384, 32768, 65536,
    131072, 262144, 524288, 1048576
);

pub struct DisruptorBuilder<const N: usize>;

impl<const N: usize> DisruptorBuilder<N>
where
    PowerOfTwo<N>: Verified,
{
    pub fn build_uni_producer<E, W, F>(
        self,
        event_factory: F,
        wait_strategy: W,
    ) -> UPBuilder<NC, E, W, UniProducerBarrier>
    where
        F: FnMut() -> E,
        E: 'static + Send + Sync,
        W: 'static + WaitStrategy,
    {
        build_uni_producer_unchecked(N, event_factory, wait_strategy)
    }

    pub fn build_multi_producer<E, W, F>(
        self,
        event_factory: F,
        wait_strategy: W,
    ) -> MPBuilder<NC, E, W, MultiProducerBarrier>
    where
        F: FnMut() -> E,
        E: 'static + Send + Sync,
        W: 'static + WaitStrategy,
    {
        build_multi_producer_unchecked(N, event_factory, wait_strategy)
    }
}
