# DevCore Security Model

## Trust boundaries

- Fedora bootc provides the trusted operating-system foundation.
- Secure Boot compatibility is preserved where practical.
- SELinux remains enforcing outside explicitly documented disposable tests.
- PAM and systemd-logind own password verification, session creation, seats,
  and device lifecycle.
- `greetd` starts the DevCore greeter but does not replace PAM.
- The root-owned `org.devcore.FirstBoot1` service is the only component that
  applies first-boot account and system settings; the `devcore` greeter may
  call its validated methods but cannot own the bus name.
- Privileged resource, update, and hardware operations are system services with
  narrow D-Bus and authorization boundaries.
- The root-owned `org.devcore.Installer1` service is the sole destructive
  installation boundary. Only the `devcore-live` session may send to it; it
  rechecks a stable disk ID immediately before erasure and rejects installer
  media, removable, virtual, running-root, and under-32-GiB devices.
- User development environments run rootless through Podman and OCI images.

## Data protection

- Do not store plaintext passwords.
- Do not implement custom password hashing or authentication.
- Do not embed credentials, tokens, API keys, browser profiles, or signing keys
  in source, OCI images, QCOW2 images, or ISO artifacts.
- Chrome and Codex use user-initiated first-run setup and their own credentials.
- Third-party graphical applications use Flatpak where available.
- Post-install software selection is explicit and user-scoped. The provisioning
  service checks NetworkManager readiness, validates a reviewed catalog,
  requires consent, applies profile/free-space admission, and runs only fixed
  direct argv through the bounded executor. It never accepts or stores a Wi-Fi
  password, token, shell fragment, or arbitrary package-manager executable.
- Provisioning progress is an atomic 0600 state file containing bundle IDs and
  bounded status only. A restarted service can resume completed steps, while
  offline requests wait for the user to reconnect Wi-Fi and press Start again.
- First-boot configuration excludes passwords. The completion file is a
  private atomic 0600 record under `/var/lib/devcore`; password bytes travel
  only through the transient D-Bus request and the `chpasswd` stdin pipe, never
  through command argv or persistent logs.
- Installer passwords travel once over a D-Bus UNIX FD into `chpasswd` stdin.
  They never appear in status, progress, diagnostics, or the target first-boot
  record. Cancellation is accepted only before destructive storage begins.

## Supply chain

- Base OCI images are pinned by digest in `platform/image/base-images.lock`.
- The live installer embeds the exact digest-pinned BaseOS OCI archive and its
  SHA-256 manifest. It verifies and loads that archive locally; the active
  path uses neither Anaconda/Kickstart nor `registry:localhost/...`.
- Rust source uses a locked Cargo dependency graph.
- Release inputs, artifacts, checksums, SBOMs, and test reports are retained.
- Signing happens only in a dedicated release environment.
- Updates are image based and must retain a bootable rollback deployment.

## Current limitations

`devcored` now has a long-running, read-only D-Bus snapshot service packaged
with a fixed low-privilege identity. Its resource budgets are advisory to the
user-session Work service: validated Build/Test jobs are wrapped in direct
`systemd-run --user --scope` argv with provisional CPU and memory properties.
The source verifier exercises the wrapper through a disposable fixture, but
live cgroup accounting, task suspension, thermal controls, and privileged
system-wide policy enforcement remain unverified or unimplemented.
The Environment, Build, and Test components currently produce validated plans;
the bounded execution/coordinator primitive is now exposed through the
user-session `devcore-workd` service for typed requests. It remains rootless and
does not expose arbitrary shell strings or privileged cgroup mutation. The
read-only Hardware1 service inventories kernel interfaces without device
mutation authority. Update1 validates digest-pinned targets and exposes
bounded status/planning only; apply and rollback remain behind a future
authorization and reboot-coordination boundary. The native installer is
source-validated, but its system-bus authorization, deployment/failure
recovery, and installed-system boot still require disposable-VM evidence. The
native greeter and
nested compositor are source-tested, and the feature-gated native DRM path is
compile-tested in an isolated container. PAM/logind, seat acquisition, and
live guest-session integration remain unverified until runtime tests are
complete. First-boot has source-level validation, packaging, and a disposable
session-bus status check, but its root-level apply path is intentionally not
run on the host and still needs guest evidence. Studio workspace file operations
are also constrained to the
registered environment root, reject traversal and symlink escapes, cap text
payloads, and publish saves through atomic replacement. Studio Git status, diff,
and short-history actions are read-only direct-argv operations in the same
rootless, digest-pinned environment boundary; repository mutation commands are
not exposed by that interface.

The post-install service and catalog are source-tested and D-Bus-smoke-tested,
but real Flatpak/OCI downloads, first-login Wi-Fi behavior, application
launches, and hardware-specific resource measurements remain guest evidence
gates. The Flathub Chrome and Android Studio entries are community-provided
sources and remain opt-in.
