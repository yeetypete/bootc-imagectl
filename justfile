# bootc-imagectl developer tasks.

# List available recipes.
default:
    @just --list

# Check formatting and lint.
check:
    cargo fmt --check
    cargo clippy --all-targets --all-features --locked

# Run the tests.
test:
    cargo test --locked

# Run the end-to-end tests.
test-e2e *args:
    CARGO_TARGET_{{ uppercase(arch()) }}_UNKNOWN_LINUX_GNU_RUNNER=tests/e2e/run.sh \
        cargo test --locked --features e2e --test e2e -- {{ args }}

# Build a release binary.
build:
    cargo build --release --locked
