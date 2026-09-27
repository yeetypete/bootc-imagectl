# bootc-imagectl

`bootc-imagectl` makes it easy to build a [bootc](https://bootc-dev.github.io/bootc/)
image from a regular distribution container image, and install it onto a disk.

It runs as the last step of a container build, and turns the root filesystem
built during a Docker/Podman build into an image that boots with bootc's
[composefs backend](https://bootc.dev/bootc/experimental-composefs.html).

Supported Linux distributions:

- Arch Linux
- Debian and its derivatives, e.g. Ubuntu
- Fedora

## What it does

`bootc-imagectl finalize` is meant to run at the end of an image build. It
turns the root filesystem into a ready-to-use bootc image. It keeps the
image's users and groups stable across rebuilds and upgrades, and strips
unwanted machine-specific state from the image. See
[Building an image](#building-an-image) for usage details, and
[Users and groups](docs/src/users-and-groups.md) for how accounts are handled.

`bootc-imagectl install` installs the image onto a disk, with an encrypted
root by default. This is meant to be run on a live system, e.g. from a USB
stick with a live Linux distribution booted. See [Installing](#installing)
for usage details.

## Building an image

```dockerfile
FROM docker.io/library/rust:1 AS bootc-imagectl

RUN cargo install --locked \
    --git https://github.com/yeetypete/bootc-imagectl bootc-imagectl

FROM registry.fedoraproject.org/fedora:44

# Create all users and groups with a fixed UID and GID set by the lock file.
RUN dnf install -y systemd
COPY 00-bootc-imagectl.lock.conf /usr/lib/sysusers.d/
RUN systemd-sysusers

RUN dnf install -y bootc dracut kernel openssh-server

COPY --from=bootc-imagectl /usr/local/cargo/bin/bootc-imagectl /usr/libexec/bootc-imagectl
RUN /usr/libexec/bootc-imagectl finalize \
    --sysusers-lock /usr/lib/sysusers.d/00-bootc-imagectl.lock.conf
```

On the first build, `finalize` will fail and print the lines the sysusers lock
file must contain. Add them to `00-bootc-imagectl.lock.conf`, commit it next to
the Containerfile, and rebuild. Later builds will fail in the same manner whenever
a package adds a user or group not fixed in the lock file. This is your signal to
add the new user or group to the lock file and rebuild.

[`tests/images`](tests/images) contains complete, tested images for each supported
distribution. They may be used as a reference for your own bootc image builds.

## Installing

From a live system, e.g. a USB stick with one of the supported Linux distributions
booted, install the image onto a disk with:

```bash
curl -fsSL https://github.com/yeetypete/bootc-imagectl/raw/main/install.sh \
    | sudo bash -s -- --image docker.io/example/my-bootc-image:latest /dev/my-disk
```

This calls `bootc-imagectl install` with the image reference and the disk to
install to. Running `bootc upgrade` in the installed system will update it based
on the image reference it was installed from.

## First boot

On first boot, systemd-firstboot will ask for settings the image leaves
unset, such as the locale, the time zone and the root password. To boot
unattended instead, set them with `firstboot.*` credentials (see
[systemd-firstboot](https://www.freedesktop.org/software/systemd/man/latest/systemd-firstboot.html)),
or disable the prompts in the image with a kernel argument:

```toml
# /usr/lib/bootc/kargs.d/10-firstboot.toml
kargs = ["systemd.firstboot=no"]
```

To unlock the encrypted root with the TPM in addition to a passphrase (only
applies to encrypted installs), enroll it once the system is
installed:

```bash
sudo systemd-cryptenroll --tpm2-device=auto /dev/my-disk
```

## Updates

The installed system updates transactionally with `bootc upgrade`, which
pulls a newer image and stages it as a new deployment you can roll back to if
needed. See the [`bootc` upgrade docs](https://bootc-dev.github.io/bootc/upgrades.html).

## License

`bootc-imagectl` is released under the [MIT License](LICENSE).
