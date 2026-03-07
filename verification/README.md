# Verification in TLA+

This folder contains TLA+ specifications that formally verify the disruptor's
safety properties (no data races, correct sequencing) by exhaustively exploring
all possible interleavings of writers and readers.

There are two models:

- **`Disruptor.tla`** — Single-Producer, Multiple-Consumer (SPMC)
- **`MPMC.tla`** — Multiple-Producer, Multiple-Consumer (MPMC)

This is complementary to the [loom](https://crates.io/crates/loom) tests in
`tests/loom_tests.rs`, which verify memory ordering correctness at the Rust
implementation level. The TLA+ specs verify the algorithm's correctness at a
higher level of abstraction.

## Prerequisites

You need the TLC model checker, which requires Java.

```bash
# macOS
brew install tlaplus

# Or use the TLA+ Toolbox GUI / VS Code TLA+ extension
```

## Running

### Command line

```bash
just tla
```

### TLA+ Toolbox / VS Code

Open the `.tla` file and configure the model with the values below, then run
the model checker.

## Model configurations

These configurations complete in under a minute:

**SPMC (`Disruptor.tla`):**

- MaxPublished <- 10
- Size <- 8
- Writers <- { "w" }
- Readers <- { "r1", "r2" }
- NULL <- [ model value ]

**MPMC (`MPMC.tla`):**

- MaxPublished <- 10
- Size <- 8
- Writers <- { "w1", "w2" }
- Readers <- { "r1", "r2" }
- NULL <- [ model value ]
