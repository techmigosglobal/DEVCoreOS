# Build and Release

## Prerequisites

- Rust 1.90 (`rust-toolchain.toml`)
- Podman
- Fedora Image Builder CLI/container and its layers (needed later for ISO)
- QEMU/KVM plus x86-64 UEFI firmware (needed later for VM release evidence)

No large download is required for source validation. Before ISO assembly,
install Image Builder in an isolated Fedora builder and make room for the
BaseOS image, its embedded OCI archive, and a disposable VM disk. The native
build scripts stop with a clear error if those prerequisites are absent.

## Source checks

```bash
platform/ci.sh
```

## BaseOS and native installer image

Resolve and publish a digest-pinned BaseOS first. The installer builder only
accepts this immutable reference; it rejects a `registry:localhost` source.

```bash
export DEVCORE_BASE_IMAGE=ghcr.io/techmigosglobal/devcoreos@sha256:<64-hex-digest>
platform/image/build-oci.sh
platform/image/build-native-installer-oci.sh
```

The second command saves the exact BaseOS image into the installer image as an
OCI archive and writes its SHA-256 manifest. It does not download during an
installation.

## Native live ISO and VM evidence

Every push to `main` runs the `build_iso` CI job after source validation. The
job publishes a commit-specific BaseOS image, builds the offline native
installer, verifies that exactly one non-empty ISO was produced, writes its
SHA-256 checksum, and uploads both files as the
`devcoreos-native-installer-<commit>` Actions artifact. The workflow can also
be started manually with **Run workflow**.

Run this only in a privileged, isolated image-builder environment:

```bash
sudo DEVCORE_INSTALLER_IMAGE_REF=ghcr.io/techmigosglobal/devcoreos-installer:alpha \
  platform/image/build-native-installer-iso.sh
```

Use a blank disposable virtio disk, never a host block device. Release
evidence must show: Try boot reaches the desktop; Install boot opens the
native Installer; terminal/network remain usable during deployment; the
selected 32+ GiB virtual disk receives GPT + 1 GiB ESP + ext4 root; the
installed disk boots; and the configured account reaches DevCore login.

## Publication

Release tags run the GitHub workflow and publish only `alpha`, `beta`, or
`stable` channel images at `ghcr.io/techmigosglobal/devcoreos`. The workflow
records the pushed immutable digest in release metadata; later update planning
uses that published digest rather than an installer-local mutable tag.
