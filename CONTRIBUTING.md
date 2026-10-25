# Contributing

## Setup

Tasks run through [`just`](https://github.com/casey/just):

```bash
just check                   # Formatting and clippy.
just test                    # Unit tests.
just test-container          # Container tests.
just test-vm                 # VM tests.
just test-install            # Install tests.
just test-image ubuntu       # Every suite against one image.
just example ubuntu-desktop  # Install an example and boot it in a QEMU window.
```

`bootc-imagectl` uses [prek](https://github.com/j178/prek) (or `pre-commit`)
hooks for linting and formatting. Install them with `prek install`.

## Tests

`bootc-imagectl` has three test suites, which run against a real image of each
distribution in [`tests/images`](tests/images). The install tests also run
against the images in [`examples`](examples), tagged `<name>-example`:

| Suite | Command | Runs in | Host requirements |
| --- | --- | --- | --- |
| `tests/container` | `just test-container` | A container of each finalized image | Podman |
| `tests/vm` | `just test-vm` | A VM booted from each image | Podman, [bcvk](https://github.com/bootc-dev/bcvk), access to `/dev/kvm` |
| `tests/install` | `just test-install` | A VM booted from each image installed onto a disk | Podman, bcvk, systemd-vmspawn 259 or newer, systemd-journal-remote, virtiofsd, access to `/dev/kvm` and `/dev/vhost-vsock` |

> [!NOTE]
> Access to `/dev/kvm` and `/dev/vhost-vsock` usually comes with membership in
> the `kvm` group.

The `just test-*` recipes run `cargo xtask test <suite>`. This command builds
`bootc-imagectl` from the working tree, builds each image with it, then runs
the suite's test binary in the image. Each image in `tests/images` is also
built as a `-homed` variant without a regular user. `--image <tag>` limits the
run to one image, e.g. `just test-vm --image fedora-homed`. Other arguments
after the suite go to the test binary, for example a test name filter.

Images are built with Podman. `--build-option` (or
`BOOTC_IMAGECTL_BUILD_OPTIONS`) passes an option to `podman build`, such as a
cache. `{image}` in an option is automatically replaced by the name of the
image being built, e.g. `fedora`.

The install tests format the root with ext4. Set
`BOOTC_IMAGECTL_TEST_FILESYSTEM=btrfs` to install with btrfs instead.

When an install test fails, the logs of the booted system are in
`target/debug/install`, which contains `console.log`, `vmspawn.log` and
`vm.journal`. The journal can be read with
`journalctl --file=target/debug/install/vm.journal`.

### Writing tests

Each suite has its own test harness, built on
[libtest-mimic](https://docs.rs/libtest-mimic). Each module lists its tests
in `TESTS`. A test that only applies to some images declares a
`Requirement`, such as a distribution or a `Feature`, and is ignored for the
others. See [`src/testing.rs`](src/testing.rs).

## Adding a distribution

A distribution needs:

- A backend in [`src/distro`](src/distro) that implements the `Distro` trait,
  selected by its os-release `ID` in [`src/distro.rs`](src/distro.rs).
  Derivatives that list it in `ID_LIKE` in `/etc/os-release` may also use it.
- A test image in `tests/images/<name>`, with its sysusers lock file. `.j2`
  files are templates rendered with `homed` set for the homed variant.
- Its accounts and the groups of the homed wizard's user in
  [`src/testing.rs`](src/testing.rs). Checks specific to it go in
  `tests/container/<name>.rs` and require its distribution.

## Commits

Commit messages should follow [Conventional Commits](https://www.conventionalcommits.org/).
