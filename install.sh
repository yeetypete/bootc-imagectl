#!/bin/bash
# Install a bootc image built with bootc-imagectl onto a disk:
#
#     curl -fsSL https://github.com/yeetypete/bootc-imagectl/raw/main/install.sh \
#         | sudo bash -s -- --image docker.io/example/image:latest
set -euo pipefail

readonly STORAGE=/var/lib/containers

usage() {
    echo "Usage: install.sh --image IMAGE [INSTALL-OPTION]... [DEVICE]" >&2
    echo "Other options are forwarded to \`bootc-imagectl install\`." >&2
    echo "See \`bootc-imagectl install --help\`." >&2
}

fatal() {
    echo "install.sh: $*" >&2
    exit 1
}

# WORKAROUND: podman cannot store images on the overlayfs root of live systems, so store the
# image in memory instead.
prepare_storage() {
    mkdir -p "${STORAGE}"
    [[ $(findmnt --noheadings --output FSTYPE --target "${STORAGE}") == overlay ]] || return 0
    echo "Storing the image in memory. If it does not fit, mount a disk at ${STORAGE} and run this again."
    mount -t tmpfs -o size=80%,mode=0700 tmpfs "${STORAGE}"
}

main() {
    local image=
    local args=()
    while (($#)); do
        case "$1" in
        --image)
            [[ $# -ge 2 ]] || fatal "--image needs a value"
            image="$2"
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            args+=("$1")
            shift
            ;;
        esac
    done

    [[ ${EUID} -eq 0 ]] || fatal "must run as root"
    if [[ -z ${image} ]]; then
        usage
        exit 2
    fi

    { : </dev/tty; } 2>/dev/null || fatal "needs a terminal for interactive prompts"

    # Debian and Ubuntu ship podman's storage configuration in containers-storage,
    # which podman only recommends.
    if ! command -v podman >/dev/null ||
        [[ ! -e /usr/share/containers/storage.conf && ! -e /etc/containers/storage.conf ]]; then
        echo "Installing podman"
        if command -v apt-get >/dev/null; then
            apt-get update
            DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
                ca-certificates containers-storage podman
        elif command -v dnf >/dev/null; then
            dnf install -y podman
        elif command -v pacman >/dev/null; then
            pacman -Syu --noconfirm --needed podman
        else
            fatal "cannot install podman without apt-get, dnf or pacman"
        fi
    fi

    prepare_storage

    # `udevadm wait` needs udev's netlink events from the host network.
    # Installing from the registry avoids skopeo's temporary layer copies, which
    # are in memory on live systems.
    exec podman run --rm --interactive --tty \
        --privileged --pid=host --ipc=host --network=host \
        --security-opt label=type:unconfined_t \
        --volume /dev:/dev --volume /run/udev:/run/udev:ro \
        --volume "${STORAGE}:${STORAGE}" \
        "${image}" /usr/libexec/bootc-imagectl install \
        --source-imgref "docker://${image}" "${args[@]}" </dev/tty
}

main "$@"
