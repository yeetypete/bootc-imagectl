#!/bin/sh
# Cargo runs the VM test binary through this script, see the justfile.
set -eu
tests=$1
shift
images=$(dirname "$0")/../images

# Run the tests against the image named $1 with the remaining arguments. The
# tests of the other images live in modules named after them and are skipped.
run_tests() {
    name=$1
    shift
    for other in "$images"/*/; do
        other=$(basename "$other")
        [ "$other" = "$name" ] || set -- "$@" --skip "$other::"
    done
    echo "running the tests against $name" >&2
    BOOTC_IMAGECTL_TEST_IMAGE=$name "$tests" "$@"
}

for dir in "$images"/*/; do
    run_tests "$(basename "$dir")" "$@"
done
