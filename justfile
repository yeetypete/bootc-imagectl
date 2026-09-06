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

# Run the container tests.
test-container *args:
    CARGO_TARGET_{{ uppercase(arch()) }}_UNKNOWN_LINUX_GNU_RUNNER=tests/container/run.sh \
        cargo test --locked --features container --test container -- {{ args }}

# Run the VM tests.
test-vm *args:
    CARGO_TARGET_{{ uppercase(arch()) }}_UNKNOWN_LINUX_GNU_RUNNER=tests/vm/run.sh \
        cargo test --locked --features vm --test vm -- {{ args }}

# Build a release binary.
build:
    cargo build --release --locked
