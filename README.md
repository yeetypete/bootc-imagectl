# bootc-imagectl

`bootc-imagectl` makes it easy to build a [bootc](https://bootc-dev.github.io/bootc/)
image from a regular distribution container image, and install it onto a disk.

It runs as the last step of a container build, and turns the root filesystem
built during a Podman build into an image that boots with bootc's
[composefs backend](https://bootc.dev/bootc/bootc-experimental-composefs.7.html).

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

## Assumptions

`bootc-imagectl` makes some opinionated assumptions about how a bootc image
is built:

- The image is built with Podman.
- The image uses bootc's composefs backend and boots with systemd-boot.
- The image boots from a [UKI](https://bootc.dev/bootc/bootc-experimental-composefs.7.html#building-sealed-images).
- Every user and group has a fixed UID and GID from a sysusers lock file.

> [!NOTE]
> Derived images, built `FROM` an image that `bootc-imagectl finalize` already
> processed, are not yet supported.

## Building an image

See [`tests/images/fedora/Containerfile.j2`](tests/images/fedora/Containerfile.j2)
for a complete example. [`tests/images`](tests/images) has one for each
supported distribution.

On a first build, `finalize` will fail and print the lines the sysusers lock
file must contain. Add them to `00-bootc-imagectl.lock.conf`, commit it next to
the Containerfile, and rebuild. Later builds will fail in the same manner whenever
a package adds a user or group not fixed in the lock file. This is your signal to
add the new user or group to the lock file and rebuild.

## Secure Boot

To sign the image for Secure Boot, pass a key as a build secret and its
certificate to `finalize` and `bootc container ukify` (see the
[example](tests/images/fedora/Containerfile.j2)).
`finalize` signs systemd-boot and stages the certificate for systemd-boot to
enroll. This needs systemd 257 or newer.

To enroll the key, put the firmware in setup mode, then pick
*Enroll Secure Boot keys: auto* in the boot menu. This replaces the firmware's
keys, including Microsoft's.

## Installing

From a live system, e.g. a USB stick with one of the supported Linux distributions
booted, install the image onto a disk with:

```bash
curl -fsSL https://github.com/yeetypete/bootc-imagectl/raw/main/install.sh \
    | sudo bash -s -- --image docker.io/example/my-bootc-image:latest
```

This calls `bootc-imagectl install` with the image reference, which lists the
disks and asks which one to install to. Running `bootc upgrade` in the installed
system will update it based on the image reference it was installed from.

The root filesystem is ext4 by default. Pass `--filesystem btrfs` to use btrfs
instead. The image must then ship `mkfs.btrfs`, e.g. from `btrfs-progs`.

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
