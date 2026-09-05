# bootc-imagectl developer tasks.

# List available recipes.
default:
    @just --list

# Check formatting and lint.
check:
    cargo fmt --check
    cargo clippy --all-targets --locked

# Run the tests.
test:
    cargo test --locked

# Build a release binary.
build:
    cargo build --release --locked
