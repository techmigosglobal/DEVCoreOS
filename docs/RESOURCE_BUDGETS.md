# DevCore Resource Budgets

## Measurement policy

DevCore does not claim to be lighter or faster without measurement evidence.
`tools/bench/collect-baseline.sh` records raw host or guest facts; the release
process stores those raw files alongside a human-readable report.

## Required measurements

- Boot time.
- Idle RAM.
- Idle CPU.
- Running process count.
- DevCore service and compositor RSS.
- PSI CPU, memory, and I/O.
- Storage footprint.
- Build responsiveness under contention.
- Battery and thermal behavior on supported physical hardware.

## Initial budgeting rules

- LOW, BALANCED, STANDARD, and WORKSTATION are test fixtures, not separate
  editions.
- Capacity labels only establish conservative startup defaults.
- PSI and workload state override capacity-derived concurrency decisions.
- The first policy slice uses small baseline caps: build workers of 1/2/4/8
  for LOW/BALANCED/STANDARD/WORKSTATION, indexer workers of 1/1/2/4, and
  emulator workers of 0/1/1/2. A 1% ten-second PSI signal halves restartable
  background concurrency; 5% caps it at one worker, and LOW/on-battery
  emulators are deferred. These values are provisional until measured.
- Idle background services require an owner, a measurable purpose, and a
  startup policy.
- The immutable base image includes only session/network/audio/graphics support,
  terminal, Git, rootless Podman, Fedora's low-overhead zram-generator defaults,
  and the DevCore core. Compilers, SDKs, browsers, IDE extensions, and
  third-party GUI applications are lazy, explicit-install capabilities rather
  than base-image packages. zram configuration is kept declarative and has no
  polling daemon; its actual benefit must be measured in a DevCore guest.
- Post-install provisioning is also profile- and free-space-aware: it refuses
  Android Studio/Flutter/Android SDK defaults on constrained profiles, reserves
  a fixed 4 GiB headroom, and installs only the bundles the user selected.
  It does not poll for Wi-Fi or download during boot; the user retries after
  NetworkManager reports a connection.
- Long-running polling is prohibited unless an event-driven alternative is
  documented as unavailable and the polling cost is measured.
- New limits are not accepted until the previous behavior and new behavior are
  measured on the same profile.

## Scope planning boundary

`ResourceSnapshot::workload_scope` now turns a scheduling decision into a
validated, non-executing `WorkloadScopePlan`. The plan uses stable
`devcore-job-*` unit names, systemd CPU weights, and conservative cgroup-v2
memory controls:

- protected interactive work receives a `MemoryLow` floor;
- normal build/indexer work receives no artificial memory ceiling; and
- reclaimable cache/emulator work receives `MemoryHigh`, while emulator work
  also receives a `MemoryMax` ceiling.

The plan can render a direct `systemd-run --user --scope` argv. Build and Test
request helpers reject a scope created for a different workload class, and the
user-session Work coordinator applies the wrapper before supervising the
request. The core crate itself never launches a command or changes a cgroup;
the Work service remains rootless, while a future privileged resource service
would be required for system-wide policy. Live cgroup accounting and
measurements are still required before these provisional values are treated as
final.

## Current baseline status

No DevCore OS guest baseline has been captured. The existing `devcored` command
can collect host telemetry, but host measurements must not be presented as
DevCore image measurements.
