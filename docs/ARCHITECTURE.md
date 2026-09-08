# DevCore Architecture

## Foundation

DevCore uses Fedora bootc for the kernel, firmware, Mesa, systemd, PAM,
systemd-logind, networking, audio, storage, virtualization, boot chain, and
drivers. It is not a Fedora desktop remix: the DevCore desktop, service layer,
developer workflows, and management interfaces are DevCore-owned.

## Runtime boundaries

```text
Fedora bootc image
  ├── systemd, PAM, systemd-logind, SELinux, NetworkManager, PipeWire
  ├── greetd → native compositor → first-boot setup or DevCore greeter
  ├── DevCore Smithay compositor → DevCore Slint shell and applications
  ├── devcored system service → cgroup/PSI/hardware policy
  ├── devcore-hardwared system service → read-only hardware inventory
  └── user session services
       ├── Environment Manager → rootless Podman OCI environments
       ├── Provision Manager → explicit Wi-Fi-aware optional software setup
       ├── Build Engine → existing build adapters and content-addressed cache
       ├── Test Engine → resource-aware test scheduling
       └── DevCore Studio → Workspace, LSP, DAP, PTY, Git, build/test D-Bus clients
```

## IPC

DevCore services expose versioned D-Bus contracts. Privileged operations stay
behind narrowly scoped system-bus methods; UI applications do not manipulate
cgroups, thermal controls, accounts, or updates directly.

Initial interfaces and their current status:

- `org.devcore.Resource1` — implemented first boundary for snapshots and
  explicit refresh; automatic pressure/workload events and policy controls are
  still staged. The shell consumes the explicit refresh path, subscribes to
  `SnapshotChanged`, and keeps a local kernel-collection fallback when the
  service is unavailable.
- `org.devcore.Hardware1` — implemented read-only CPU, memory, power, network,
  and primary DRM inventory with explicit refresh; the shell consumes a bounded
  overview readout, while firmware, driver mutation, and health remediation
  remain incomplete.
- `org.devcore.Environment1` — implemented in the on-demand user-session
  `devcore-workd` service for digest-pinned definition and rootless lifecycle
  request submission; the service still requires a logged-in session and
  Podman for live execution.
- `devcore-execution` — implemented bounded direct-argv supervisor and
  filesystem result/log primitive; the user-session Work service now owns
  typed Build/Test authorization and OCI-plan mapping around it.
- `greetd` IPC — implemented session protocol boundary, with the greeter,
  native compositor, and shell launch contract staged for graphical login.
  PAM conversation, logind/seat startup, and live native-seat launch still need
  guest runtime evidence. The nested Smithay backend remains available for
  protocol/rendering development.
- `org.devcore.FirstBoot1` — implemented root-owned setup boundary with strict
  locale, keyboard, timezone, account, hostname, and profile validation. It
  persists only a 0600 non-secret completion record after fixed system tools
  succeed; password input is transient and is sent to `chpasswd` through a
  pipe. The native greeter has a four-step setup screen and calls this service
  before login when the completion record is absent. Live root apply, PAM,
  logind, and guest-seat evidence remain pending.
- `org.devcore.Build1` — implemented initial typed adapter submission in
  `devcore-workd`; DAG breadth, artifact/cache records, and live OCI evidence
  remain incomplete.
- `org.devcore.Test1` — implemented initial typed test submission in
  `devcore-workd`; device/remote workers and result indexing remain incomplete.
- `org.devcore.Job1` — implemented status, cancellation, and bounded reads of
  private terminal stdout/stderr records for the user-session job coordinator.
- `org.devcore.Workspace1` — implemented bounded directory listing, UTF-8
  Studio file reads, and atomic saves for normalized paths below a registered
  environment workspace; traversal, absolute paths, non-files, and symlink
  escapes are rejected.
- `org.devcore.Git1` — implemented bounded read-only Git status, diff, and
  short-history submission through the rootless Work executor; mutation actions
  and repository indexing remain incomplete.
- `org.devcore.Update1` — implemented read-only bootc status plus validated
  digest-pinned channel/target planning; the shell displays the configured
  channel and bounded status preview, while applying, rollback authorization,
  reboot coordination, deployment history, and release artifact evidence
  remain incomplete.
- `org.devcore.Provision1` — implemented as an on-demand user-session
  catalog/plan/start/cancel boundary. It waits for NetworkManager readiness,
  requires explicit consent, admits bundles against the selected hardware
  profile and free-space estimate, and persists resumable 0600 state. The
  base image carries only the Flatpak runtime; Flutter/Android environments
  are digest-pinned OCI pulls, Android Studio and Chrome are user-scoped
  Flatpaks, and Codex remains an external-provider entry rather than an
  invented download source.

The first daemon slice is deliberately on-demand: it collects once at startup,
serves typed read-only properties, and emits `SnapshotChanged` after an
explicit `Refresh` call. The shell subscribes to that signal without polling;
kernel/systemd event sources that initiate future refreshes will be connected
before any periodic polling or enforcement logic is introduced.

Post-install provisioning follows the same on-demand rule. The build system
creates only a BaseOS ISO and never fetches optional developer payloads for
that artifact. On the installed target PC, opening the Studio provisioning
panel activates `devcore-provisiond`; boot and login do not start package
downloads. NetworkManager remains responsible for Wi-Fi credentials, and a
failed/offline request remains resumable without storing a password or token.
The user must choose and approve packages after seeing their download and disk
requirements. The first-login UI must still be exercised in a disposable
installed guest before release evidence is accepted.

## State ownership

- SQLite stores indexed local metadata such as projects, environments, build
  history, test records, and user preferences.
- Filesystem/content-addressed storage holds artifacts, logs, and caches.
- OCI images hold reproducible development environments.
- The immutable bootc deployment holds OS content.
- No credentials, passwords, API keys, or signing keys are persisted in source
  or release images.

## Resource-engine policy

The initial hardware profile is a conservative startup label only. Runtime
decisions must incorporate live PSI, workload class, battery/AC state, thermal
state, and observed contention. The shell, terminal, Studio, debugger, and
unsaved work are protected before restartable build workers, emulators,
indexers, and caches.
