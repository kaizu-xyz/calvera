# By default just list all available commands
[private]
default:
    @just -l

# Lints the code
lint: clippy fmt-check doc-check

# Formats the code with nightly cargo
fmt:
    cargo +nightly fmt

# Checks that the code is formatted
fmt-check:
    cargo +nightly fmt -- --check

# Checks that docs emit no warnings
doc-check:
    RUSTDOCFLAGS="-D warnings" cargo doc --document-private-items --no-deps

# Checks clippy lints
clippy:
    cargo clippy --no-deps -- -D warnings

# Checks compilation
check:
    cargo check

alias b := build

# Builds in release mode
build:
    cargo build --release

alias t := test

# Runs the tests
test *FLAGS:
    cargo test {{FLAGS}}

# Runs criterion benchmarks
bench *FLAGS:
    cargo bench {{FLAGS}}

# Runs loom concurrency verification tests
loom *FLAGS:
    RUSTFLAGS="--cfg loom" cargo test --test loom_tests --release {{FLAGS}}

# Runs TLA+ formal verification (requires tlc: brew install tlaplus)
tla:
    cd verification && tlc Disruptor.tla
    cd verification && tlc MPMC.tla
