# DevCore Environment Manager

The first Environment Manager slice plans rootless Podman lifecycle and
in-environment command invocations for Rust, Go, Python, Node.js, Flutter,
Android SDK, Java, Qt, and CMake workspaces. It
uses a single normalized workspace bind mount, a read-only root filesystem,
`keep-id` user mapping, dropped capabilities, `no-new-privileges`, and a
network-disabled default.

The planner never builds a shell command string and does not execute host
commands. Build steps can map their host working directory to a validated
`/workspace`-relative container directory. Typed Build/Test jobs use a lazy,
disposable `podman run --rm` invocation, while the on-demand user-session
`devcore-workd` service owns lifecycle submission, cancellation, status, and
bounded terminal log reads. Mutable toolchains and third-party packages remain
inside OCI images rather than the minimal OS base. Toolchain identifiers are
declarative metadata; the selected environment image supplies the actual SDK
and compiler lazily when a Build, Test, or explicit environment operation runs.

`EnvironmentSpec` now also produces executor requests for create,
workspace-scoped exec, stop, and remove lifecycle operations. These requests are
revalidated at the environment boundary and remain declarative until a service
authorizes the transition and sends them through the bounded execution
coordinator. The on-demand `devcore-workd` session service now owns that
submission boundary and keeps definitions in memory for the session; no
permanent daemon or Podman process is started at boot by this feature.
