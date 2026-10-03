# Contributing

## Setup

Tasks run through [`just`](https://github.com/casey/just):

```bash
just check           # Formatting and clippy.
just test            # Unit tests.
just test-container  # Container tests.
just test-vm         # VM tests.
just test-install    # Install tests.
```

`bootc-imagectl` uses [prek](https://github.com/j178/prek) (or `pre-commit`)
hooks for linting and formatting. Install them with `prek install`.

## Tests

`bootc-imagectl` has three test suites, which run against a real image of each
distribution in [`tests/images`](tests/images):

| Suite | Command | Runs in | Host requirements |
| --- | --- | --- | --- |
| `tests/container` | `just test-container` | A container of each finalized image | Podman |
| `tests/vm` | `just test-vm` | A VM booted from each image | Podman, [bcvk](https://github.com/bootc-dev/bcvk), access to `/dev/kvm` |
| `tests/install` | `just test-install` | A VM booted from each image installed onto a disk | Podman, bcvk, systemd-vmspawn 259 or newer, systemd-journal-remote, virtiofsd, access to `/dev/kvm` and `/dev/vhost-vsock` |

> [!NOTE]
> Access to `/dev/kvm` and `/dev/vhost-vsock` usually comes with membership in
> the `kvm` group.
>
> On Ubuntu, bcvk also needs the AppArmor profile of `bwrap` to let the
> programs it runs keep their capabilities, as set up in
> [the CI workflow](.github/workflows/ci.yaml).

The `just test-*` recipes run `cargo xtask test <suite>`. This command builds
`bootc-imagectl` from the working tree, builds each image with it, then runs
the suite's test binary in the image. Arguments after the suite go to the
test binary, for example a test name filter.

Images are built with Podman by default. `--builder docker` (or
`BOOTC_IMAGECTL_BUILDER=docker`) builds them with Docker Buildx instead.
`--build-option` (or `BOOTC_IMAGECTL_BUILD_OPTIONS`) passes an option to
the builder, such as a cache. `{image}` in an option is automatically
replaced by the name of the image being built, e.g. `fedora`.

The install tests format the root with ext4. Set
`BOOTC_IMAGECTL_TEST_FILESYSTEM=btrfs` to install with btrfs instead.

When an install test fails, the logs of the booted system are in
`target/debug/install`, which contains `console.log`, `vmspawn.log` and
`journal`. The journal can be read with
`journalctl --directory=target/debug/install/journal`.

### Writing tests

A test for one image goes in the suite's module named after the image, such
as `tests/vm/fedora.rs`. A check that applies to every image should share
the same test name in each module and call a shared function.

## Adding a distribution

A distribution needs:

- A backend in [`src/distro`](src/distro) that implements the `Distro` trait,
  selected by its os-release `ID` in [`src/distro.rs`](src/distro.rs).
  Derivatives that list it in `ID_LIKE` in `/etc/os-release` may also use it.
- A test image in `tests/images/<name>`, with its sysusers lock file.
- A module per test suite in `tests/*/<name>.rs`.

## Commits

Commit messages should follow [Conventional Commits](https://www.conventionalcommits.org/).
