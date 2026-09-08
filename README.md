# DevCore OS

DevCore OS is a Fedora bootc-based developer operating system for x86-64 UEFI
machines. The project keeps Fedora's kernel, drivers, Secure-Boot-compatible
boot chain and deployment engine while providing native DevCore desktop,
Studio, onboarding, system services, and installer experiences in Rust.

The active installer is not Anaconda. It is a native Slint live-desktop client
backed by root-owned `org.devcore.Installer1`. It accepts only an eligible
stable `/dev/disk/by-id` disk, requires an exact disk-bound erase phrase,
installs from a SHA-256-verified BaseOS archive embedded on the ISO, and uses
`bootc install to-filesystem` after preparing GPT, a 1 GiB EFI system
partition, and ext4 root. The live desktop stays available during installation.

```bash
platform/ci.sh
```

See [current implementation truth](docs/CURRENT_STATE.md),
[build/release gates](docs/BUILD_AND_RELEASE.md), and the
[UI reference mapping](docs/UI_REFERENCE_MAPPING.md). The native installer is
source-tested, but a live ISO, disposable install, installed-system boot, and
pixel comparison still need runtime evidence before release.
