# Calvera

A low-latency, lock-free inter-thread communication library for Rust,
based on the [LMAX Disruptor](https://github.com/LMAX-Exchange/disruptor) pattern.

Use it when `std::sync::mpsc` or Crossbeam channels aren't fast enough and you
need sub-microsecond publish-to-consume latency. Calvera trades CPU resources
for lower latency and higher throughput — no locks, no allocations on the hot path.

## Contents

- [Quick start](#quick-start)
- [Features](#features)
- [Consumer models](#consumer-models)
- [Patterns](#patterns)
- [Design choices](#design-choices)
- [Correctness](#correctness)
- [Performance](#performance)
- [Documentation](#documentation)

## Quick start

Add to your `Cargo.toml`:

```toml
calvera = "0.1.0"
```

### Single and batch publication

```rust
use calvera::*;

struct Event {
    price: f64,
}

fn main() {
    let factory = || Event { price: 0.0 };

    let processor = |e: &Event, sequence: Sequence, end_of_batch: bool| {
        // Process each event as it arrives.
        // `end_of_batch` is true on the last event of a batch — useful for
        // amortizing I/O (e.g., flush after a batch).
    };

    let mut producer = build_uni_producer_unchecked(64, factory, BusySpin)
        .handle_events_with(processor)
        .build();

    // Single publish.
    for i in 0..10 {
        producer.publish(|e| { e.price = i as f64; });
    }

    // Batch publish — amortizes slot claiming overhead.
    producer.batch_publish(5, |iter| {
        for e in iter {
            e.price = 42.0;
        }
    });
    // Producer goes out of scope → consumers finish → disruptor drops.
}
```

### Compile-time capacity validation

```rust
use calvera::*;

// Compile error if N is not a power of two:
let builder = Disruptor::with_capacity::<64>();
```

### Multiple producers with pinned, interdependent consumers

```rust
use calvera::*;
use std::thread;

struct Event {
    price: f64,
}

fn main() {
    let factory = || Event { price: 0.0 };

    let h1 = |e: &Event, _: Sequence, _: bool| { /* ... */ };
    let h2 = |e: &Event, _: Sequence, _: bool| { /* ... */ };
    let h3 = |e: &Event, _: Sequence, _: bool| { /* ... */ };

    let mut p1 = build_multi_producer_unchecked(64, factory, BusySpin)
        .pin_at_core(1).handle_events_with(h1)   // h1 and h2 read
        .pin_at_core(2).handle_events_with(h2)   // concurrently
            .and_then()
            .pin_at_core(3).handle_events_with(h3) // h3 waits for both
        .build();

    let mut p2 = p1.clone();

    thread::scope(|s| {
        s.spawn(move || {
            for i in 0..10 {
                p1.publish(|e| { e.price = i as f64; });
            }
        });
        s.spawn(move || {
            for i in 10..20 {
                p2.publish(|e| { e.price = i as f64; });
            }
        });
    });
}
```

### Processors with non-Send state

```rust
use std::{cell::RefCell, rc::Rc};
use calvera::*;

struct Event { price: f64 }

#[derive(Default)]
struct State { data: Rc<RefCell<i32>> }

fn main() {
    let factory = || Event { price: 0.0 };
    let initial_state = || State::default();

    let processor = |s: &mut State, e: &Event, _: Sequence, _: bool| {
        *s.data.borrow_mut() += 1;
    };

    let mut producer = build_uni_producer_unchecked(64, factory, BusySpin)
        .handle_events_and_state_with(processor, initial_state)
        .build();

    for i in 0..10 {
        producer.publish(|e| { e.price = i as f64; });
    }
}
```

### Event polling (pull-based)

```rust
use calvera::*;

struct Event { price: f64 }

fn main() {
    let factory = || Event { price: 0.0 };

    let builder = build_uni_producer_unchecked(64, factory, BusySpin);
    let (mut poller, builder) = builder.event_poller();
    let mut producer = builder.build();

    for i in 0..10 {
        producer.publish(|e| { e.price = i as f64; });
    }
    drop(producer);

    loop {
        match poller.poll() {
            Ok(mut events) => {
                for event in &mut events {
                    // Process event.
                }
            } // EventGuard dropped → signals progress to disruptor
            Err(EPolling::NoEvents) => { /* do other work or retry */ }
            Err(EPolling::Shutdown) => break,
        }
    }
}
```

## Features

- Single Producer Single Consumer (SPSC)
- Single Producer Multi Consumer (SPMC) with consumer interdependencies
- Multi Producer Single Consumer (MPSC)
- Multi Producer Multi Consumer (MPMC) with consumer interdependencies
- Busy-spin wait strategies (`BusySpin`, `BusySpinWithSpinLoopHint`)
- Batch publication and batch consumption
- Event Poller (pull-based) API
- Thread affinity (CPU core pinning) per consumer
- Custom thread names per consumer
- Compile-time ring buffer size validation
- Processors with non-`Send` state (initialized on consumer thread)

## Consumer models

There are two ways to consume events:

| Model | How | When to use |
|-------|-----|-------------|
| **Managed** (push) | `.handle_events_with(closure)` | Disruptor owns the thread, events pushed to your closure |
| **Unmanaged** (pull) | `.event_poller()` → `poller.poll()` | You own the thread, pull events when ready |

Both have comparable performance. Use what fits your architecture.

### Consumer topologies

Consumers added in the same group read events **concurrently**.
`.and_then()` creates a **sequential** stage:

```
              ┌─ consumer A ─┐
Producer ─────┤              ├── .and_then() ──▶ consumer C
              └─ consumer B ─┘
```

A and B read in parallel. C only sees events after both A and B finish.

## Patterns

### Enum event types

When multiple event types share a ring buffer:

```rust
enum Event {
    Login { id: i32 },
    Order { user_id: i32, price: f64, quantity: i32 },
    Heartbeat,
}

let processor = |e: &Event, _: Sequence, _: bool| {
    match e {
        Event::Login { id }                          => { /* ... */ }
        Event::Order { user_id, price, quantity }    => { /* ... */ }
        Event::Heartbeat                             => { /* ... */ }
    }
};
```

### Splitting workload across processors

Assign each processor an ID and only process events where `sequence % N == id`:

```rust
let processor0 = |e: &Event, seq: Sequence, _| {
    if seq % 2 == 0 { /* process */ }
};
let processor1 = |e: &Event, seq: Sequence, _| {
    if seq % 2 == 1 { /* process */ }
};
```

## Design choices

Everything is optimized for low latency:

- **Pre-allocated ring buffer** — events are created at startup via a factory
  closure. No per-event heap allocation. Memory is contiguous for cache locality.
- **Power-of-two sizing** — slot indexing is `sequence & mask` (bitmask), not
  `sequence % size` (modulo division).
- **No dynamic dispatch** — everything is monomorphized.
- **No locks on the hot path** — all coordination via atomics and CAS.
- **Consumer barrier caching** — producer skips atomic loads when it knows
  there's room, re-checking only when the cache says it's full.

You cannot allocate an event and *move* it into the ring buffer. Instead, the
producer gets a mutable reference to a pre-existing slot and writes in-place.
You can still move owned data into a field of the event.

## Correctness

This library uses `unsafe` to achieve low latency. These approaches are used to
eliminate bugs:

- Minimal `unsafe` blocks, each with a `// SAFETY` comment.
- High test coverage (unit tests, doc tests, compile-fail tests).
- **Loom** verification — exhaustive exploration of all thread interleavings
  and memory orderings (`just loom`).
- **TLA+** formal verification — algorithm-level safety proofs for SPMC and
  MPMC scenarios (`just tla`). See [`verification/`](verification/).
- All tests run under Miri.

## Performance

Benchmarks compare Calvera against Crossbeam channels. Run them with:

```bash
just bench
```

Results are written to `target/criterion/` with HTML reports.

The results below were gathered on an Apple M2 (macOS, no core isolation).
Latency is mean per element. Throughput is elements per second.

### Raw throughput (10M events)

| Scenario | Crossbeam | Calvera | Speedup |
|---------:|----------:|--------:|--------:|
| SPSC     | 207 ms    | 19 ms   | **10.9x** |
| MPSC     | 363 ms    | 46 ms   | **7.9x**  |

### SPSC latency per element (no pause between bursts)

| Burst size | Crossbeam | Calvera | Speedup |
|-----------:|----------:|--------:|--------:|
|          1 |     78 ns |   15 ns | **5.1x** |
|         10 |     45 ns |  4.7 ns | **9.6x** |
|        100 |     17 ns |  2.0 ns | **8.9x** |

### MPSC latency per element (no pause between bursts)

| Burst size | Crossbeam | Calvera | Speedup |
|-----------:|----------:|--------:|--------:|
|          1 |    282 ns |  242 ns | **1.2x** |
|         10 |    111 ns |   35 ns | **3.2x** |
|        100 |     46 ns |   11 ns | **4.3x** |

### Polling vs managed consumers (SPSC, no pause)

| Burst size | Managed | Polling |
|-----------:|--------:|--------:|
|          1 |   16 ns |   18 ns |
|         10 |   49 ns |   50 ns |
|        100 |  192 ns |  196 ns |

Polling and managed consumers have comparable performance.

### Key takeaways

- **Batch publication is critical** — going from burst 1 → 100 improves SPSC
  per-element latency from 15 ns to 2 ns (7.5x).
- Calvera's advantage grows with burst size because it amortizes the cost of
  slot claiming across the batch.
- MPSC with burst size 1 shows the smallest gap (1.2x) because the CAS
  contention dominates — but at burst 10+ the bitfield scheme pulls ahead.
- Calvera's latency is **resilient to pauses** between bursts (consistent
  across 0 ms, 1 ms, and 10 ms pauses), while Crossbeam shows more variance.

For stable results on Linux, see [core isolation guide](docs/linux-core-isolation.md).

## Documentation

- [Architecture](docs/architecture.md) — module map, data flow, memory ordering
- [Linux core isolation](docs/linux-core-isolation.md) — `isolcpus`, `nohz_full`, SCHED_FIFO
- [TLA+ verification](verification/README.md) — formal model checking
- [UnsafeCell post-mortem](docs/postmortem-unsafecell-access-model.md) — a bug loom caught

## Acknowledgements

Inspired by the [LMAX Disruptor](https://github.com/LMAX-Exchange/disruptor) by
LMAX Exchange and the [disruptor-rs](https://github.com/nicholassm/disruptor-rs)
Rust implementation by Nicholas Simons Mikkelsen.
