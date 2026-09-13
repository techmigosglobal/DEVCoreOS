# DevCore Current State

## Implemented source slice

- Feature-owned Rust workspace under `features/`, shared primitives under
  `shared/`, platform packaging under `platform/`, and stable D-Bus contracts
  under `contracts/dbus/`.
- Native Slint shell, Studio surface, greeter/first-boot flow, compositor,
  resource/update/hardware/provision services, and the new Installer app.
- Root-owned `org.devcore.Installer1` service with stable `/dev/disk/by-id`
  inventory, disk exclusion/minimum-capacity preflight, disk-bound erase
  confirmation, lifecycle state, cancellation boundary, and progress signals.
- Offline installer packaging: a digest-pinned GHCR BaseOS is saved as an OCI
  archive inside the live image, SHA-256 verified locally, loaded locally, and
  deployed to the prepared external filesystem through `bootc install
  to-filesystem`. No installer path uses Anaconda, Kickstart, or a localhost
  registry fetch.
- x86-64 UEFI live-media configuration with **Try DevCore OS** and **Install
  DevCore OS** entries. Both enter the `devcore-live` desktop; the Install
  entry opens the native installer while the desktop remains usable.

## Validation and artifact evidence

`cargo test --workspace` passes, including installer domain/state tests and
the shell's live-install boot-entry guard. The native installer, privileged
daemon, and static image contract compile and have shell/XML/JSON validation.

GitHub Actions run `34706001864` built the native installer image with the
digest-pinned Fedora 43 base, assembled a non-empty 2.6 GiB x86-64 ISO, wrote
its SHA-256 checksum, and uploaded both as artifact
`devcoreos-native-installer-e7c822a2e3e5fa039106c5702f2909bf2ed1d153`.
The CI workflow now also requires a disposable UEFI/KVM smoke boot to reach
Linux and systemd before future ISO artifacts are uploaded.

## Release gates still open

ISO assembly is confirmed in GitHub Actions. A complete interactive target
installation and installed-system boot have not yet been demonstrated, so
these claims remain deliberately unmade:

- live desktop graphical boot;
- installer D-Bus service under a real system bus and polkit session;
- destructive install to a disposable virtio disk;
- booting the installed target and authenticating the configured account;
- screenshot comparison at 1672×941 against `uiuxreferences/`;
- Secure Boot and physical-hardware certification.

The previous Anaconda assets and diagnostic ISO are retained as non-active
rollback/evidence material until the native disposable-VM installation gate
passes. They are not used by the active `platform/` build path.
