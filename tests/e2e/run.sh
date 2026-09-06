#!/bin/sh
# Cargo runs the e2e test binary through this script, see the justfile. Build
# the image in each directory here and run the tests inside a container of it.
set -eu
tests=$1
shift
imagectl=$(dirname "$tests")/../bootc-imagectl
here=$(dirname "$0")

# Run the tests in the image named $1 with the remaining arguments. The tests
# of the other images live in modules named after them and are skipped.
run_tests() {
    name=$1
    shift
    for other in "$here"/*/; do
        other=$(basename "$other")
        [ "$other" = "$name" ] || set -- "$@" --skip "$other::"
    done
    echo "running the tests in localhost/bootc-imagectl-e2e:$name" >&2
    podman run --rm --network=none \
        --volume "$tests:/usr/libexec/bootc-imagectl-e2e:ro" \
        --volume "$imagectl:/usr/libexec/bootc-imagectl:ro" \
        "localhost/bootc-imagectl-e2e:$name" /usr/libexec/bootc-imagectl-e2e "$@"
}

for dir in "$here"/*/; do
    image=$(basename "$dir")
    podman build --tag "localhost/bootc-imagectl-e2e:$image" "$dir"
    run_tests "$image" "$@"
done
