# DevCore Build Engine

The first Build Engine slice is a dependency-aware planning boundary. It knows
the existing Cargo, Go, CMake, Gradle, Flutter, Qt, npm, and pnpm adapters,
validates a deterministic dependency DAG, produces direct program/argument
vectors without shell interpolation, and reads the shared `devcored` build
worker budget.

It intentionally does not execute a host command. The execution milestone must
run these plans through the Environment Manager inside a rootless OCI
development environment, with cancellation, logs, artifacts,
content-addressed caching, and reproducibility checks. This keeps build tools
out of the minimal OS base and avoids granting untrusted project commands host
privileges. The current seam maps a `BuildStep` working directory into the
environment's `/workspace` path and rejects paths outside that mount. Typed
Build requests use a disposable `podman run --rm` environment invocation, so
the service does not require a pre-started named container.

`BuildStep::execution_request` packages that validated argv for the bounded
`devcore-execution` supervisor. The request can be run only after the Work
service has authorized the environment lifecycle and resource scope; this
library still does not start a process, cache an artifact, or write a build
record.

`BuildStep::execution_request_with_scope` adds the same boundary for a
workload-matched `WorkloadScopePlan`. It rejects an Indexer, Emulator, or other
incorrect scope before producing the wrapped request. The resource plan is
still declarative until a privileged scope service applies it.

`BuildCacheDescriptor` now derives a SHA-256 content address from the pinned
environment image, source fingerprint, adapter command, step, and normalized
dependencies. `BuildCacheStore` publishes regular-file artifacts atomically
under that address and treats repeated publication as idempotent. The
user-session service does not yet discover source fingerprints or attach cache
records to completed jobs, so this primitive is not presented as incremental
build support yet.

The initial `org.devcore.Build1` surface is now provided by the on-demand
user-session `devcore-workd` service. It accepts a known adapter, environment,
job id, step id, and workspace directory, then submits the validated request to
the bounded coordinator. Studio can query the resulting Job status and bounded
stdout record. The service does not yet persist DAGs, artifacts, cache keys, or
incremental-build records.
