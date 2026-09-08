# DevCore Execution Boundary

`devcore-execution` is the reusable process primitive for the user-session
Build and Test service. It intentionally does not decide whether a command is allowed
to run. Callers must first validate the project plan and produce an OCI-scoped
argv such as the Environment Manager's Podman command.

The boundary then provides:

- direct `Command` argv execution with no shell interpolation;
- `stdin` closed by default so a background job cannot block on a hidden
  terminal;
- a small allowlist of inherited runtime variables and explicit request
  variables only;
- concurrent bounded stdout/stderr readers, capped at 16 MiB per stream;
- cancellation that terminates the supervised process group with a bounded
  TERM/KILL grace period and reports a distinct cancelled state; and
- atomic private filesystem records containing JSON metadata and separate log
  files.

`JobCoordinator` owns the in-process lifecycle map used by `devcore-workd`. It
rejects duplicate identifiers without replacing an active job, supports status
and cancellation lookup, and wakes waiting handles when a job reaches a
terminal state. Captured logs stay out of status responses and are written only
through the optional private `JobStore`.

Build and Test can additionally wrap their environment-scoped argv in a
workload-matched resource-scope plan. That wrapper remains direct argv and is
validated before use; it is not permission to create a cgroup from an ordinary
job request. `JobCoordinator::start_in_scope` adds the final guard: the scope
unit must be derived from the same job id and deferred policy decisions are
rejected before a worker thread is created.

The current implementation includes the `devcore-workd` session-bus service
and focused D-Bus startup/introspection checks. That verifier exercises typed
Build/Test submission, successful completion, cancellation, status, and
bounded log reads with disposable command fixtures. `Job1.ReadLog` exposes
only bounded slices of terminal stdout/stderr records; it does not stream a
live pipe. The executor now creates a Unix process group per request so
cancellation does not leave descendants such as a container client or compiler
running. It does not yet replace the planned SQLite metadata index. Live
Podman, systemd-scope, and
guest-session execution remain environment-dependent runtime checks.
