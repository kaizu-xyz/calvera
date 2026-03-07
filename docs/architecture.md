# Architecture

Calvera is a Rust implementation of the [LMAX Disruptor](https://lmax-exchange.github.io/disruptor/) —
a lock-free, bounded ring buffer for low-latency inter-thread communication.

## Core idea

Instead of passing messages through a channel (which allocates, copies, and
synchronizes), threads share a **pre-allocated ring buffer** and coordinate
via **atomic sequence numbers**. No locks, no allocations on the hot path.

```
Producer ──publish──▶ RingBuffer ──consume──▶ Consumer(s)
              │                                    │
         (write slot)                        (read slot)
              │                                    │
         ProducerBarrier ◀─── sequence ───▶ ConsumerBarrier
```

## Data flow

1. **Producer claims a slot** — increments its sequence counter (uni: local
   increment, multi: CAS on shared cursor).
2. **Producer writes** — gets a `*mut E` to the slot via `ring_buffer.get()`,
   calls the user's update closure.
3. **Producer publishes** — advances the producer barrier (uni: Release store,
   multi: XOR on availability bitfield).
4. **Consumer waits** — spins on the producer barrier until the target sequence
   is available, then issues an Acquire fence.
5. **Consumer reads** — gets a `*const E` via `ring_buffer.get_ref()`, calls
   the user's handler.
6. **Consumer signals progress** — advances its cursor so the producer knows
   the slot can be reused.

## Module map

```
src/
├── lib.rs                    Crate root, public re-exports
├── sync.rs                   Centralized sync primitives (std / loom)
├── ring_buffer.rs            Pre-allocated ring buffer with UnsafeCell slots
├── disruptor.rs              Compile-time capacity builder (PowerOfTwo)
├── errors.rs                 Error types (ERingBufferFull, EMissingFreeSlots, EPolling)
│
├── _periphery/
│   ├── barrier.rs            Barrier trait (sequence visibility contract)
│   ├── cursor.rs             CachePadded atomic cursor
│   ├── wait_strategies.rs    BusySpin, BusySpinWithSpinLoopHint
│   └── affinity.rs           CPU core pinning via core_affinity
│
├── producer/
│   ├── mod.rs                Producer + ProducerBarrier traits, MutBatchIter
│   ├── uni.rs                UniProducer — single-producer (no CAS)
│   └── multi.rs              MultiProducer — multi-producer (CAS + bitfield)
│
├── consumer/
│   ├── mod.rs                Consumer barriers (Uni/Multi), ConsumerHandle
│   ├── managed.rs            Push-based consumers (disruptor-owned threads)
│   └── unmanaged.rs          Pull-based consumers (EventPoller)
│
└── builder/
    ├── mod.rs                Typestate builder, BuilderContext, ProcessorSettings
    ├── uni.rs                UPBuilder (single-producer)
    └── multi.rs              MPBuilder (multi-producer)
```

## Ring buffer

- Fixed-size, power-of-two slot count.
- Slot indexing is a bitmask: `sequence & (size - 1)` — no modulo division.
- Slots use `UnsafeCell` for interior mutability. `unsafe impl Sync` is sound
  because the sequence protocol guarantees exclusive write / shared read at
  any given time.
- Pre-allocated via a factory closure — no per-event heap allocation.

## Sequence protocol

All coordination happens through monotonically increasing `i64` sequence
numbers. The sentinel `-1` means "nothing yet".

```
Producer cursor:  ──0──1──2──3──4──5──▶  (highest published)
Consumer cursor:  ──0──1──2──3──▶        (highest consumed)
                                 ^^^^
                                 available to read (4, 5)
                           ^^^^^^^^^^^^
                           safe to overwrite (slots behind consumer)
```

**Backpressure:** If the producer catches up to the consumer (wraps around the
ring buffer), it must wait. `free_slots()` computes available space:

```
free_slots = consumer_sequence - (producer_sequence - buffer_size)
```

**Consumer barrier caching:** The producer caches the slowest consumer's
position in `sequence_clear_of_consumers`. It only re-checks the real consumer
barrier when the cache says there's no room — avoiding an atomic load on most
publishes.

## Producer variants

### UniProducer (single-producer)

- Only one thread publishes — no contention on the sequence counter.
- Sequence claiming is a local `+= 1` — zero atomics.
- Publishing is a single `Release` store to the producer barrier cursor.
- Fastest variant. Use this unless you truly need multiple publishing threads.

### MultiProducer (multi-producer)

- Multiple threads publish concurrently via `Clone`.
- Sequence claiming uses `compare_exchange` (CAS) on a shared cursor.
- **Out-of-order publishing problem:** Producer A claims seq 5, producer B
  claims seq 6. B might finish writing first. If the barrier just tracked
  the cursor, consumers would see seq 6 before seq 5 is written.
- **Solution: availability bitfield.** Each slot has a bit in an `AtomicU64`
  array. Publishing XORs the bit (encoding even/odd round parity).
  `get_after` scans bits to find the highest *contiguous* published sequence.
- The `Mutex` in `SharedProducerCtx` is only for Clone/Drop (control plane).
  The hot path is fully lock-free.

## Consumer models

### Managed (push-based)

The disruptor owns the consumer thread. You give it a closure:

```rust
builder.handle_events_with(|event, sequence, end_of_batch| { ... })
```

The thread runs a loop: wait → batch process → advance cursor → repeat.
`end_of_batch` is `true` on the last event of each batch, useful for
amortizing I/O (e.g., flush after a batch).

Stateful variant: `handle_events_and_state_with` takes a `FnOnce() -> S`
that creates state on the consumer thread — `S` doesn't need to be `Send`.

### Unmanaged (pull-based)

You own the thread and poll when ready:

```rust
let (mut poller, builder) = builder.event_poller();
// ... later, on your thread:
match poller.poll() {
    Ok(mut events) => { for e in &mut events { /* ... */ } }
    Err(EPolling::NoEvents) => { /* do other work */ }
    Err(EPolling::Shutdown) => { /* exit */ }
}
```

Returns an `EventGuard` — an RAII iterator. Dropping it advances the cursor.

## Consumer topologies

Consumers in the same group read events **concurrently** (in parallel).
`.and_then()` creates a **sequential** dependency — downstream consumers
only see events after all upstream consumers finish:

```
              ┌─ consumer A ─┐
Producer ─────┤              ├── .and_then() ──▶ consumer C
              └─ consumer B ─┘
```

A reads concurrently with B. C waits for both A and B.

- **1 consumer in group** → `UniConsumerBarrier` (single atomic load)
- **N consumers in group** → `MultiConsumerBarrier` (min of N atomic loads)

## Builder typestate

The builder uses phantom type parameters (`NC`, `SC`, `MC`) to enforce at
compile time that you can't call `.build()` without at least one consumer:

```
NC (no consumers)  →  .handle_events_with()  →  SC (single consumer)
SC                 →  .handle_events_with()  →  MC (multiple consumers)
SC / MC            →  .build()               →  Producer
```

`.build()` is not available on `NC`. This is a zero-cost compile-time check.

## Memory ordering

| Operation | Ordering | Why |
|-----------|----------|-----|
| Producer publish (uni) | `Release` store | Ensures all slot writes are visible before the sequence is published |
| Producer publish (multi, single) | `Release` fetch_xor | Same, but on the availability bitfield |
| Producer publish (multi, batch) | `Release` fence + `Relaxed` fetch_xor | Fence covers all slots, then relaxed XOR per slot |
| Consumer wait | `Acquire` fence after barrier load | Ensures consumer sees all data the producer wrote |
| Cursor store (consumer progress) | `Release` store | Ensures the producer sees the consumer is done |
| Consumer barrier read | `Relaxed` load | Producer reads consumer progress — Acquire fence is on the consumer side |
| Shutdown sentinel | `Relaxed` load/store | Only needs to be eventually visible |

## Verification

Calvera has three layers of verification:

1. **Unit tests** (`cargo test`) — functional correctness with realistic ring
   buffer sizes. SPSC, MPSC, SPMC, MPMC, batch, backpressure, pipeline,
   event pollers, stress test.

2. **Loom tests** (`just loom`) — exhaustive interleaving exploration for
   memory ordering correctness. Tiny state spaces (2-4 slots, 2-3 threads)
   because loom's state space grows exponentially. See
   [postmortem](postmortem-unsafecell-access-model.md) for a bug loom caught.

3. **TLA+ specs** (`just tla`) — formal verification of the algorithm's safety
   properties at a higher level of abstraction. See [verification/](../verification/).

## Performance

- **No allocations** on the hot path — everything is pre-allocated.
- **No locks** — all coordination via atomics and CAS.
- **Cache-friendly** — contiguous memory, `CachePadded` cursors prevent false
  sharing.
- **Consumer barrier caching** — producer skips atomic loads when it knows
  there's room.
- **Bitmask indexing** — `sequence & mask` instead of `sequence % size`.
- **Core pinning** — optional CPU affinity via `pin_at_core()` for
  deterministic latency. See [linux-core-isolation.md](linux-core-isolation.md).
