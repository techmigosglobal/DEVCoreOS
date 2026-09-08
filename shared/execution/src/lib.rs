#![forbid(unsafe_code)]
//! Bounded process execution for service-owned DevCore jobs.
//!
//! The executor accepts an already-planned argv vector and never invokes a
//! shell. It keeps output bounded, supports cooperative cancellation, and can
//! persist a result plus separate logs in a caller-owned private directory.
//! Build and Test services remain responsible for proving that an argv vector
//! came from a validated OCI environment plan before calling this crate.

use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    fs::{self, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use devcore_core::{ScopePlanError, WorkloadScopePlan};
use nix::{sys::signal, unistd::Pid};
use serde::{Deserialize, Serialize};

const MAX_CAPTURE_BYTES: usize = 16 * 1024 * 1024;
const MAX_LOG_READ_BYTES: u64 = 1024 * 1024;
const SUPERVISOR_INTERVAL: Duration = Duration::from_millis(25);
const CANCELLATION_GRACE: Duration = Duration::from_millis(250);

/// An execution request containing a direct program and argv entries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionRequest {
    /// Stable job identifier used for result and log filenames.
    pub id: String,
    /// Program plus arguments. The first entry is passed to Command::new.
    pub argv: Vec<String>,
    /// Optional host working directory for the supervisor process.
    pub working_directory: Option<PathBuf>,
    /// Explicit environment entries for the supervisor process.
    pub environment: BTreeMap<String, String>,
}

impl ExecutionRequest {
    /// Creates a request from a program and separate arguments.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let mut argv = vec![program.into()];
        argv.extend(args.into_iter().map(Into::into));
        Self {
            id: id.into(),
            argv,
            working_directory: None,
            environment: BTreeMap::new(),
        }
    }

    /// Creates a request from a complete, already-planned argv vector.
    #[must_use]
    pub fn from_argv(id: impl Into<String>, argv: Vec<String>) -> Self {
        Self {
            id: id.into(),
            argv,
            working_directory: None,
            environment: BTreeMap::new(),
        }
    }

    /// Sets the supervisor working directory.
    #[must_use]
    pub fn with_working_directory(mut self, path: PathBuf) -> Self {
        self.working_directory = Some(path);
        self
    }

    /// Adds one explicit supervisor environment entry.
    #[must_use]
    pub fn with_environment(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.environment.insert(key.into(), value.into());
        self
    }

    /// Validates the request without starting a process.
    pub fn validate(&self) -> Result<(), ExecutionError> {
        validate_job_id(&self.id)?;
        if self.argv.is_empty()
            || self.argv[0].is_empty()
            || self.argv[0].contains(char::is_whitespace)
        {
            return Err(ExecutionError::InvalidProgram);
        }
        if self.argv.iter().any(|argument| argument.contains('\0')) {
            return Err(ExecutionError::NulArgument);
        }
        for key in self.environment.keys() {
            if key.is_empty() || key.contains('=') || key.contains('\0') {
                return Err(ExecutionError::InvalidEnvironmentKey(key.clone()));
            }
        }
        if let Some(path) = &self.working_directory
            && !path.is_absolute()
        {
            return Err(ExecutionError::RelativeWorkingDirectory(path.clone()));
        }
        Ok(())
    }
}

fn validate_job_id(id: &str) -> Result<(), ExecutionError> {
    if id.is_empty()
        || id.len() > 63
        || !id.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
        || id.starts_with('-')
    {
        return Err(ExecutionError::InvalidJobId(id.to_owned()));
    }
    Ok(())
}

/// A cancellation handle shared with an active job.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    state: Arc<(Mutex<bool>, Condvar)>,
}

impl CancellationToken {
    /// Creates a non-cancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation and wakes a waiting supervisor.
    pub fn cancel(&self) {
        let (lock, wake) = &*self.state;
        if let Ok(mut cancelled) = lock.lock() {
            *cancelled = true;
            wake.notify_all();
        }
    }

    /// Returns whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        let (lock, _) = &*self.state;
        lock.lock().map(|cancelled| *cancelled).unwrap_or(true)
    }

    fn wait_for_change(&self, duration: Duration) {
        let (lock, wake) = &*self.state;
        if let Ok(cancelled) = lock.lock() {
            let _ = wake.wait_timeout_while(cancelled, duration, |cancelled| !*cancelled);
        }
    }
}

/// Terminal state of a supervised job.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum JobState {
    /// The child returned success.
    Succeeded,
    /// The child returned a non-zero status or could not be started.
    Failed,
    /// The cancellation token terminated the child.
    Cancelled,
}

/// Captured output and exit metadata from one job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionResult {
    /// Final job state.
    pub state: JobState,
    /// Platform exit code, when the child returned one.
    pub exit_code: Option<i32>,
    /// Captured standard output, capped by the execution limit.
    pub stdout: Vec<u8>,
    /// Captured standard error, capped by the execution limit.
    pub stderr: Vec<u8>,
    /// Whether either output stream exceeded its capture limit.
    pub output_truncated: bool,
    /// Wall-clock execution duration in milliseconds.
    pub duration_millis: u64,
}

/// Maximum output captured for each stream of one job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionLimits {
    /// Maximum bytes retained per stream.
    pub max_output_bytes: usize,
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            max_output_bytes: MAX_CAPTURE_BYTES,
        }
    }
}

impl ExecutionLimits {
    /// Validates the capture limit without starting a process.
    pub fn validate(self) -> Result<Self, ExecutionError> {
        if self.max_output_bytes == 0 || self.max_output_bytes > MAX_CAPTURE_BYTES {
            return Err(ExecutionError::InvalidOutputLimit(self.max_output_bytes));
        }
        Ok(self)
    }
}

/// Runs validated direct-argv requests with bounded output and cancellation.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExecutionEngine;

impl ExecutionEngine {
    /// Creates a stateless execution engine.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Runs one request and waits for its child to finish or be cancelled.
    pub fn run(
        &self,
        request: &ExecutionRequest,
        cancellation: &CancellationToken,
        limits: ExecutionLimits,
    ) -> Result<ExecutionResult, ExecutionError> {
        request.validate()?;
        let limits = limits.validate()?;
        if cancellation.is_cancelled() {
            return Ok(ExecutionResult {
                state: JobState::Cancelled,
                exit_code: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
                output_truncated: false,
                duration_millis: 0,
            });
        }

        let mut command = Command::new(&request.argv[0]);
        command
            .args(&request.argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear();
        configure_process_group(&mut command);
        inherit_safe_environment(&mut command);
        command.envs(&request.environment);
        if let Some(working_directory) = &request.working_directory {
            command.current_dir(working_directory);
        }

        let started = Instant::now();
        let mut child = command.spawn().map_err(ExecutionError::Spawn)?;
        let stdout = child
            .stdout
            .take()
            .ok_or(ExecutionError::MissingOutputPipe)?;
        let stderr = child
            .stderr
            .take()
            .ok_or(ExecutionError::MissingOutputPipe)?;
        let stdout_reader = thread::spawn(move || read_bounded(stdout, limits.max_output_bytes));
        let stderr_reader = thread::spawn(move || read_bounded(stderr, limits.max_output_bytes));

        let mut cancelled = false;
        let status = loop {
            if let Some(status) = child.try_wait().map_err(ExecutionError::Wait)? {
                break status;
            }
            if cancellation.is_cancelled() {
                cancelled = true;
                break terminate_process_group(&mut child)?;
            }
            cancellation.wait_for_change(SUPERVISOR_INTERVAL);
        };

        let stdout = stdout_reader
            .join()
            .map_err(|_| ExecutionError::OutputReaderPanicked)??;
        let stderr = stderr_reader
            .join()
            .map_err(|_| ExecutionError::OutputReaderPanicked)??;
        let output_truncated = stdout.truncated || stderr.truncated;
        Ok(ExecutionResult {
            state: if cancelled {
                JobState::Cancelled
            } else if status.success() {
                JobState::Succeeded
            } else {
                JobState::Failed
            },
            exit_code: status.code(),
            stdout: stdout.bytes,
            stderr: stderr.bytes,
            output_truncated,
            duration_millis: elapsed_millis(started.elapsed()),
        })
    }
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    // Every supervised request becomes the leader of its own process group.
    // This keeps a cancelled wrapper from leaking descendants such as a
    // container client or a compiler process.
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

fn terminate_process_group(child: &mut Child) -> Result<ExitStatus, ExecutionError> {
    #[cfg(unix)]
    {
        let process_group = Pid::from_raw(child.id().try_into().unwrap_or(i32::MAX));
        let _ = signal::killpg(process_group, signal::Signal::SIGTERM);
    }

    let deadline = Instant::now() + CANCELLATION_GRACE;
    loop {
        if let Some(status) = child.try_wait().map_err(ExecutionError::Wait)? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            #[cfg(unix)]
            {
                let process_group = Pid::from_raw(child.id().try_into().unwrap_or(i32::MAX));
                let _ = signal::killpg(process_group, signal::Signal::SIGKILL);
            }
            let _ = child.kill();
            return child.wait().map_err(ExecutionError::Wait);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn inherit_safe_environment(command: &mut Command) {
    for key in [
        "HOME",
        "PATH",
        "XDG_RUNTIME_DIR",
        "TMPDIR",
        "LANG",
        "LC_ALL",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
}

fn elapsed_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug)]
struct BoundedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

fn read_bounded(mut reader: impl Read, limit: usize) -> io::Result<BoundedOutput> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        if remaining > 0 {
            bytes.extend_from_slice(&buffer[..read.min(remaining)]);
        }
        if read > remaining {
            truncated = true;
        }
    }
    Ok(BoundedOutput { bytes, truncated })
}

/// A private filesystem store for bounded job results and logs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobStore {
    root: PathBuf,
}

/// One of the two private output streams persisted for a job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobLogStream {
    /// Captured standard output.
    Stdout,
    /// Captured standard error.
    Stderr,
}

impl JobLogStream {
    fn suffix(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

/// A bounded slice of a persisted job log.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobLogChunk {
    /// Bytes beginning at the requested offset.
    pub bytes: Vec<u8>,
    /// Whether the returned slice reaches the current end of the log.
    pub end_of_file: bool,
    /// Current total size of the selected log in bytes.
    pub total_bytes: u64,
}

impl JobStore {
    /// Creates the store directory and restricts it to the current user.
    pub fn open(root: PathBuf) -> Result<Self, StoreError> {
        fs::create_dir_all(&root).map_err(StoreError::CreateDirectory)?;
        restrict_permissions(&root).map_err(StoreError::SetPermissions)?;
        Ok(Self { root })
    }

    /// Persists metadata plus separate stdout/stderr files for one job.
    pub fn persist(
        &self,
        request: &ExecutionRequest,
        result: &ExecutionResult,
    ) -> Result<(), StoreError> {
        validate_job_id(&request.id).map_err(StoreError::InvalidJobId)?;
        let metadata = StoredJob {
            id: request.id.clone(),
            state: result.state,
            exit_code: result.exit_code,
            output_truncated: result.output_truncated,
            duration_millis: result.duration_millis,
        };
        let metadata_bytes = serde_json::to_vec(&metadata).map_err(StoreError::Serialize)?;
        self.write_atomic(&request.id, "json", &metadata_bytes)?;
        self.write_atomic(&request.id, "stdout", &result.stdout)?;
        self.write_atomic(&request.id, "stderr", &result.stderr)?;
        Ok(())
    }

    /// Reads a bounded slice of one private log stream.
    ///
    /// Logs are persisted atomically after a job reaches a terminal state.
    /// Keeping the limit here, rather than trusting a D-Bus caller, prevents
    /// one request from allocating an arbitrary amount of memory.
    pub fn read_log(
        &self,
        id: &str,
        stream: JobLogStream,
        offset: u64,
        max_bytes: u64,
    ) -> Result<JobLogChunk, StoreError> {
        validate_job_id(id).map_err(StoreError::InvalidJobId)?;
        if max_bytes == 0 || max_bytes > MAX_LOG_READ_BYTES {
            return Err(StoreError::InvalidLogReadLimit(max_bytes));
        }

        let path = self.root.join(format!("{id}.{}", stream.suffix()));
        let mut file = fs::File::open(path).map_err(StoreError::Read)?;
        let total_bytes = file.metadata().map_err(StoreError::Read)?.len();
        file.seek(SeekFrom::Start(offset))
            .map_err(StoreError::Read)?;

        let capacity = total_bytes
            .saturating_sub(offset)
            .min(max_bytes)
            .try_into()
            .expect("bounded log read fits usize");
        let mut bytes = Vec::with_capacity(capacity);
        file.take(max_bytes)
            .read_to_end(&mut bytes)
            .map_err(StoreError::Read)?;
        let end_of_file = offset.saturating_add(bytes.len() as u64) >= total_bytes;
        Ok(JobLogChunk {
            bytes,
            end_of_file,
            total_bytes,
        })
    }

    fn write_atomic(&self, id: &str, suffix: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let target = self.root.join(format!("{id}.{suffix}"));
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(StoreError::Clock)?
            .as_nanos();
        let temporary = self.root.join(format!(".{id}.{suffix}.{nonce}.tmp"));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(StoreError::Write)?;
        restrict_permissions(&temporary).map_err(StoreError::SetPermissions)?;
        file.write_all(bytes).map_err(StoreError::Write)?;
        file.sync_all().map_err(StoreError::Write)?;
        drop(file);
        fs::rename(&temporary, target).map_err(StoreError::Write)
    }
}

/// Lifecycle state exposed by the service job registry.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum JobLifecycle {
    /// The job has been accepted but has not started.
    Queued,
    /// The direct child is running.
    Running,
    /// The child completed successfully.
    Succeeded,
    /// The child failed or its result could not be persisted.
    Failed,
    /// The child was terminated after cancellation.
    Cancelled,
}

impl JobLifecycle {
    fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// Queryable job metadata that deliberately excludes captured logs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JobStatus {
    /// Stable job identifier.
    pub id: String,
    /// Current lifecycle state.
    pub lifecycle: JobLifecycle,
    /// Child exit code, when available.
    pub exit_code: Option<i32>,
    /// Whether captured output exceeded the configured limit.
    pub output_truncated: bool,
    /// Wall-clock duration in milliseconds.
    pub duration_millis: u64,
    /// Non-secret failure description, when the supervisor failed.
    pub error: Option<String>,
}

#[derive(Debug)]
struct JobCell {
    status: Mutex<JobStatus>,
    wake: Condvar,
    cancellation: CancellationToken,
}

impl JobCell {
    fn new(id: String) -> Self {
        Self {
            status: Mutex::new(JobStatus {
                id,
                lifecycle: JobLifecycle::Queued,
                exit_code: None,
                output_truncated: false,
                duration_millis: 0,
                error: None,
            }),
            wake: Condvar::new(),
            cancellation: CancellationToken::new(),
        }
    }

    fn set_status(&self, status: JobStatus) {
        if let Ok(mut current) = self.status.lock() {
            *current = status;
            self.wake.notify_all();
        }
    }
}

/// A handle for waiting on one registered job.
#[derive(Clone, Debug)]
pub struct JobHandle {
    cell: Arc<JobCell>,
}

impl JobHandle {
    /// Returns the stable identifier of this job.
    #[must_use]
    pub fn id(&self) -> String {
        self.cell
            .status
            .lock()
            .expect("job status mutex is not poisoned")
            .id
            .clone()
    }

    /// Waits until the job reaches a terminal state.
    #[must_use]
    pub fn wait(&self) -> JobStatus {
        let status = self
            .cell
            .status
            .lock()
            .expect("job status mutex is not poisoned");
        self.cell
            .wake
            .wait_while(status, |status| !status.lifecycle.is_terminal())
            .expect("job status mutex is not poisoned")
            .clone()
    }

    /// Returns the latest status without waiting.
    #[must_use]
    pub fn status(&self) -> JobStatus {
        self.cell
            .status
            .lock()
            .expect("job status mutex is not poisoned")
            .clone()
    }
}

/// In-process registry used by the user-session Build/Test D-Bus service.
#[derive(Clone, Debug)]
pub struct JobCoordinator {
    jobs: Arc<Mutex<BTreeMap<String, Arc<JobCell>>>>,
    store: Option<JobStore>,
}

impl JobCoordinator {
    /// Creates a coordinator without persistence.
    #[must_use]
    pub fn new() -> Self {
        Self {
            jobs: Arc::new(Mutex::new(BTreeMap::new())),
            store: None,
        }
    }

    /// Creates a coordinator that persists terminal results to a job store.
    #[must_use]
    pub fn with_store(store: JobStore) -> Self {
        Self {
            jobs: Arc::new(Mutex::new(BTreeMap::new())),
            store: Some(store),
        }
    }

    /// Registers and starts one validated job on a bounded worker thread.
    pub fn start(
        &self,
        request: ExecutionRequest,
        limits: ExecutionLimits,
    ) -> Result<JobHandle, CoordinatorError> {
        request
            .validate()
            .map_err(CoordinatorError::InvalidRequest)?;
        limits
            .validate()
            .map_err(CoordinatorError::InvalidRequest)?;
        let id = request.id.clone();
        let cell = Arc::new(JobCell::new(id.clone()));
        {
            let mut jobs = self
                .jobs
                .lock()
                .map_err(|_| CoordinatorError::StatePoisoned)?;
            if jobs.contains_key(&id) {
                return Err(CoordinatorError::DuplicateJob(id));
            }
            jobs.insert(id.clone(), Arc::clone(&cell));
        }

        let store = self.store.clone();
        let worker_cell = Arc::clone(&cell);
        let worker = thread::Builder::new()
            .name(format!("devcore-job-{id}"))
            .spawn(move || run_registered_job(worker_cell, request, limits, store))
            .map_err(|error| CoordinatorError::WorkerSpawn {
                id: id.clone(),
                error,
            });
        if let Err(error) = worker {
            if let Ok(mut jobs) = self.jobs.lock() {
                jobs.remove(&id);
            }
            return Err(error);
        }
        Ok(JobHandle { cell })
    }

    /// Registers a job under a matching, non-deferred resource scope.
    ///
    /// The scope is rendered as direct argv and then supervised exactly like a
    /// normal job. A mismatched unit name or deferred policy is rejected before
    /// a worker thread is created.
    pub fn start_in_scope(
        &self,
        request: ExecutionRequest,
        scope: &WorkloadScopePlan,
        limits: ExecutionLimits,
    ) -> Result<JobHandle, CoordinatorError> {
        let expected_unit = format!("devcore-job-{}", request.id);
        if scope.unit_name != expected_unit {
            return Err(CoordinatorError::ScopeJobMismatch {
                request_id: request.id,
                unit_name: scope.unit_name.clone(),
            });
        }
        if scope.deferred {
            return Err(CoordinatorError::Deferred(request.id));
        }
        let argv = scope
            .systemd_run_argv(&request.argv)
            .map_err(CoordinatorError::InvalidScope)?;
        let scoped_request = ExecutionRequest { argv, ..request };
        self.start(scoped_request, limits)
    }

    /// Requests cancellation of a registered job.
    pub fn cancel(&self, id: &str) -> Result<(), CoordinatorError> {
        let cell = self
            .jobs
            .lock()
            .map_err(|_| CoordinatorError::StatePoisoned)?
            .get(id)
            .cloned()
            .ok_or_else(|| CoordinatorError::UnknownJob(id.to_owned()))?;
        cell.cancellation.cancel();
        Ok(())
    }

    /// Returns the latest status of a registered job.
    pub fn status(&self, id: &str) -> Result<JobStatus, CoordinatorError> {
        self.jobs
            .lock()
            .map_err(|_| CoordinatorError::StatePoisoned)?
            .get(id)
            .map(|cell| {
                JobHandle {
                    cell: Arc::clone(cell),
                }
                .status()
            })
            .ok_or_else(|| CoordinatorError::UnknownJob(id.to_owned()))
    }

    /// Reads a bounded persisted log slice for a registered job.
    pub fn read_log(
        &self,
        id: &str,
        stream: JobLogStream,
        offset: u64,
        max_bytes: u64,
    ) -> Result<JobLogChunk, CoordinatorError> {
        if !self
            .jobs
            .lock()
            .map_err(|_| CoordinatorError::StatePoisoned)?
            .contains_key(id)
        {
            return Err(CoordinatorError::UnknownJob(id.to_owned()));
        }
        self.store
            .as_ref()
            .ok_or(CoordinatorError::LogsUnavailable)?
            .read_log(id, stream, offset, max_bytes)
            .map_err(CoordinatorError::Store)
    }
}

impl Default for JobCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

fn run_registered_job(
    cell: Arc<JobCell>,
    request: ExecutionRequest,
    limits: ExecutionLimits,
    store: Option<JobStore>,
) {
    cell.set_status(JobStatus {
        id: request.id.clone(),
        lifecycle: JobLifecycle::Running,
        exit_code: None,
        output_truncated: false,
        duration_millis: 0,
        error: None,
    });
    let result = ExecutionEngine::new().run(&request, &cell.cancellation, limits);
    match result {
        Ok(result) => {
            let persisted = store
                .as_ref()
                .map(|store| store.persist(&request, &result))
                .transpose();
            let (lifecycle, error) = match persisted {
                Ok(_) => (job_lifecycle(result.state), None),
                Err(error) => (JobLifecycle::Failed, Some(error.to_string())),
            };
            cell.set_status(JobStatus {
                id: request.id,
                lifecycle,
                exit_code: result.exit_code,
                output_truncated: result.output_truncated,
                duration_millis: result.duration_millis,
                error,
            });
        }
        Err(error) => cell.set_status(JobStatus {
            id: request.id,
            lifecycle: JobLifecycle::Failed,
            exit_code: None,
            output_truncated: false,
            duration_millis: 0,
            error: Some(error.to_string()),
        }),
    }
}

fn job_lifecycle(state: JobState) -> JobLifecycle {
    match state {
        JobState::Succeeded => JobLifecycle::Succeeded,
        JobState::Failed => JobLifecycle::Failed,
        JobState::Cancelled => JobLifecycle::Cancelled,
    }
}

/// Job-registry failure.
#[derive(Debug)]
pub enum CoordinatorError {
    /// The request or output limits failed validation.
    InvalidRequest(ExecutionError),
    /// A job with the same identifier is already registered.
    DuplicateJob(String),
    /// No registered job has this identifier.
    UnknownJob(String),
    /// The requested policy explicitly deferred this workload.
    Deferred(String),
    /// The scope unit was not derived from the request identifier.
    ScopeJobMismatch {
        /// Job identifier carried by the request.
        request_id: String,
        /// Unit name supplied by the caller.
        unit_name: String,
    },
    /// The supplied scope could not be converted into safe direct argv.
    InvalidScope(ScopePlanError),
    /// The coordinator was created without a private log store.
    LogsUnavailable,
    /// A private log could not be read.
    Store(StoreError),
    /// A registry mutex was poisoned by a failed worker or caller.
    StatePoisoned,
    /// The worker thread could not be created.
    WorkerSpawn {
        /// Job whose worker could not be created.
        id: String,
        /// Thread creation error.
        error: io::Error,
    },
}

impl fmt::Display for CoordinatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(error) => write!(formatter, "invalid job request: {error}"),
            Self::DuplicateJob(id) => write!(formatter, "job {id:?} is already registered"),
            Self::UnknownJob(id) => write!(formatter, "unknown job {id:?}"),
            Self::Deferred(id) => write!(formatter, "job {id:?} is deferred by resource policy"),
            Self::ScopeJobMismatch {
                request_id,
                unit_name,
            } => write!(
                formatter,
                "scope {unit_name:?} does not match job {request_id:?}"
            ),
            Self::InvalidScope(error) => write!(formatter, "invalid resource scope: {error}"),
            Self::LogsUnavailable => formatter.write_str("job logs are unavailable"),
            Self::Store(error) => write!(formatter, "cannot read job log: {error}"),
            Self::StatePoisoned => formatter.write_str("job registry state is poisoned"),
            Self::WorkerSpawn { id, error } => {
                write!(formatter, "cannot start worker for job {id:?}: {error}")
            }
        }
    }
}

impl Error for CoordinatorError {}

#[derive(Debug, Serialize)]
struct StoredJob {
    id: String,
    state: JobState,
    exit_code: Option<i32>,
    output_truncated: bool,
    duration_millis: u64,
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Validation or process-execution failure.
#[derive(Debug)]
pub enum ExecutionError {
    /// Job identifier is not safe for a filename.
    InvalidJobId(String),
    /// Program is empty or contains whitespace.
    InvalidProgram,
    /// An argv entry contains a NUL byte.
    NulArgument,
    /// Environment key is malformed.
    InvalidEnvironmentKey(String),
    /// Working directory is not absolute.
    RelativeWorkingDirectory(PathBuf),
    /// Output capture limit is outside the safe range.
    InvalidOutputLimit(usize),
    /// Child process could not be started.
    Spawn(io::Error),
    /// Child status could not be queried or waited for.
    Wait(io::Error),
    /// Child output pipe was unexpectedly unavailable.
    MissingOutputPipe,
    /// A bounded output reader thread failed.
    OutputReaderPanicked,
    /// A bounded output reader returned an I/O error.
    ReadOutput(io::Error),
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJobId(id) => write!(formatter, "invalid job id {id:?}"),
            Self::InvalidProgram => formatter.write_str("invalid direct-argv program"),
            Self::NulArgument => formatter.write_str("argv contains a NUL byte"),
            Self::InvalidEnvironmentKey(key) => {
                write!(formatter, "invalid environment key {key:?}")
            }
            Self::RelativeWorkingDirectory(path) => {
                write!(
                    formatter,
                    "working directory is not absolute: {}",
                    path.display()
                )
            }
            Self::InvalidOutputLimit(limit) => {
                write!(formatter, "invalid per-stream output limit {limit} bytes")
            }
            Self::Spawn(error) => write!(formatter, "cannot start job: {error}"),
            Self::Wait(error) => write!(formatter, "cannot wait for job: {error}"),
            Self::MissingOutputPipe => formatter.write_str("job output pipe was unavailable"),
            Self::OutputReaderPanicked => formatter.write_str("job output reader panicked"),
            Self::ReadOutput(error) => write!(formatter, "cannot read job output: {error}"),
        }
    }
}

impl Error for ExecutionError {}

impl From<io::Error> for ExecutionError {
    fn from(error: io::Error) -> Self {
        Self::ReadOutput(error)
    }
}

/// Filesystem persistence failure.
#[derive(Debug)]
pub enum StoreError {
    /// Store root could not be created.
    CreateDirectory(io::Error),
    /// Store or file permissions could not be restricted.
    SetPermissions(io::Error),
    /// Job identifier was invalid.
    InvalidJobId(ExecutionError),
    /// Metadata could not be serialized.
    Serialize(serde_json::Error),
    /// Timestamp could not be read.
    Clock(std::time::SystemTimeError),
    /// A metadata or log file could not be written or atomically renamed.
    Write(io::Error),
    /// A private log could not be opened, inspected, or read.
    Read(io::Error),
    /// A log request exceeded the bounded read policy.
    InvalidLogReadLimit(u64),
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateDirectory(error) => write!(formatter, "cannot create job store: {error}"),
            Self::SetPermissions(error) => write!(formatter, "cannot restrict job store: {error}"),
            Self::InvalidJobId(error) => error.fmt(formatter),
            Self::Serialize(error) => write!(formatter, "cannot serialize job result: {error}"),
            Self::Clock(error) => write!(formatter, "cannot create job record nonce: {error}"),
            Self::Write(error) => write!(formatter, "cannot write job record: {error}"),
            Self::Read(error) => write!(formatter, "cannot read job log: {error}"),
            Self::InvalidLogReadLimit(limit) => write!(
                formatter,
                "invalid job log read limit {limit} bytes; expected 1..=1048576"
            ),
        }
    }
}

impl Error for StoreError {}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, thread, time::Duration};

    use super::{
        CancellationToken, CoordinatorError, ExecutionEngine, ExecutionError, ExecutionLimits,
        ExecutionRequest, JobCoordinator, JobLifecycle, JobLogStream, JobState, JobStore,
    };
    use devcore_core::{
        HardwareCapacity, MachineProfile, PowerState, ResourceSnapshot, WorkloadClass,
    };
    use nix::{sys::signal, unistd::Pid};

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("devcore-execution-{name}-{}", std::process::id()))
    }

    #[test]
    fn runs_direct_argv_and_bounds_output() {
        let request = ExecutionRequest::new("bounded-output", "sh", ["-c", "printf '0123456789'"]);
        let result = ExecutionEngine::new()
            .run(
                &request,
                &CancellationToken::new(),
                ExecutionLimits {
                    max_output_bytes: 4,
                },
            )
            .unwrap();

        assert_eq!(result.state, JobState::Succeeded);
        assert_eq!(result.stdout, b"0123");
        assert!(result.output_truncated);
    }

    #[test]
    fn cancellation_terminates_a_running_job() {
        let request = ExecutionRequest::new("cancel-job", "sleep", ["5"]);
        let token = CancellationToken::new();
        let cancellation = token.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            cancellation.cancel();
        });

        let result = ExecutionEngine::new()
            .run(&request, &token, ExecutionLimits::default())
            .unwrap();
        canceller.join().unwrap();

        assert_eq!(result.state, JobState::Cancelled);
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_terminates_descendant_process_group() {
        let request = ExecutionRequest::new(
            "cancel-group-job",
            "sh",
            ["-c", "sleep 5 & printf '%s\\n' \"$!\"; wait"],
        );
        let token = CancellationToken::new();
        let cancellation = token.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            cancellation.cancel();
        });

        let result = ExecutionEngine::new()
            .run(&request, &token, ExecutionLimits::default())
            .unwrap();
        canceller.join().unwrap();

        assert_eq!(result.state, JobState::Cancelled);
        let descendant_pid = String::from_utf8(result.stdout)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        thread::sleep(Duration::from_millis(50));
        assert!(
            signal::kill(Pid::from_raw(descendant_pid), None).is_err(),
            "cancelled job descendant {descendant_pid} is still alive"
        );
    }

    #[test]
    fn invalid_requests_never_spawn_a_process() {
        let request = ExecutionRequest::new("bad id", "sh", ["-c", "true"]);
        let error = ExecutionEngine::new()
            .run(
                &request,
                &CancellationToken::new(),
                ExecutionLimits::default(),
            )
            .unwrap_err();

        assert!(matches!(error, ExecutionError::InvalidJobId(_)));
    }

    #[test]
    fn store_writes_metadata_and_separate_logs() {
        let root = test_root("store");
        let _ = fs::remove_dir_all(&root);
        let store = JobStore::open(root.clone()).unwrap();
        let request = ExecutionRequest::new("stored-job", "sh", ["-c", "printf ok"]);
        let result = ExecutionEngine::new()
            .run(
                &request,
                &CancellationToken::new(),
                ExecutionLimits::default(),
            )
            .unwrap();
        store.persist(&request, &result).unwrap();

        assert!(root.join("stored-job.json").is_file());
        assert_eq!(fs::read(root.join("stored-job.stdout")).unwrap(), b"ok");
        assert!(root.join("stored-job.stderr").is_file());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn store_reads_bounded_private_log_slices() {
        let root = test_root("read-log");
        let _ = fs::remove_dir_all(&root);
        let store = JobStore::open(root.clone()).unwrap();
        let request = ExecutionRequest::new("read-log-job", "sh", ["-c", "printf stdout"]);
        let result = ExecutionEngine::new()
            .run(
                &request,
                &CancellationToken::new(),
                ExecutionLimits::default(),
            )
            .unwrap();
        store.persist(&request, &result).unwrap();

        let first = store
            .read_log("read-log-job", JobLogStream::Stdout, 0, 3)
            .unwrap();
        assert_eq!(first.bytes, b"std");
        assert!(!first.end_of_file);
        assert_eq!(first.total_bytes, 6);

        let second = store
            .read_log("read-log-job", JobLogStream::Stdout, 3, 3)
            .unwrap();
        assert_eq!(second.bytes, b"out");
        assert!(second.end_of_file);

        let empty = store
            .read_log("read-log-job", JobLogStream::Stderr, 0, 3)
            .unwrap();
        assert!(empty.bytes.is_empty());
        assert!(empty.end_of_file);
        assert_eq!(empty.total_bytes, 0);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn store_rejects_unbounded_log_reads() {
        let root = test_root("read-log-limit");
        let _ = fs::remove_dir_all(&root);
        let store = JobStore::open(root.clone()).unwrap();
        assert!(matches!(
            store.read_log("job", JobLogStream::Stdout, 0, 1024 * 1024 + 1),
            Err(super::StoreError::InvalidLogReadLimit(limit)) if limit == 1024 * 1024 + 1
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn coordinator_runs_jobs_and_persists_terminal_state() {
        let root = test_root("coordinator");
        let _ = fs::remove_dir_all(&root);
        let coordinator = JobCoordinator::with_store(JobStore::open(root.clone()).unwrap());
        let request = ExecutionRequest::new("coordinated-job", "printf", ["ok"]);
        let handle = coordinator
            .start(request, ExecutionLimits::default())
            .unwrap();

        let status = handle.wait();

        assert_eq!(status.id, "coordinated-job");
        assert_eq!(status.lifecycle, JobLifecycle::Succeeded);
        assert_eq!(coordinator.status("coordinated-job").unwrap(), status);
        assert_eq!(
            fs::read(root.join("coordinated-job.stdout")).unwrap(),
            b"ok"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn coordinator_rejects_duplicates_and_cancels_registered_jobs() {
        let coordinator = JobCoordinator::new();
        let request = ExecutionRequest::new("duplicate-job", "sleep", ["5"]);
        let handle = coordinator
            .start(request.clone(), ExecutionLimits::default())
            .unwrap();
        assert!(matches!(
            coordinator.start(request, ExecutionLimits::default()),
            Err(CoordinatorError::DuplicateJob(id)) if id == "duplicate-job"
        ));

        coordinator.cancel("duplicate-job").unwrap();
        assert_eq!(handle.wait().lifecycle, JobLifecycle::Cancelled);
        assert!(matches!(
            coordinator.cancel("missing-job"),
            Err(CoordinatorError::UnknownJob(id)) if id == "missing-job"
        ));
    }

    #[test]
    fn scoped_coordinator_rejects_deferred_work_before_spawning() {
        let coordinator = JobCoordinator::new();
        let request = ExecutionRequest::new("emulator-job", "printf", ["not-started"]);
        let snapshot = ResourceSnapshot {
            capacity: HardwareCapacity::new(2, 4 * 1024 * 1024),
            initial_profile: MachineProfile::Low,
            memory_available_kib: Some(2 * 1024 * 1024),
            cpu_pressure: None,
            memory_pressure: None,
            io_pressure: None,
            power: PowerState::default(),
        };
        let scope = snapshot
            .workload_scope("emulator-job", WorkloadClass::Emulator)
            .unwrap();

        assert!(matches!(
            coordinator.start_in_scope(request, &scope, ExecutionLimits::default()),
            Err(CoordinatorError::Deferred(id)) if id == "emulator-job"
        ));
        assert!(matches!(
            coordinator.status("emulator-job"),
            Err(CoordinatorError::UnknownJob(id)) if id == "emulator-job"
        ));
    }

    #[test]
    fn scoped_coordinator_rejects_a_scope_for_another_job() {
        let coordinator = JobCoordinator::new();
        let request = ExecutionRequest::new("build-job", "printf", ["not-started"]);
        let snapshot = ResourceSnapshot {
            capacity: HardwareCapacity::new(2, 4 * 1024 * 1024),
            initial_profile: MachineProfile::Low,
            memory_available_kib: Some(2 * 1024 * 1024),
            cpu_pressure: None,
            memory_pressure: None,
            io_pressure: None,
            power: PowerState::default(),
        };
        let scope = snapshot
            .workload_scope("different-job", WorkloadClass::Build)
            .unwrap();

        assert!(matches!(
            coordinator.start_in_scope(request, &scope, ExecutionLimits::default()),
            Err(CoordinatorError::ScopeJobMismatch { request_id, unit_name })
                if request_id == "build-job" && unit_name == "devcore-job-different-job"
        ));
    }
}
