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

# Boot a test image in a VM.
vm image *args:
    cargo xtask vm {{ args }} {{ image }}

# Run the install tests.
test-install *args:
    cargo xtask test install {{ args }}

# Run every suite against one image.
test-image image *args:
    cargo xtask test-image {{ args }} {{ image }}

# Install an example image and boot it in a VM with a graphical console.
example name *args:
    cargo xtask example {{ args }} {{ name }}

# Build a release binary.
build:
    cargo build --release --locked
