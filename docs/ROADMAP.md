# DevCore Roadmap

| Milestone | Outcome | Status |
| --- | --- | --- |
| Foundation | Feature-owned Rust source and Fedora bootc BaseOS contract | In progress |
| Desktop | Shell, compositor, greeter, Studio and service integration | In progress |
| Native installer | Offline payload, Installer1, live boot entries, guided UEFI/GPT/ext4 flow | Source complete; VM gate open |
| Live/installable alpha | ISO, live desktop, disposable installation and installed-boot evidence | ISO assembled; boot/install gates in progress |
| Release | GHCR channels, signing, checksums, Secure Boot/physical evidence | Not started |

Source compilation is never release evidence. The native installer milestone
closes only after a disposable UEFI/KVM install and reboot test, injected
failure/retry test, and reference-viewport UI review.
