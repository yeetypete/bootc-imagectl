# bootc-imagectl developer tasks.

# List available recipes.
default:
    @just --list

# Check formatting and lint.
check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --all-features --locked

# Run the tests.
test:
    cargo test --locked

# Run the container tests.
test-container *args:
    cargo xtask test container {{ args }}

# Run the VM tests.
test-vm *args:
    cargo xtask test vm {{ args }}

# Build a release binary.
build:
    cargo build --release --locked
