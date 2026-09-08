#![forbid(unsafe_code)]
#![allow(missing_docs)]
//! User-session orchestration for validated DevCore work requests.
//!
//! The service owns the narrow transition from Environment/Build/Test plans to
//! the bounded executor. It runs as the logged-in user, accepts only digest
//! pinned environments and typed plan inputs, and never accepts a raw shell
//! command or a caller-supplied cgroup command.

use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use devcore_build::{BuildAdapter, BuildStep, PlanError};
use devcore_core::{ResourceSnapshot, ScopePlanError, WorkloadClass};
use devcore_environment::{EnvironmentError, EnvironmentSpec};
use devcore_execution::{
    CoordinatorError, ExecutionLimits, ExecutionRequest, JobCoordinator, JobLogChunk, JobLogStream,
    JobStatus, JobStore,
};
use devcore_test::{TestCommand, TestKind, TestPlanError, TestStep};

/// Maximum UTF-8 text payload accepted by the Studio workspace API.
pub const MAX_WORKSPACE_TEXT_BYTES: usize = 2 * 1024 * 1024;
/// Maximum number of entries returned by one Studio directory listing.
pub const MAX_WORKSPACE_ENTRIES: usize = 256;

static NEXT_WORKSPACE_TEMP: AtomicU64 = AtomicU64::new(0);

/// Shared state used by the D-Bus interfaces in the service binary.
pub type SharedWorkState = Arc<Mutex<WorkState>>;

/// Typed Test Engine input carried by the user-session service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestSubmission {
    /// Stable step identifier.
    pub step_id: String,
    /// Test category identifier.
    pub kind: String,
    /// Direct executable name.
    pub program: String,
    /// Direct executable arguments.
    pub args: Vec<String>,
    /// Host project directory, mapped into `/workspace`.
    pub working_directory: PathBuf,
}

/// A bounded text file returned by the Studio workspace API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceTextFile {
    /// The normalized workspace-relative path.
    pub path: String,
    /// UTF-8 file contents.
    pub contents: String,
}

/// One bounded entry returned by the Studio workspace explorer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceEntry {
    /// The normalized workspace-relative path.
    pub path: String,
    /// Whether the entry itself is a directory.
    pub directory: bool,
    /// Whether the entry is a symbolic link. Links are never followed while
    /// listing; file reads and writes apply their own containment checks.
    pub symlink: bool,
    /// Byte size for regular files, or zero for other entry types.
    pub bytes: u64,
}

/// In-memory environment definitions plus the persistent job coordinator.
#[derive(Debug)]
pub struct WorkState {
    snapshot: ResourceSnapshot,
    environments: BTreeMap<String, EnvironmentSpec>,
    jobs: JobCoordinator,
}

impl WorkState {
    /// Creates user-session work state with private job persistence.
    #[must_use]
    pub fn new(snapshot: ResourceSnapshot, store: JobStore) -> Self {
        Self {
            snapshot,
            environments: BTreeMap::new(),
            jobs: JobCoordinator::with_store(store),
        }
    }

    /// Wraps the state for sharing across D-Bus interface objects.
    #[must_use]
    pub fn shared(self) -> SharedWorkState {
        Arc::new(Mutex::new(self))
    }

    /// Defines one reproducible rootless environment.
    pub fn define_environment(&mut self, environment: EnvironmentSpec) -> Result<(), WorkError> {
        if !environment.is_digest_pinned() {
            return Err(WorkError::InvalidValue(
                "environment image must be digest pinned".to_owned(),
            ));
        }
        if self.environments.contains_key(&environment.name) {
            return Err(WorkError::DuplicateEnvironment(environment.name));
        }
        self.environments
            .insert(environment.name.clone(), environment);
        Ok(())
    }

    /// Defines an environment when absent and reuses an identical definition.
    ///
    /// A changed image, workspace, toolchain set, or network policy is never
    /// silently accepted under an existing name.
    pub fn ensure_environment(&mut self, environment: EnvironmentSpec) -> Result<(), WorkError> {
        if let Some(existing) = self.environments.get(&environment.name) {
            if existing == &environment {
                return Ok(());
            }
            return Err(WorkError::DuplicateEnvironment(environment.name));
        }
        self.define_environment(environment)
    }

    /// Starts a defined environment through a Build-class resource scope.
    pub fn create_environment(
        &self,
        environment_name: &str,
        job_id: &str,
    ) -> Result<String, WorkError> {
        let environment = self.environment(environment_name)?;
        let request = environment.create_execution_request(job_id)?;
        self.start_request(request, WorkloadClass::Build)
    }

    /// Starts one direct command in a defined environment.
    pub fn exec_environment(
        &self,
        environment_name: &str,
        job_id: &str,
        working_directory: PathBuf,
        program: String,
        args: Vec<String>,
    ) -> Result<String, WorkError> {
        let environment = self.environment(environment_name)?;
        let request =
            environment.exec_execution_request(job_id, &working_directory, program, &args)?;
        self.start_request(request, WorkloadClass::Interactive)
    }

    /// Stops a defined environment.
    pub fn stop_environment(
        &self,
        environment_name: &str,
        job_id: &str,
    ) -> Result<String, WorkError> {
        let environment = self.environment(environment_name)?;
        let request = environment.stop_execution_request(job_id)?;
        self.start_request(request, WorkloadClass::Build)
    }

    /// Removes a defined environment after it has been stopped.
    pub fn remove_environment(
        &self,
        environment_name: &str,
        job_id: &str,
    ) -> Result<String, WorkError> {
        let environment = self.environment(environment_name)?;
        let request = environment.remove_execution_request(job_id)?;
        self.start_request(request, WorkloadClass::Build)
    }

    /// Starts a typed Build Engine adapter step.
    pub fn start_build(
        &self,
        environment_name: &str,
        job_id: &str,
        step_id: String,
        adapter: &str,
        working_directory: PathBuf,
    ) -> Result<String, WorkError> {
        let environment = self.environment(environment_name)?;
        let adapter = parse_build_adapter(adapter)?;
        let step = BuildStep::new(step_id, adapter, working_directory);
        let request = step.execution_request(job_id, &environment)?;
        self.start_request(request, WorkloadClass::Build)
    }

    /// Starts a typed Test Engine step.
    pub fn start_test(
        &self,
        environment_name: &str,
        job_id: &str,
        submission: TestSubmission,
    ) -> Result<String, WorkError> {
        let environment = self.environment(environment_name)?;
        let kind = parse_test_kind(&submission.kind)?;
        let step = TestStep::new(
            submission.step_id,
            kind,
            TestCommand::new(submission.program, submission.args),
            submission.working_directory,
        );
        let request = step.execution_request(job_id, &environment)?;
        self.start_request(request, kind.workload_class())
    }

    /// Starts a bounded, read-only Git status operation in a workspace.
    pub fn start_git_status(
        &self,
        environment_name: &str,
        job_id: &str,
        working_directory: PathBuf,
    ) -> Result<String, WorkError> {
        let args = vec![
            "status".to_owned(),
            "--short".to_owned(),
            "--branch".to_owned(),
        ];
        self.start_git_operation(environment_name, job_id, working_directory, args)
    }

    /// Starts a bounded, read-only Git diff operation in a workspace.
    pub fn start_git_diff(
        &self,
        environment_name: &str,
        job_id: &str,
        working_directory: PathBuf,
    ) -> Result<String, WorkError> {
        let args = vec![
            "diff".to_owned(),
            "--no-color".to_owned(),
            "--no-ext-diff".to_owned(),
        ];
        self.start_git_operation(environment_name, job_id, working_directory, args)
    }

    /// Starts a bounded, read-only Git history operation in a workspace.
    pub fn start_git_history(
        &self,
        environment_name: &str,
        job_id: &str,
        working_directory: PathBuf,
    ) -> Result<String, WorkError> {
        let args = vec![
            "log".to_owned(),
            "--oneline".to_owned(),
            "--decorate".to_owned(),
            "--no-color".to_owned(),
            "-n".to_owned(),
            "20".to_owned(),
        ];
        self.start_git_operation(environment_name, job_id, working_directory, args)
    }

    fn start_git_operation(
        &self,
        environment_name: &str,
        job_id: &str,
        working_directory: PathBuf,
        args: Vec<String>,
    ) -> Result<String, WorkError> {
        let environment = self.environment(environment_name)?;
        let request = environment.run_execution_request_at(
            job_id,
            &working_directory,
            "git".to_owned(),
            &args,
        )?;
        self.start_request(request, WorkloadClass::Interactive)
    }

    /// Requests cancellation for one registered job.
    pub fn cancel(&self, job_id: &str) -> Result<(), WorkError> {
        self.jobs.cancel(job_id).map_err(WorkError::Coordinator)
    }

    /// Returns one registered job status without exposing its logs.
    pub fn status(&self, job_id: &str) -> Result<JobStatus, WorkError> {
        self.jobs.status(job_id).map_err(WorkError::Coordinator)
    }

    /// Reads a bounded slice of one terminal job log stream.
    pub fn read_log(
        &self,
        job_id: &str,
        stream: &str,
        offset: u64,
        max_bytes: u64,
    ) -> Result<JobLogChunk, WorkError> {
        let stream = match stream {
            "stdout" => JobLogStream::Stdout,
            "stderr" => JobLogStream::Stderr,
            _ => {
                return Err(WorkError::InvalidValue(
                    "job log stream must be stdout or stderr".to_owned(),
                ));
            }
        };
        self.jobs
            .read_log(job_id, stream, offset, max_bytes)
            .map_err(WorkError::Coordinator)
    }

    /// Reads a bounded UTF-8 file below a registered environment workspace.
    pub fn read_workspace_file(
        &self,
        environment_name: &str,
        relative_path: &str,
    ) -> Result<WorkspaceTextFile, WorkError> {
        let environment = self.environment(environment_name)?;
        let (_, normalized, file_path) = resolve_existing_file(&environment, relative_path)?;
        let metadata = fs::metadata(&file_path).map_err(|error| workspace_io(&file_path, error))?;
        let file_length = usize::try_from(metadata.len()).map_err(|_| {
            WorkError::WorkspaceFile(format!(
                "workspace file {} is larger than the Studio limit",
                normalized.display()
            ))
        })?;
        if file_length > MAX_WORKSPACE_TEXT_BYTES {
            return Err(WorkError::WorkspaceFile(format!(
                "workspace file {} exceeds the {}-byte Studio limit",
                normalized.display(),
                MAX_WORKSPACE_TEXT_BYTES
            )));
        }

        let file = File::open(&file_path).map_err(|error| workspace_io(&file_path, error))?;
        let mut bytes = Vec::with_capacity(file_length);
        file.take((MAX_WORKSPACE_TEXT_BYTES as u64) + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| workspace_io(&file_path, error))?;
        if bytes.len() > MAX_WORKSPACE_TEXT_BYTES {
            return Err(WorkError::WorkspaceFile(format!(
                "workspace file {} exceeds the {}-byte Studio limit",
                normalized.display(),
                MAX_WORKSPACE_TEXT_BYTES
            )));
        }
        let contents = String::from_utf8(bytes).map_err(|_| {
            WorkError::WorkspaceFile(format!(
                "workspace file {} is not valid UTF-8 text",
                normalized.display()
            ))
        })?;

        Ok(WorkspaceTextFile {
            path: path_to_string(&normalized)?,
            contents,
        })
    }

    /// Atomically replaces or creates a bounded UTF-8 file below a registered
    /// environment workspace.
    pub fn write_workspace_file(
        &self,
        environment_name: &str,
        relative_path: &str,
        contents: &str,
    ) -> Result<WorkspaceTextFile, WorkError> {
        if contents.len() > MAX_WORKSPACE_TEXT_BYTES {
            return Err(WorkError::WorkspaceFile(format!(
                "workspace file content exceeds the {}-byte Studio limit",
                MAX_WORKSPACE_TEXT_BYTES
            )));
        }
        let environment = self.environment(environment_name)?;
        let (root, normalized) = resolve_relative_path(&environment, relative_path)?;
        let target = root.join(&normalized);
        let parent = target.parent().ok_or_else(|| {
            WorkError::WorkspaceFile(format!(
                "workspace file path {} has no parent",
                normalized.display()
            ))
        })?;
        let canonical_parent =
            fs::canonicalize(parent).map_err(|error| workspace_io(parent, error))?;
        ensure_within_root(&root, &canonical_parent, parent)?;

        let existing = match fs::symlink_metadata(&target) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(WorkError::WorkspaceFile(format!(
                        "refusing to write through workspace symlink {}",
                        normalized.display()
                    )));
                }
                if !metadata.file_type().is_file() {
                    return Err(WorkError::WorkspaceFile(format!(
                        "workspace path {} is not a regular file",
                        normalized.display()
                    )));
                }
                let canonical_target =
                    fs::canonicalize(&target).map_err(|error| workspace_io(&target, error))?;
                ensure_below_root(&root, &canonical_target, &target)?;
                Some(metadata)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(workspace_io(&target, error)),
        };

        let temporary_name = format!(
            ".{}.devcore-tmp-{}-{}",
            normalized
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("file"),
            std::process::id(),
            NEXT_WORKSPACE_TEMP.fetch_add(1, Ordering::Relaxed)
        );
        let temporary = canonical_parent.join(temporary_name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

            let mode = existing
                .as_ref()
                .map_or(0o600, |metadata| metadata.permissions().mode() & 0o7777);
            options.mode(mode);
        }
        let mut temporary_file = options
            .open(&temporary)
            .map_err(|error| workspace_io(&temporary, error))?;
        let write_result = (|| {
            temporary_file
                .write_all(contents.as_bytes())
                .map_err(|error| workspace_io(&temporary, error))?;
            temporary_file
                .sync_all()
                .map_err(|error| workspace_io(&temporary, error))?;
            drop(temporary_file);
            fs::rename(&temporary, &target).map_err(|error| workspace_io(&target, error))
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }

        Ok(WorkspaceTextFile {
            path: path_to_string(&normalized)?,
            contents: contents.to_owned(),
        })
    }

    /// Lists one bounded directory below a registered environment workspace.
    ///
    /// The empty path lists the workspace root. Directory entries are returned
    /// before files, then sorted by normalized path for stable UI output.
    pub fn list_workspace(
        &self,
        environment_name: &str,
        relative_directory: &str,
    ) -> Result<Vec<WorkspaceEntry>, WorkError> {
        let environment = self.environment(environment_name)?;
        let (_root, normalized, directory_path) =
            resolve_existing_directory(&environment, relative_directory)?;
        let mut entries = Vec::new();
        for entry in
            fs::read_dir(&directory_path).map_err(|error| workspace_io(&directory_path, error))?
        {
            if entries.len() >= MAX_WORKSPACE_ENTRIES {
                return Err(WorkError::WorkspaceFile(format!(
                    "workspace directory {} has more than {} entries",
                    normalized.display(),
                    MAX_WORKSPACE_ENTRIES
                )));
            }
            let entry = entry.map_err(|error| workspace_io(&directory_path, error))?;
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| {
                WorkError::WorkspaceFile(format!(
                    "workspace directory {} contains a non-UTF-8 entry",
                    normalized.display()
                ))
            })?;
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|error| workspace_io(&entry.path(), error))?;
            let path = normalized.join(name);
            entries.push(WorkspaceEntry {
                path: path_to_string(&path)?,
                directory: metadata.file_type().is_dir(),
                symlink: metadata.file_type().is_symlink(),
                bytes: if metadata.file_type().is_file() {
                    metadata.len()
                } else {
                    0
                },
            });
        }
        entries.sort_by(|left, right| {
            right
                .directory
                .cmp(&left.directory)
                .then_with(|| left.path.cmp(&right.path))
        });
        Ok(entries)
    }

    fn environment(&self, name: &str) -> Result<EnvironmentSpec, WorkError> {
        self.environments
            .get(name)
            .cloned()
            .ok_or_else(|| WorkError::UnknownEnvironment(name.to_owned()))
    }

    fn start_request(
        &self,
        request: ExecutionRequest,
        workload: WorkloadClass,
    ) -> Result<String, WorkError> {
        let scope = self
            .snapshot
            .workload_scope(&request.id, workload)
            .map_err(WorkError::Scope)?;
        let handle = self
            .jobs
            .start_in_scope(request, &scope, ExecutionLimits::default())
            .map_err(WorkError::Coordinator)?;
        Ok(handle.id())
    }
}

fn parse_build_adapter(value: &str) -> Result<BuildAdapter, WorkError> {
    match value {
        "cargo" => Ok(BuildAdapter::Cargo),
        "go" => Ok(BuildAdapter::Go),
        "cmake" => Ok(BuildAdapter::CMake),
        "gradle" => Ok(BuildAdapter::Gradle),
        "flutter" => Ok(BuildAdapter::Flutter),
        "qt" => Ok(BuildAdapter::Qt),
        "npm" => Ok(BuildAdapter::Npm),
        "pnpm" => Ok(BuildAdapter::Pnpm),
        _ => Err(WorkError::InvalidValue(format!(
            "unknown build adapter {value:?}"
        ))),
    }
}

fn parse_test_kind(value: &str) -> Result<TestKind, WorkError> {
    match value {
        "static-analysis" => Ok(TestKind::StaticAnalysis),
        "unit" => Ok(TestKind::Unit),
        "integration" => Ok(TestKind::Integration),
        "ui" => Ok(TestKind::Ui),
        "api" => Ok(TestKind::Api),
        "database" => Ok(TestKind::Database),
        "device" => Ok(TestKind::Device),
        "emulator" => Ok(TestKind::Emulator),
        "performance" => Ok(TestKind::Performance),
        "memory-pressure" => Ok(TestKind::MemoryPressure),
        "security" => Ok(TestKind::Security),
        "regression" => Ok(TestKind::Regression),
        _ => Err(WorkError::InvalidValue(format!(
            "unknown test kind {value:?}"
        ))),
    }
}

/// Failure returned by the user-session work service.
#[derive(Debug)]
pub enum WorkError {
    InvalidValue(String),
    DuplicateEnvironment(String),
    UnknownEnvironment(String),
    Environment(EnvironmentError),
    Build(PlanError),
    Test(TestPlanError),
    Scope(ScopePlanError),
    Coordinator(CoordinatorError),
    WorkspaceFile(String),
}

impl fmt::Display for WorkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidValue(value) => formatter.write_str(value),
            Self::DuplicateEnvironment(name) => {
                write!(formatter, "environment {name:?} is already defined")
            }
            Self::UnknownEnvironment(name) => write!(formatter, "unknown environment {name:?}"),
            Self::Environment(error) => error.fmt(formatter),
            Self::Build(error) => error.fmt(formatter),
            Self::Test(error) => error.fmt(formatter),
            Self::Scope(error) => error.fmt(formatter),
            Self::Coordinator(error) => error.fmt(formatter),
            Self::WorkspaceFile(error) => formatter.write_str(error),
        }
    }
}

impl Error for WorkError {}

impl From<EnvironmentError> for WorkError {
    fn from(error: EnvironmentError) -> Self {
        Self::Environment(error)
    }
}

impl From<PlanError> for WorkError {
    fn from(error: PlanError) -> Self {
        Self::Build(error)
    }
}

impl From<TestPlanError> for WorkError {
    fn from(error: TestPlanError) -> Self {
        Self::Test(error)
    }
}

fn resolve_existing_file(
    environment: &EnvironmentSpec,
    relative_path: &str,
) -> Result<(PathBuf, PathBuf, PathBuf), WorkError> {
    let (root, normalized) = resolve_relative_path(environment, relative_path)?;
    let candidate = root.join(&normalized);
    let canonical =
        fs::canonicalize(&candidate).map_err(|error| workspace_io(&candidate, error))?;
    ensure_below_root(&root, &canonical, &candidate)?;
    let metadata = fs::metadata(&canonical).map_err(|error| workspace_io(&canonical, error))?;
    if !metadata.is_file() {
        return Err(WorkError::WorkspaceFile(format!(
            "workspace path {} is not a regular file",
            normalized.display()
        )));
    }
    Ok((root, normalized, canonical))
}

fn resolve_existing_directory(
    environment: &EnvironmentSpec,
    relative_directory: &str,
) -> Result<(PathBuf, PathBuf, PathBuf), WorkError> {
    let (root, normalized) = resolve_relative_components(environment, relative_directory, true)?;
    let candidate = root.join(&normalized);
    let canonical =
        fs::canonicalize(&candidate).map_err(|error| workspace_io(&candidate, error))?;
    ensure_within_root(&root, &canonical, &candidate)?;
    if !canonical.is_dir() {
        return Err(WorkError::WorkspaceFile(format!(
            "workspace path {} is not a directory",
            normalized.display()
        )));
    }
    Ok((root, normalized, canonical))
}

fn resolve_relative_path(
    environment: &EnvironmentSpec,
    relative_path: &str,
) -> Result<(PathBuf, PathBuf), WorkError> {
    resolve_relative_components(environment, relative_path, false)
}

fn resolve_relative_components(
    environment: &EnvironmentSpec,
    relative_path: &str,
    allow_empty: bool,
) -> Result<(PathBuf, PathBuf), WorkError> {
    if !allow_empty && relative_path.trim().is_empty() {
        return Err(WorkError::WorkspaceFile(
            "workspace file path cannot be empty".to_owned(),
        ));
    }
    let root = fs::canonicalize(&environment.workspace)
        .map_err(|error| workspace_io(&environment.workspace, error))?;
    if !root.is_dir() {
        return Err(WorkError::WorkspaceFile(format!(
            "environment workspace {} is not a directory",
            environment.workspace.display()
        )));
    }
    let path = Path::new(relative_path);
    if path.is_absolute() {
        return Err(WorkError::WorkspaceFile(
            "workspace file path must be relative".to_owned(),
        ));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(WorkError::WorkspaceFile(
                    "workspace file path must not contain '.', '..', or a root".to_owned(),
                ));
            }
        }
    }
    if !allow_empty && normalized.as_os_str().is_empty() {
        return Err(WorkError::WorkspaceFile(
            "workspace file path cannot be empty".to_owned(),
        ));
    }
    Ok((root, normalized))
}

fn ensure_within_root(root: &Path, path: &Path, requested: &Path) -> Result<(), WorkError> {
    if path != root && path.strip_prefix(root).is_err() {
        return Err(WorkError::WorkspaceFile(format!(
            "workspace path {} resolves outside the registered workspace",
            requested.display()
        )));
    }
    Ok(())
}

fn ensure_below_root(root: &Path, path: &Path, requested: &Path) -> Result<(), WorkError> {
    if path == root || path.strip_prefix(root).is_err() {
        return Err(WorkError::WorkspaceFile(format!(
            "workspace path {} resolves outside the registered workspace",
            requested.display()
        )));
    }
    Ok(())
}

fn path_to_string(path: &Path) -> Result<String, WorkError> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        WorkError::WorkspaceFile("workspace file path is not valid UTF-8".to_owned())
    })
}

fn workspace_io(path: &Path, error: std::io::Error) -> WorkError {
    WorkError::WorkspaceFile(format!(
        "workspace file operation on {} failed: {error}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicUsize, Ordering},
    };

    use devcore_core::{HardwareCapacity, MachineProfile, PowerState};
    use devcore_environment::{NetworkPolicy, Toolchain};
    use devcore_execution::{CoordinatorError, JobStore};

    use super::{
        MAX_WORKSPACE_ENTRIES, MAX_WORKSPACE_TEXT_BYTES, TestSubmission, WorkError, WorkState,
    };

    static NEXT_STORE: AtomicUsize = AtomicUsize::new(0);

    fn state(profile: MachineProfile) -> WorkState {
        let root = std::env::temp_dir().join(format!(
            "devcore-workd-test-{}-{}-{}",
            std::process::id(),
            profile.as_str(),
            NEXT_STORE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        WorkState::new(
            devcore_core::ResourceSnapshot {
                capacity: HardwareCapacity::new(2, 4 * 1024 * 1024),
                initial_profile: profile,
                memory_available_kib: Some(2 * 1024 * 1024),
                cpu_pressure: None,
                memory_pressure: None,
                io_pressure: None,
                power: PowerState::default(),
            },
            JobStore::open(root).unwrap(),
        )
    }

    fn environment() -> devcore_environment::EnvironmentSpec {
        devcore_environment::EnvironmentSpec::new(
            "devcore-rust",
            "quay.io/devcore/rust@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            PathBuf::from("/workspaces/devcore"),
            vec![Toolchain::Rust],
            NetworkPolicy::Disabled,
        )
        .unwrap()
    }

    fn workspace_environment(root: &Path) -> devcore_environment::EnvironmentSpec {
        devcore_environment::EnvironmentSpec::new(
            "devcore-rust",
            "quay.io/devcore/rust@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            root.to_path_buf(),
            vec![Toolchain::Rust],
            NetworkPolicy::Disabled,
        )
        .unwrap()
    }

    fn workspace_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "devcore-workd-workspace-{}-{}-{}",
            std::process::id(),
            label,
            NEXT_STORE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        root
    }

    #[test]
    fn only_digest_pinned_environments_can_enter_service_state() {
        let mut work = state(MachineProfile::Low);
        work.define_environment(environment()).unwrap();
        assert!(matches!(
            work.define_environment(environment()),
            Err(WorkError::DuplicateEnvironment(name)) if name == "devcore-rust"
        ));

        let mut tagged = environment();
        tagged.image = "quay.io/devcore/rust:latest".to_owned();
        assert!(matches!(
            work.define_environment(tagged),
            Err(WorkError::InvalidValue(message)) if message.contains("digest pinned")
        ));
    }

    #[test]
    fn ensure_reuses_only_an_identical_environment_definition() {
        let mut work = state(MachineProfile::Balanced);
        let original = environment();
        work.ensure_environment(original.clone()).unwrap();
        work.ensure_environment(original).unwrap();

        let mut changed = environment();
        changed.network = NetworkPolicy::Enabled;
        assert!(matches!(
            work.ensure_environment(changed),
            Err(WorkError::DuplicateEnvironment(name)) if name == "devcore-rust"
        ));
    }

    #[test]
    fn outside_workspace_exec_is_rejected_before_a_job_is_registered() {
        let mut work = state(MachineProfile::Low);
        work.define_environment(environment()).unwrap();

        let result = work.exec_environment(
            "devcore-rust",
            "outside-job",
            PathBuf::from("/workspaces/other"),
            "cargo".to_owned(),
            vec!["test".to_owned()],
        );

        assert!(matches!(result, Err(WorkError::Environment(_))));
        assert!(matches!(
            work.status("outside-job"),
            Err(WorkError::Coordinator(CoordinatorError::UnknownJob(id))) if id == "outside-job"
        ));
    }

    #[test]
    fn outside_workspace_git_status_is_rejected_before_a_job_is_registered() {
        let mut work = state(MachineProfile::Low);
        work.define_environment(environment()).unwrap();

        let result = work.start_git_status(
            "devcore-rust",
            "outside-git-job",
            PathBuf::from("/workspaces/other"),
        );

        assert!(matches!(result, Err(WorkError::Environment(_))));
        assert!(matches!(
            work.status("outside-git-job"),
            Err(WorkError::Coordinator(CoordinatorError::UnknownJob(id))) if id == "outside-git-job"
        ));
    }

    #[test]
    fn outside_workspace_git_read_models_are_rejected_before_a_job_is_registered() {
        let mut work = state(MachineProfile::Low);
        work.define_environment(environment()).unwrap();

        let diff_result = work.start_git_diff(
            "devcore-rust",
            "outside-diff-job",
            PathBuf::from("/workspaces/other"),
        );
        let history_result = work.start_git_history(
            "devcore-rust",
            "outside-history-job",
            PathBuf::from("/workspaces/other"),
        );

        assert!(matches!(diff_result, Err(WorkError::Environment(_))));
        assert!(matches!(history_result, Err(WorkError::Environment(_))));
        assert!(matches!(
            work.status("outside-diff-job"),
            Err(WorkError::Coordinator(CoordinatorError::UnknownJob(id))) if id == "outside-diff-job"
        ));
        assert!(matches!(
            work.status("outside-history-job"),
            Err(WorkError::Coordinator(CoordinatorError::UnknownJob(id))) if id == "outside-history-job"
        ));
    }

    #[test]
    fn low_profile_emulator_is_deferred_before_process_creation() {
        let mut work = state(MachineProfile::Low);
        work.define_environment(environment()).unwrap();

        let result = work.start_test(
            "devcore-rust",
            "emulator-job",
            TestSubmission {
                step_id: "android".to_owned(),
                kind: "emulator".to_owned(),
                program: "printf".to_owned(),
                args: vec!["not-started".to_owned()],
                working_directory: PathBuf::from("/workspaces/devcore"),
            },
        );

        assert!(matches!(
            result,
            Err(WorkError::Coordinator(CoordinatorError::Deferred(id))) if id == "emulator-job"
        ));
        assert!(matches!(
            work.status("emulator-job"),
            Err(WorkError::Coordinator(CoordinatorError::UnknownJob(id))) if id == "emulator-job"
        ));
    }

    #[test]
    fn typed_test_kind_uses_the_shared_workload_policy() {
        let mut work = state(MachineProfile::Low);
        work.define_environment(environment()).unwrap();

        let result = work.start_test(
            "devcore-rust",
            "bad-kind-job",
            TestSubmission {
                step_id: "step".to_owned(),
                kind: "not-a-test-kind".to_owned(),
                program: "printf".to_owned(),
                args: vec!["not-started".to_owned()],
                working_directory: PathBuf::from("/workspaces/devcore"),
            },
        );

        assert!(
            matches!(result, Err(WorkError::InvalidValue(message)) if message.contains("test kind"))
        );
        assert_eq!(
            work.status("bad-kind-job").unwrap_err().to_string(),
            "unknown job \"bad-kind-job\""
        );
    }

    #[test]
    fn studio_workspace_reads_and_atomically_replaces_utf8_text() {
        let root = workspace_root("roundtrip");
        fs::write(root.join("README.md"), "before\n").unwrap();
        let mut work = state(MachineProfile::Balanced);
        work.define_environment(workspace_environment(&root))
            .unwrap();

        let original = work
            .read_workspace_file("devcore-rust", "README.md")
            .unwrap();
        assert_eq!(original.path, "README.md");
        assert_eq!(original.contents, "before\n");

        let saved = work
            .write_workspace_file("devcore-rust", "README.md", "after\n✓\n")
            .unwrap();
        assert_eq!(saved.path, "README.md");
        assert_eq!(saved.contents, "after\n✓\n");
        assert_eq!(
            fs::read_to_string(root.join("README.md")).unwrap(),
            "after\n✓\n"
        );
        assert!(
            !fs::read_dir(&root)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .any(|name| name.to_string_lossy().contains("devcore-tmp"))
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn studio_workspace_lists_directories_before_bounded_file_entries() {
        let root = workspace_root("listing");
        fs::write(root.join("README.md"), "safe\n").unwrap();
        fs::write(root.join("src/lib.rs"), "fn main() {}\n").unwrap();
        let mut work = state(MachineProfile::Balanced);
        work.define_environment(workspace_environment(&root))
            .unwrap();

        let entries = work.list_workspace("devcore-rust", "").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "src");
        assert!(entries[0].directory);
        assert_eq!(entries[1].path, "README.md");
        assert!(!entries[1].directory);
        assert_eq!(entries[1].bytes, 5);

        let nested = work.list_workspace("devcore-rust", "src").unwrap();
        assert_eq!(nested[0].path, "src/lib.rs");

        fs::create_dir_all(root.join("many")).unwrap();
        for index in 0..=MAX_WORKSPACE_ENTRIES {
            fs::write(root.join("many").join(format!("file-{index}.txt")), "x").unwrap();
        }
        assert!(matches!(
            work.list_workspace("devcore-rust", "many"),
            Err(WorkError::WorkspaceFile(message)) if message.contains("more than")
        ));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn studio_workspace_rejects_traversal_absolute_and_oversized_paths() {
        let root = workspace_root("boundaries");
        fs::write(root.join("README.md"), "safe\n").unwrap();
        let mut work = state(MachineProfile::Low);
        work.define_environment(workspace_environment(&root))
            .unwrap();

        for path in ["../README.md", "./README.md", "/etc/passwd"] {
            assert!(matches!(
                work.read_workspace_file("devcore-rust", path),
                Err(WorkError::WorkspaceFile(_))
            ));
        }
        for directory in ["..", "./src", "/tmp"] {
            assert!(matches!(
                work.list_workspace("devcore-rust", directory),
                Err(WorkError::WorkspaceFile(_))
            ));
        }
        assert!(matches!(
            work.write_workspace_file(
                "devcore-rust",
                "src/too-large.txt",
                &"x".repeat(MAX_WORKSPACE_TEXT_BYTES + 1),
            ),
            Err(WorkError::WorkspaceFile(message)) if message.contains("Studio limit")
        ));

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn studio_workspace_does_not_follow_symlinks_outside_the_workspace() {
        use std::os::unix::fs::symlink;

        let root = workspace_root("symlink");
        let outside = workspace_root("outside");
        fs::write(outside.join("secret.txt"), "private\n").unwrap();
        symlink(outside.join("secret.txt"), root.join("link.txt")).unwrap();
        symlink(&outside, root.join("external")).unwrap();

        let mut work = state(MachineProfile::Low);
        work.define_environment(workspace_environment(&root))
            .unwrap();
        for path in ["link.txt", "external/new.txt"] {
            assert!(matches!(
                work.read_workspace_file("devcore-rust", path),
                Err(WorkError::WorkspaceFile(_))
            ));
            assert!(matches!(
                work.write_workspace_file("devcore-rust", path, "blocked\n"),
                Err(WorkError::WorkspaceFile(_))
            ));
        }
        assert_eq!(
            fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "private\n"
        );

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
}
