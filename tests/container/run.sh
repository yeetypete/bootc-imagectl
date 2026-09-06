#!/bin/sh
# Cargo runs the container test binary through this script, see the justfile.
set -eu
tests=$1
shift
imagectl=$(dirname "$tests")/../bootc-imagectl
images=$(dirname "$0")/../images

# Run the tests in the image named $1 with the remaining arguments. The tests
# of the other images live in modules named after them and are skipped.
run_tests() {
    name=$1
    shift
    for other in "$images"/*/; do
        other=$(basename "$other")
        [ "$other" = "$name" ] || set -- "$@" --skip "$other::"
    done
    echo "running the tests in localhost/bootc-imagectl-test:$name" >&2
    podman run --rm --network=none \
        --volume "$tests:/usr/libexec/bootc-imagectl-test:ro" \
        --volume "$imagectl:/usr/libexec/bootc-imagectl:ro" \
        "localhost/bootc-imagectl-test:$name" /usr/libexec/bootc-imagectl-test "$@"
}

for dir in "$images"/*/; do
    image=$(basename "$dir")
    podman build --tag "localhost/bootc-imagectl-test:$image" "$dir"
    run_tests "$image" "$@"
done
