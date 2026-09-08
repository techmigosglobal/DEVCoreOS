#![forbid(unsafe_code)]
//! Build-plan domain types for DevCore's isolated build engine.
//!
//! This crate plans work but does not execute host commands. Execution belongs
//! behind the OCI environment boundary, where the user-session Work service can apply
//! resource budgets, cancellation, logging, and artifact tracking safely.

use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    fs::{self, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use devcore_core::{ResourceSnapshot, ScopePlanError, WorkloadClass, WorkloadScopePlan};
use devcore_environment::EnvironmentSpec;
use devcore_execution::ExecutionRequest;
use sha2::{Digest, Sha256};

/// Existing build systems DevCore can address without replacing their tools.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum BuildAdapter {
    /// Rust projects using Cargo.
    Cargo,
    /// Go projects using the Go command.
    Go,
    /// CMake-generated native projects.
    CMake,
    /// Gradle projects, including Android projects.
    Gradle,
    /// Flutter projects.
    Flutter,
    /// Qt projects using a CMake build directory.
    Qt,
    /// Node projects using npm scripts.
    Npm,
    /// Node projects using pnpm scripts.
    Pnpm,
}

impl BuildAdapter {
    /// Returns the stable adapter identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Go => "go",
            Self::CMake => "cmake",
            Self::Gradle => "gradle",
            Self::Flutter => "flutter",
            Self::Qt => "qt",
            Self::Npm => "npm",
            Self::Pnpm => "pnpm",
        }
    }

    /// Returns a shell-free default command for this adapter.
    #[must_use]
    pub fn default_command(self) -> CommandSpec {
        match self {
            Self::Cargo => CommandSpec::new("cargo", &["build"]),
            Self::Go => CommandSpec::new("go", &["build", "./..."]),
            Self::CMake | Self::Qt => CommandSpec::new("cmake", &["--build", "build"]),
            Self::Gradle => CommandSpec::new("./gradlew", &["assemble"]),
            Self::Flutter => CommandSpec::new("flutter", &["build"]),
            Self::Npm => CommandSpec::new("npm", &["run", "build"]),
            Self::Pnpm => CommandSpec::new("pnpm", &["run", "build"]),
        }
    }
}

/// The reproducible inputs used to address one build result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildCacheDescriptor {
    environment_image: String,
    source_fingerprint: String,
    step_id: String,
    adapter: BuildAdapter,
    argv: Vec<String>,
    dependencies: Vec<String>,
}

impl BuildCacheDescriptor {
    /// Creates a cache descriptor from a digest-pinned environment and step.
    pub fn from_step(
        environment: &EnvironmentSpec,
        step: &BuildStep,
        source_fingerprint: impl Into<String>,
    ) -> Result<Self, PlanError> {
        environment
            .validate()
            .map_err(PlanError::InvalidEnvironment)?;
        if !environment.is_digest_pinned() {
            return Err(PlanError::UnpinnedEnvironment(environment.image.clone()));
        }
        let source_fingerprint = source_fingerprint.into();
        if source_fingerprint.trim().is_empty() || source_fingerprint.contains('\0') {
            return Err(PlanError::InvalidCacheFingerprint(source_fingerprint));
        }
        let command = step.command();
        let mut dependencies = step.dependencies.clone();
        dependencies.sort();
        dependencies.dedup();
        let mut argv = Vec::with_capacity(command.args.len() + 1);
        argv.push(command.program.to_owned());
        argv.extend(command.args.into_iter().map(str::to_owned));

        Ok(Self {
            environment_image: environment.image.clone(),
            source_fingerprint,
            step_id: step.id.clone(),
            adapter: step.adapter,
            argv,
            dependencies,
        })
    }

    /// Returns the lowercase SHA-256 content address for this descriptor.
    #[must_use]
    pub fn key(&self) -> BuildCacheKey {
        let mut hasher = Sha256::new();
        hasher.update(b"devcore-build-cache-v1");
        for value in [
            self.environment_image.as_str(),
            self.source_fingerprint.as_str(),
            self.step_id.as_str(),
            self.adapter.as_str(),
        ] {
            hash_field(&mut hasher, value);
        }
        hash_fields(&mut hasher, &self.argv);
        hash_fields(&mut hasher, &self.dependencies);
        BuildCacheKey(hex_digest(&hasher.finalize()))
    }
}

fn hash_field(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

fn hash_fields(hasher: &mut Sha256, values: &[String]) {
    hasher.update((values.len() as u64).to_be_bytes());
    for value in values {
        hash_field(hasher, value);
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut digest = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut digest, "{byte:02x}").expect("writing to a String cannot fail");
    }
    digest
}

/// A validated lowercase SHA-256 build-result address.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BuildCacheKey(String);

impl BuildCacheKey {
    /// Returns the stable hexadecimal key text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BuildCacheKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A filesystem-scoped, atomically published build-result cache.
#[derive(Clone, Debug)]
pub struct BuildCacheStore {
    root: PathBuf,
}

impl BuildCacheStore {
    /// Opens a cache directory, creating only that directory if absent.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, CacheStoreError> {
        let root = root.into();
        if root.as_os_str().is_empty() {
            return Err(CacheStoreError::EmptyRoot);
        }
        fs::create_dir_all(&root).map_err(|source| CacheStoreError::Io {
            operation: "create cache root",
            path: root.clone(),
            source,
        })?;
        Ok(Self { root })
    }

    /// Returns the content-addressed path for a cache key.
    #[must_use]
    pub fn artifact_path(&self, key: &BuildCacheKey) -> PathBuf {
        self.root.join(key.as_str())
    }

    /// Reports whether a complete regular-file artifact is present.
    pub fn contains(&self, key: &BuildCacheKey) -> Result<bool, CacheStoreError> {
        let path = self.artifact_path(key);
        match fs::metadata(&path) {
            Ok(metadata) => Ok(metadata.is_file()),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(CacheStoreError::Io {
                operation: "inspect cache artifact",
                path,
                source,
            }),
        }
    }

    /// Publishes one regular file under its content address without overwriting it.
    pub fn publish_file(
        &self,
        key: &BuildCacheKey,
        source: &Path,
    ) -> Result<PathBuf, CacheStoreError> {
        let source_metadata = fs::metadata(source).map_err(|source_error| CacheStoreError::Io {
            operation: "inspect artifact source",
            path: source.to_path_buf(),
            source: source_error,
        })?;
        if !source_metadata.is_file() {
            return Err(CacheStoreError::InvalidSource(source.to_path_buf()));
        }

        let destination = self.artifact_path(key);
        match fs::metadata(&destination) {
            Ok(metadata) if metadata.is_file() => return Ok(destination),
            Ok(_) => return Err(CacheStoreError::InvalidDestination(destination)),
            Err(source_error) if source_error.kind() == io::ErrorKind::NotFound => {}
            Err(source_error) => {
                return Err(CacheStoreError::Io {
                    operation: "inspect cache destination",
                    path: destination,
                    source: source_error,
                });
            }
        }

        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
        let temporary = self.root.join(format!(
            ".{}.{}.{}.tmp",
            key.as_str(),
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut temporary_file =
            options
                .open(&temporary)
                .map_err(|source_error| CacheStoreError::Io {
                    operation: "create temporary cache artifact",
                    path: temporary.clone(),
                    source: source_error,
                })?;
        let mut source_file = match fs::File::open(source) {
            Ok(file) => file,
            Err(source_error) => {
                let _ = fs::remove_file(&temporary);
                return Err(CacheStoreError::Io {
                    operation: "open artifact source",
                    path: source.to_path_buf(),
                    source: source_error,
                });
            }
        };
        let copy_result = io::copy(&mut source_file, &mut temporary_file);
        if let Err(source_error) = copy_result {
            let _ = fs::remove_file(&temporary);
            return Err(CacheStoreError::Io {
                operation: "copy artifact into cache",
                path: source.to_path_buf(),
                source: source_error,
            });
        }
        if let Err(source_error) = temporary_file.sync_all() {
            drop(temporary_file);
            let _ = fs::remove_file(&temporary);
            return Err(CacheStoreError::Io {
                operation: "sync temporary cache artifact",
                path: temporary,
                source: source_error,
            });
        }
        drop(temporary_file);
        match fs::rename(&temporary, &destination) {
            Ok(()) => Ok(destination),
            Err(source_error) if source_error.kind() == io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&temporary);
                Ok(destination)
            }
            Err(source_error) => {
                let _ = fs::remove_file(&temporary);
                Err(CacheStoreError::Io {
                    operation: "publish cache artifact",
                    path: destination,
                    source: source_error,
                })
            }
        }
    }
}

/// Failure while opening or publishing a build-cache artifact.
#[derive(Debug)]
pub enum CacheStoreError {
    /// The cache root was empty.
    EmptyRoot,
    /// The source was not a regular file.
    InvalidSource(PathBuf),
    /// A cache key path was occupied by a non-file.
    InvalidDestination(PathBuf),
    /// A filesystem operation failed.
    Io {
        /// Operation being performed.
        operation: &'static str,
        /// Path involved in the operation.
        path: PathBuf,
        /// Underlying filesystem failure.
        source: io::Error,
    },
}

impl fmt::Display for CacheStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRoot => formatter.write_str("build cache root is empty"),
            Self::InvalidSource(path) => {
                write!(
                    formatter,
                    "build cache source is not a regular file: {}",
                    path.display()
                )
            }
            Self::InvalidDestination(path) => write!(
                formatter,
                "build cache destination is not a regular file: {}",
                path.display()
            ),
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "{operation} {}: {source}", path.display()),
        }
    }
}

impl Error for CacheStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::EmptyRoot | Self::InvalidSource(_) | Self::InvalidDestination(_) => None,
        }
    }
}

/// An executable plus fixed arguments; no shell string is constructed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    /// Program name resolved inside the selected environment.
    pub program: &'static str,
    /// Fixed arguments passed directly to the program.
    pub args: Vec<&'static str>,
}

impl CommandSpec {
    fn new(program: &'static str, args: &'static [&'static str]) -> Self {
        Self {
            program,
            args: args.to_vec(),
        }
    }
}

/// One node in a build dependency DAG.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildStep {
    /// Stable step identifier within a plan.
    pub id: String,
    /// Adapter responsible for this step.
    pub adapter: BuildAdapter,
    /// Project directory passed as the command working directory.
    pub working_directory: PathBuf,
    /// Steps that must complete before this step starts.
    pub dependencies: Vec<String>,
}

impl BuildStep {
    /// Creates a build step with no dependencies.
    #[must_use]
    pub fn new(id: impl Into<String>, adapter: BuildAdapter, working_directory: PathBuf) -> Self {
        Self {
            id: id.into(),
            adapter,
            working_directory,
            dependencies: Vec::new(),
        }
    }

    /// Adds a dependency and returns the updated step.
    #[must_use]
    pub fn depends_on(mut self, dependency: impl Into<String>) -> Self {
        self.dependencies.push(dependency.into());
        self
    }

    /// Returns the adapter's direct command specification.
    #[must_use]
    pub fn command(&self) -> CommandSpec {
        self.adapter.default_command()
    }

    /// Maps this step into a direct command inside a validated environment.
    pub fn environment_command(
        &self,
        environment: &EnvironmentSpec,
    ) -> Result<Vec<String>, PlanError> {
        environment
            .validate()
            .map_err(PlanError::InvalidEnvironment)?;
        let command = self.command();
        let args = command
            .args
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect::<Vec<_>>();
        environment
            .run_command_at(&self.working_directory, command.program, &args)
            .map_err(PlanError::InvalidEnvironment)
    }

    /// Builds an executor request from this step and a validated environment.
    pub fn execution_request(
        &self,
        job_id: impl Into<String>,
        environment: &EnvironmentSpec,
    ) -> Result<ExecutionRequest, PlanError> {
        Ok(ExecutionRequest::from_argv(
            job_id,
            self.environment_command(environment)?,
        ))
    }

    /// Wraps this step in a validated Build workload scope.
    pub fn execution_request_with_scope(
        &self,
        job_id: impl Into<String>,
        environment: &EnvironmentSpec,
        scope: &WorkloadScopePlan,
    ) -> Result<ExecutionRequest, PlanError> {
        if scope.workload != WorkloadClass::Build {
            return Err(PlanError::WrongWorkload {
                expected: WorkloadClass::Build,
                actual: scope.workload,
            });
        }
        let request = self.execution_request(job_id, environment)?;
        let argv = scope
            .systemd_run_argv(&request.argv)
            .map_err(PlanError::InvalidScope)?;
        Ok(ExecutionRequest::from_argv(request.id, argv))
    }
}

/// A validated collection of build steps.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BuildPlan {
    steps: Vec<BuildStep>,
}

impl BuildPlan {
    /// Creates an empty plan.
    #[must_use]
    pub const fn new() -> Self {
        Self { steps: Vec::new() }
    }

    /// Adds one step to the plan.
    pub fn push(&mut self, step: BuildStep) {
        self.steps.push(step);
    }

    /// Returns the plan's steps in insertion order.
    #[must_use]
    pub fn steps(&self) -> &[BuildStep] {
        &self.steps
    }

    /// Validates dependencies and returns a deterministic topological order.
    pub fn execution_order(&self) -> Result<Vec<String>, PlanError> {
        let mut steps = BTreeMap::new();
        for step in &self.steps {
            if steps.insert(step.id.clone(), step).is_some() {
                return Err(PlanError::DuplicateStep(step.id.clone()));
            }
        }

        for step in &self.steps {
            for dependency in &step.dependencies {
                if !steps.contains_key(dependency.as_str()) {
                    return Err(PlanError::UnknownDependency {
                        step: step.id.clone(),
                        dependency: dependency.clone(),
                    });
                }
            }
        }

        let mut state = BTreeMap::new();
        let mut order = Vec::with_capacity(self.steps.len());
        for step in &self.steps {
            visit(step.id.as_str(), &steps, &mut state, &mut order)?;
        }
        Ok(order)
    }

    /// Returns the current safe build worker cap from `devcored` policy.
    #[must_use]
    pub fn recommended_workers(&self, snapshot: &ResourceSnapshot) -> u16 {
        snapshot.workload_budget(WorkloadClass::Build).max_workers
    }
}

fn visit(
    id: &str,
    steps: &BTreeMap<String, &BuildStep>,
    state: &mut BTreeMap<String, VisitState>,
    order: &mut Vec<String>,
) -> Result<(), PlanError> {
    match state.get(id) {
        Some(VisitState::Done) => return Ok(()),
        Some(VisitState::Visiting) => return Err(PlanError::Cycle(id.to_owned())),
        None => {}
    }

    state.insert(id.to_owned(), VisitState::Visiting);
    let step = steps
        .get(id)
        .expect("dependencies were checked before traversal");
    let mut dependencies = step.dependencies.clone();
    dependencies.sort();
    dependencies.dedup();
    for dependency in dependencies {
        visit(&dependency, steps, state, order)?;
    }
    state.insert(id.to_owned(), VisitState::Done);
    order.push(id.to_owned());
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VisitState {
    Visiting,
    Done,
}

/// A build-plan validation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlanError {
    /// Two steps used the same identifier.
    DuplicateStep(String),
    /// A step refers to an identifier not present in the plan.
    UnknownDependency {
        /// Step containing the invalid dependency.
        step: String,
        /// Missing dependency identifier.
        dependency: String,
    },
    /// A dependency cycle prevents a valid execution order.
    Cycle(String),
    /// The environment definition or working directory is invalid.
    InvalidEnvironment(devcore_environment::EnvironmentError),
    /// A cache descriptor cannot use an unpinned environment image.
    UnpinnedEnvironment(String),
    /// A cache descriptor needs a non-empty source fingerprint.
    InvalidCacheFingerprint(String),
    /// The scope was created for a different workload class.
    WrongWorkload {
        /// Workload required by this build step.
        expected: WorkloadClass,
        /// Workload carried by the supplied scope.
        actual: WorkloadClass,
    },
    /// The scope command could not be represented safely.
    InvalidScope(ScopePlanError),
}

impl fmt::Display for PlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateStep(id) => write!(formatter, "duplicate build step {id:?}"),
            Self::UnknownDependency { step, dependency } => write!(
                formatter,
                "build step {step:?} depends on missing step {dependency:?}"
            ),
            Self::Cycle(id) => write!(formatter, "build dependency cycle includes {id:?}"),
            Self::InvalidEnvironment(error) => {
                write!(formatter, "invalid build environment: {error}")
            }
            Self::UnpinnedEnvironment(image) => {
                write!(
                    formatter,
                    "build cache environment image is not digest pinned: {image:?}"
                )
            }
            Self::InvalidCacheFingerprint(fingerprint) => write!(
                formatter,
                "invalid build cache source fingerprint {fingerprint:?}"
            ),
            Self::WrongWorkload { expected, actual } => write!(
                formatter,
                "build scope workload is {}, expected {}",
                actual.as_str(),
                expected.as_str()
            ),
            Self::InvalidScope(error) => write!(formatter, "invalid build scope: {error}"),
        }
    }
}

impl Error for PlanError {}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use devcore_core::{
        HardwareCapacity, MachineProfile, PowerState, ResourceSnapshot, WorkloadClass,
    };
    use devcore_environment::{EnvironmentSpec, NetworkPolicy, Toolchain};

    use super::{
        BuildAdapter, BuildCacheDescriptor, BuildCacheStore, BuildPlan, BuildStep, PlanError,
    };

    fn digest_image() -> &'static str {
        "quay.io/devcore/rust@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    }

    fn rust_environment() -> EnvironmentSpec {
        EnvironmentSpec::new(
            "devcore-rust",
            digest_image(),
            PathBuf::from("/workspaces/devcore"),
            vec![Toolchain::Rust],
            NetworkPolicy::Disabled,
        )
        .unwrap()
    }

    fn low_snapshot() -> ResourceSnapshot {
        ResourceSnapshot {
            capacity: HardwareCapacity::new(2, 4 * 1024 * 1024),
            initial_profile: MachineProfile::Low,
            memory_available_kib: Some(2 * 1024 * 1024),
            cpu_pressure: None,
            memory_pressure: None,
            io_pressure: None,
            power: PowerState::default(),
        }
    }

    #[test]
    fn creates_shell_free_adapter_commands() {
        let command = BuildAdapter::Cargo.default_command();

        assert_eq!(command.program, "cargo");
        assert_eq!(command.args, vec!["build"]);
    }

    #[test]
    fn maps_build_step_into_environment_without_host_path() {
        let environment = EnvironmentSpec::new(
            "devcore-rust",
            "quay.io/devcore/rust@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            PathBuf::from("/workspaces/devcore"),
            vec![Toolchain::Rust],
            NetworkPolicy::Disabled,
        )
        .unwrap();
        let step = BuildStep::new(
            "compile",
            BuildAdapter::Cargo,
            PathBuf::from("/workspaces/devcore/crates/core"),
        );

        let command = step.environment_command(&environment).unwrap();

        assert_eq!(command[0], "podman");
        assert_eq!(command[1], "run");
        assert_eq!(command[2], "--rm");
        assert!(
            command
                .iter()
                .any(|argument| argument == "/workspace/crates/core")
        );
        let program_index = command
            .iter()
            .position(|argument| argument == "cargo")
            .unwrap();
        assert_eq!(command[program_index + 1], "build");
        assert!(
            !command[program_index..]
                .iter()
                .any(|argument| argument.contains("/workspaces"))
        );

        let mut invalid_environment = environment;
        invalid_environment.name = "Invalid Name".to_owned();
        assert!(matches!(
            step.environment_command(&invalid_environment),
            Err(PlanError::InvalidEnvironment(_))
        ));
    }

    #[test]
    fn creates_executor_request_from_environment_command() {
        let environment = EnvironmentSpec::new(
            "devcore-rust",
            "quay.io/devcore/rust@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            PathBuf::from("/workspaces/devcore"),
            vec![Toolchain::Rust],
            NetworkPolicy::Disabled,
        )
        .unwrap();
        let step = BuildStep::new(
            "compile",
            BuildAdapter::Cargo,
            PathBuf::from("/workspaces/devcore"),
        );

        let request = step.execution_request("compile-job", &environment).unwrap();

        assert_eq!(request.id, "compile-job");
        assert_eq!(request.argv[0..3], ["podman", "run", "--rm"]);
        let program_index = request
            .argv
            .iter()
            .position(|argument| argument == "cargo")
            .unwrap();
        assert!(
            !request.argv[program_index..]
                .iter()
                .any(|argument| argument.contains("/workspaces"))
        );
    }

    #[test]
    fn wraps_build_request_in_matching_resource_scope() {
        let environment = EnvironmentSpec::new(
            "devcore-rust",
            "quay.io/devcore/rust@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            PathBuf::from("/workspaces/devcore"),
            vec![Toolchain::Rust],
            NetworkPolicy::Disabled,
        )
        .unwrap();
        let step = BuildStep::new(
            "compile",
            BuildAdapter::Cargo,
            PathBuf::from("/workspaces/devcore"),
        );
        let snapshot = low_snapshot();
        let scope = snapshot
            .workload_scope("compile-job", WorkloadClass::Build)
            .unwrap();

        let request = step
            .execution_request_with_scope("compile-job", &environment, &scope)
            .unwrap();

        assert_eq!(request.argv[0..3], ["systemd-run", "--user", "--scope"]);
        assert!(request.argv.iter().any(|item| item == "--"));
        assert!(request.argv.iter().any(|item| item == "cargo"));
    }

    #[test]
    fn rejects_a_scope_for_the_wrong_workload() {
        let environment = EnvironmentSpec::new(
            "devcore-rust",
            "quay.io/devcore/rust@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            PathBuf::from("/workspaces/devcore"),
            vec![Toolchain::Rust],
            NetworkPolicy::Disabled,
        )
        .unwrap();
        let step = BuildStep::new(
            "compile",
            BuildAdapter::Cargo,
            PathBuf::from("/workspaces/devcore"),
        );
        let scope = low_snapshot()
            .workload_scope("compile-job", WorkloadClass::Indexer)
            .unwrap();

        assert!(matches!(
            step.execution_request_with_scope("compile-job", &environment, &scope),
            Err(PlanError::WrongWorkload {
                expected: WorkloadClass::Build,
                actual: WorkloadClass::Indexer,
            })
        ));
    }

    #[test]
    fn returns_dependency_order() {
        let directory = PathBuf::from("/workspace/app");
        let mut plan = BuildPlan::new();
        plan.push(
            BuildStep::new("package", BuildAdapter::Cargo, directory.clone()).depends_on("compile"),
        );
        plan.push(BuildStep::new("compile", BuildAdapter::Cargo, directory));

        assert_eq!(plan.execution_order().unwrap(), ["compile", "package"]);
    }

    #[test]
    fn rejects_cycles_and_missing_dependencies() {
        let directory = PathBuf::from("/workspace/app");
        let mut missing = BuildPlan::new();
        missing.push(
            BuildStep::new("compile", BuildAdapter::Cargo, directory.clone()).depends_on("prepare"),
        );
        assert!(matches!(
            missing.execution_order(),
            Err(PlanError::UnknownDependency { .. })
        ));

        let mut cycle = BuildPlan::new();
        cycle.push(BuildStep::new("a", BuildAdapter::Cargo, directory.clone()).depends_on("b"));
        cycle.push(BuildStep::new("b", BuildAdapter::Cargo, directory).depends_on("a"));
        assert!(matches!(cycle.execution_order(), Err(PlanError::Cycle(_))));
    }

    #[test]
    fn duplicate_step_ids_are_rejected() {
        let directory = PathBuf::from("/workspace/app");
        let mut plan = BuildPlan::new();
        plan.push(BuildStep::new(
            "compile",
            BuildAdapter::Cargo,
            directory.clone(),
        ));
        plan.push(BuildStep::new("compile", BuildAdapter::Go, directory));

        assert!(matches!(
            plan.execution_order(),
            Err(PlanError::DuplicateStep(id)) if id == "compile"
        ));
    }

    #[test]
    fn low_profile_builds_are_limited_to_one_worker() {
        let mut plan = BuildPlan::new();
        plan.push(BuildStep::new(
            "compile",
            BuildAdapter::Cargo,
            PathBuf::from("/workspace/app"),
        ));

        assert_eq!(plan.recommended_workers(&low_snapshot()), 1);
    }

    #[test]
    fn cache_keys_are_deterministic_and_include_reproducible_inputs() {
        let environment = rust_environment();
        let first = BuildStep::new(
            "compile",
            BuildAdapter::Cargo,
            PathBuf::from("/workspaces/devcore"),
        )
        .depends_on("prepare")
        .depends_on("prepare");
        let second = BuildStep::new(
            "compile",
            BuildAdapter::Cargo,
            PathBuf::from("/workspaces/devcore"),
        )
        .depends_on("prepare");

        let first_key = BuildCacheDescriptor::from_step(&environment, &first, "source-v1")
            .unwrap()
            .key();
        let second_key = BuildCacheDescriptor::from_step(&environment, &second, "source-v1")
            .unwrap()
            .key();
        let changed_key = BuildCacheDescriptor::from_step(&environment, &second, "source-v2")
            .unwrap()
            .key();

        assert_eq!(first_key, second_key);
        assert_ne!(first_key, changed_key);
        assert_eq!(first_key.as_str().len(), 64);
        assert!(
            first_key
                .as_str()
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        );
    }

    #[test]
    fn cache_descriptor_rejects_mutable_environment_images() {
        let environment = EnvironmentSpec::new(
            "devcore-rust",
            "quay.io/devcore/rust:latest",
            PathBuf::from("/workspaces/devcore"),
            vec![Toolchain::Rust],
            NetworkPolicy::Disabled,
        )
        .unwrap();
        let step = BuildStep::new(
            "compile",
            BuildAdapter::Cargo,
            PathBuf::from("/workspaces/devcore"),
        );

        assert!(matches!(
            BuildCacheDescriptor::from_step(&environment, &step, "source-v1"),
            Err(PlanError::UnpinnedEnvironment(_))
        ));
    }

    #[test]
    fn cache_store_publishes_regular_files_idempotently() {
        static NEXT_CACHE_TEST: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "devcore-build-cache-test-{}-{}",
            std::process::id(),
            NEXT_CACHE_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        let store = BuildCacheStore::open(&root).unwrap();
        let source = root.join("artifact.bin");
        fs::write(&source, b"artifact bytes").unwrap();
        let key = BuildCacheDescriptor::from_step(
            &rust_environment(),
            &BuildStep::new(
                "compile",
                BuildAdapter::Cargo,
                PathBuf::from("/workspaces/devcore"),
            ),
            "source-v1",
        )
        .unwrap()
        .key();

        let first_path = store.publish_file(&key, &source).unwrap();
        let second_path = store.publish_file(&key, &source).unwrap();

        assert_eq!(first_path, second_path);
        assert!(store.contains(&key).unwrap());
        assert_eq!(fs::read(first_path).unwrap(), b"artifact bytes");

        let occupied_key = BuildCacheDescriptor::from_step(
            &rust_environment(),
            &BuildStep::new(
                "package",
                BuildAdapter::Cargo,
                PathBuf::from("/workspaces/devcore"),
            ),
            "source-v1",
        )
        .unwrap()
        .key();
        fs::create_dir(store.artifact_path(&occupied_key)).unwrap();
        assert!(matches!(
            store.publish_file(&occupied_key, &source),
            Err(super::CacheStoreError::InvalidDestination(_))
        ));
        let _ = fs::remove_dir_all(root);
    }
}
