#!/bin/bash
# Install a bootc image built with bootc-imagectl onto a disk:
#
#     curl -fsSL https://github.com/yeetypete/bootc-imagectl/raw/main/install.sh \
#         | sudo bash -s -- --image docker.io/example/image:latest /dev/nvme0n1
set -euo pipefail

usage() {
    echo "Usage: install.sh --image IMAGE [INSTALL-OPTION]... DEVICE" >&2
    echo "Other options are forwarded to \`bootc-imagectl install\`." >&2
    echo "See \`bootc-imagectl install --help\`." >&2
}

fatal() {
    echo "install.sh: $*" >&2
    exit 1
}

main() {
    local image=
    local args=()
    while [[ $# -gt 0 ]]; do
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
    if [[ -z ${image} || ${#args[@]} -eq 0 ]]; then
        usage
        echo >&2
        lsblk --nodeps --output NAME,SIZE,MODEL >&2
        exit 2
    fi

    { : </dev/tty; } 2>/dev/null || fatal "needs a terminal for interactive prompts"

    if ! command -v podman >/dev/null; then
        echo "Installing podman"
        if command -v apt-get >/dev/null; then
            apt-get update
            DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ca-certificates podman
        elif command -v dnf >/dev/null; then
            dnf install -y podman
        elif command -v pacman >/dev/null; then
            pacman -Syu --noconfirm --needed podman
        else
            fatal "podman is not installed, and there is no apt-get, dnf or pacman to install it"
        fi
    fi

    exec podman run --rm --interactive --tty \
        --privileged --pid=host --ipc=host \
        --security-opt label=type:unconfined_t \
        --volume /dev:/dev --volume /run/udev:/run/udev:ro \
        --volume /var/lib/containers:/var/lib/containers \
        "${image}" /usr/libexec/bootc-imagectl install "${args[@]}" </dev/tty
}

main "$@"
