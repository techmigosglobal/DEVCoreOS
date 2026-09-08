# DevCore Test Engine

The initial Test Engine slice is a dependency-aware, shell-free planner. It
models static analysis, unit, integration, UI, API, database, device,
emulator, performance, memory-pressure, security, and regression test work.

Scheduling consumes the same conservative `devcored` resource policy as the
Build Engine. Emulator work is deferred on LOW profiles and during relevant
pressure/battery conditions; ordinary test work receives the restartable build
budget. The planner records decisions but does not execute host commands.

Execution remains a separate service concern: direct argv commands must run in
the selected rootless OCI environment or on an explicitly approved device or
remote worker, with cancellation, logs, result records, and artifact paths
added before claiming end-to-end test execution.

`TestStep::execution_request` maps a validated test step into the same direct,
disposable Podman execution boundary used by Build. The service layer must still decide
whether a schedule is runnable, allocate the environment, persist the result,
and expose cancellation through its D-Bus contract.

`TestStep::execution_request_with_scope` can wrap an environment command in a
scope derived for the step's workload class. A mismatched scope is rejected,
and the resulting argv remains declarative until the service authorizes the
environment and applies the resource settings.

The initial `org.devcore.Test1` surface is now provided by `devcore-workd`. It
accepts a typed test kind and direct command arguments, so low-profile emulator
deferral and workspace validation happen before a job can be registered. The
Job interface exposes status, cancellation, and bounded stdout/stderr reads;
device transport, remote workers, result indexing, and live OCI test evidence
remain separate milestones.
