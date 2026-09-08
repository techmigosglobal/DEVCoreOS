#![forbid(unsafe_code)]
//! Resource-aware test-plan types for DevCore.
//!
//! This crate coordinates test intent and scheduling decisions without
//! executing host commands. The user-session Work service runs the resulting direct
//! argv commands inside a selected OCI environment or on an explicitly
//! approved device/remote worker.

use std::{collections::BTreeMap, error::Error, fmt, path::PathBuf};

use devcore_core::{ResourceSnapshot, ScopePlanError, WorkloadClass, WorkloadScopePlan};
use devcore_environment::EnvironmentSpec;
use devcore_execution::ExecutionRequest;

/// Test categories understood by the unified coordinator.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TestKind {
    /// Linting, formatting, type checking, or other static checks.
    StaticAnalysis,
    /// Fast in-process unit tests.
    Unit,
    /// Tests spanning more than one local process or component.
    Integration,
    /// UI or accessibility tests.
    Ui,
    /// HTTP or other service-contract tests.
    Api,
    /// Tests that use a local database instance.
    Database,
    /// Tests requiring a physical device or attached target.
    Device,
    /// Tests requiring an emulator or virtual device.
    Emulator,
    /// Timing, throughput, or latency measurements.
    Performance,
    /// Explicit memory-pressure and reclaim behavior tests.
    MemoryPressure,
    /// Security and policy checks.
    Security,
    /// Regression suites assembled from prior failures.
    Regression,
}

impl TestKind {
    /// Returns the stable test-kind identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StaticAnalysis => "static-analysis",
            Self::Unit => "unit",
            Self::Integration => "integration",
            Self::Ui => "ui",
            Self::Api => "api",
            Self::Database => "database",
            Self::Device => "device",
            Self::Emulator => "emulator",
            Self::Performance => "performance",
            Self::MemoryPressure => "memory-pressure",
            Self::Security => "security",
            Self::Regression => "regression",
        }
    }

    /// Returns the resource class used for the initial scheduling decision.
    #[must_use]
    pub const fn workload_class(self) -> WorkloadClass {
        match self {
            Self::Emulator => WorkloadClass::Emulator,
            Self::StaticAnalysis => WorkloadClass::Indexer,
            Self::Unit
            | Self::Integration
            | Self::Ui
            | Self::Api
            | Self::Database
            | Self::Device
            | Self::Performance
            | Self::MemoryPressure
            | Self::Security
            | Self::Regression => WorkloadClass::Build,
        }
    }
}

/// A direct test command that does not pass through a shell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestCommand {
    /// Executable resolved inside the selected environment or worker.
    pub program: String,
    /// Arguments passed as separate argv entries.
    pub args: Vec<String>,
}

impl TestCommand {
    /// Creates a direct command specification.
    pub fn new(
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    fn validate(&self) -> Result<(), TestPlanError> {
        if self.program.is_empty() || self.program.chars().any(char::is_whitespace) {
            return Err(TestPlanError::InvalidCommand(self.program.clone()));
        }
        Ok(())
    }
}

/// One node in a test dependency DAG.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestStep {
    /// Stable step identifier within a plan.
    pub id: String,
    /// Test category used for scheduling.
    pub kind: TestKind,
    /// Direct command to execute inside an approved worker.
    pub command: TestCommand,
    /// Project directory passed to the worker as its working directory.
    pub working_directory: PathBuf,
    /// Steps that must complete before this step starts.
    pub dependencies: Vec<String>,
}

impl TestStep {
    /// Creates a test step with no dependencies.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        kind: TestKind,
        command: TestCommand,
        working_directory: PathBuf,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            command,
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

    /// Builds an executor request from this step and a validated environment.
    pub fn execution_request(
        &self,
        job_id: impl Into<String>,
        environment: &EnvironmentSpec,
    ) -> Result<ExecutionRequest, TestPlanError> {
        self.command.validate()?;
        environment
            .validate()
            .map_err(TestPlanError::InvalidEnvironment)?;
        let command = environment
            .run_command_at(
                &self.working_directory,
                self.command.program.clone(),
                &self.command.args,
            )
            .map_err(TestPlanError::InvalidEnvironment)?;
        Ok(ExecutionRequest::from_argv(job_id, command))
    }

    /// Wraps this step in a scope matching its resource workload class.
    pub fn execution_request_with_scope(
        &self,
        job_id: impl Into<String>,
        environment: &EnvironmentSpec,
        scope: &WorkloadScopePlan,
    ) -> Result<ExecutionRequest, TestPlanError> {
        let expected = self.kind.workload_class();
        if scope.workload != expected {
            return Err(TestPlanError::WrongWorkload {
                expected,
                actual: scope.workload,
            });
        }
        let request = self.execution_request(job_id, environment)?;
        let argv = scope
            .systemd_run_argv(&request.argv)
            .map_err(TestPlanError::InvalidScope)?;
        Ok(ExecutionRequest::from_argv(request.id, argv))
    }
}

/// A validated collection of test steps.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TestPlan {
    steps: Vec<TestStep>,
}

impl TestPlan {
    /// Creates an empty test plan.
    #[must_use]
    pub const fn new() -> Self {
        Self { steps: Vec::new() }
    }

    /// Adds a test step to the plan.
    pub fn push(&mut self, step: TestStep) {
        self.steps.push(step);
    }

    /// Returns steps in insertion order.
    #[must_use]
    pub fn steps(&self) -> &[TestStep] {
        &self.steps
    }

    /// Validates commands/dependencies and returns a deterministic order.
    pub fn execution_order(&self) -> Result<Vec<String>, TestPlanError> {
        let mut steps = BTreeMap::new();
        for step in &self.steps {
            if step.id.is_empty() {
                return Err(TestPlanError::InvalidStepId(step.id.clone()));
            }
            step.command.validate()?;
            if steps.insert(step.id.clone(), step).is_some() {
                return Err(TestPlanError::DuplicateStep(step.id.clone()));
            }
        }

        for step in &self.steps {
            for dependency in &step.dependencies {
                if !steps.contains_key(dependency.as_str()) {
                    return Err(TestPlanError::UnknownDependency {
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

    /// Schedules the plan using the current conservative resource policy.
    pub fn schedule(
        &self,
        snapshot: &ResourceSnapshot,
    ) -> Result<Vec<TestSchedule>, TestPlanError> {
        let order = self.execution_order()?;
        let steps = self
            .steps
            .iter()
            .map(|step| (step.id.as_str(), step))
            .collect::<BTreeMap<_, _>>();

        order
            .into_iter()
            .map(|id| {
                let step = steps
                    .get(id.as_str())
                    .expect("execution order only contains validated steps");
                let workload = step.kind.workload_class();
                let budget = snapshot.workload_budget(workload);
                Ok(TestSchedule {
                    id,
                    kind: step.kind,
                    workload,
                    max_workers: budget.max_workers,
                    deferred: budget.deferred,
                })
            })
            .collect()
    }
}

fn visit(
    id: &str,
    steps: &BTreeMap<String, &TestStep>,
    state: &mut BTreeMap<String, VisitState>,
    order: &mut Vec<String>,
) -> Result<(), TestPlanError> {
    match state.get(id) {
        Some(VisitState::Done) => return Ok(()),
        Some(VisitState::Visiting) => return Err(TestPlanError::Cycle(id.to_owned())),
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

/// The scheduling decision for one test step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestSchedule {
    /// Test step identifier.
    pub id: String,
    /// Test category.
    pub kind: TestKind,
    /// Workload class used to derive the decision.
    pub workload: WorkloadClass,
    /// Maximum local parallel workers for this step.
    pub max_workers: u16,
    /// Whether this step should wait for another worker/device.
    pub deferred: bool,
}

/// A test-plan validation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TestPlanError {
    /// A step identifier was empty.
    InvalidStepId(String),
    /// A command executable was empty or contained whitespace.
    InvalidCommand(String),
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
    /// The scope was created for a different workload class.
    WrongWorkload {
        /// Workload required by this test step.
        expected: WorkloadClass,
        /// Workload carried by the supplied scope.
        actual: WorkloadClass,
    },
    /// The scope command could not be represented safely.
    InvalidScope(ScopePlanError),
}

impl fmt::Display for TestPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidStepId(id) => write!(formatter, "invalid test step id {id:?}"),
            Self::InvalidCommand(program) => write!(formatter, "invalid test command {program:?}"),
            Self::DuplicateStep(id) => write!(formatter, "duplicate test step {id:?}"),
            Self::UnknownDependency { step, dependency } => write!(
                formatter,
                "test step {step:?} depends on missing step {dependency:?}"
            ),
            Self::Cycle(id) => write!(formatter, "test dependency cycle includes {id:?}"),
            Self::InvalidEnvironment(error) => {
                write!(formatter, "invalid test environment: {error}")
            }
            Self::WrongWorkload { expected, actual } => write!(
                formatter,
                "test scope workload is {}, expected {}",
                actual.as_str(),
                expected.as_str()
            ),
            Self::InvalidScope(error) => write!(formatter, "invalid test scope: {error}"),
        }
    }
}

impl Error for TestPlanError {}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use devcore_core::{
        HardwareCapacity, MachineProfile, PowerState, ResourceSnapshot, WorkloadClass,
    };
    use devcore_environment::{EnvironmentSpec, NetworkPolicy, Toolchain};

    use super::{TestCommand, TestKind, TestPlan, TestPlanError, TestStep};

    fn snapshot(profile: MachineProfile) -> ResourceSnapshot {
        ResourceSnapshot {
            capacity: HardwareCapacity::new(2, 4 * 1024 * 1024),
            initial_profile: profile,
            memory_available_kib: Some(2 * 1024 * 1024),
            cpu_pressure: None,
            memory_pressure: None,
            io_pressure: None,
            power: PowerState::default(),
        }
    }

    fn step(id: &str, kind: TestKind) -> TestStep {
        TestStep::new(
            id,
            kind,
            TestCommand::new("cargo", ["test", "--workspace"]),
            PathBuf::from("/workspace/app"),
        )
    }

    #[test]
    fn returns_dependency_order_and_direct_arguments() {
        let mut plan = TestPlan::new();
        plan.push(step("report", TestKind::Regression).depends_on("unit"));
        plan.push(step("unit", TestKind::Unit));

        assert_eq!(plan.execution_order().unwrap(), ["unit", "report"]);
        assert_eq!(plan.steps()[0].command.args, ["test", "--workspace"]);
    }

    #[test]
    fn low_profile_defers_emulator_work() {
        let mut plan = TestPlan::new();
        plan.push(step("emulator", TestKind::Emulator));
        plan.push(step("static", TestKind::StaticAnalysis));

        let schedule = plan.schedule(&snapshot(MachineProfile::Low)).unwrap();
        assert!(
            schedule
                .iter()
                .any(|item| item.id == "emulator" && item.deferred)
        );
        assert!(
            schedule
                .iter()
                .any(|item| item.id == "static" && !item.deferred)
        );
    }

    #[test]
    fn rejects_cycles_and_invalid_commands() {
        let mut cycle = TestPlan::new();
        cycle.push(step("a", TestKind::Unit).depends_on("b"));
        cycle.push(step("b", TestKind::Unit).depends_on("a"));
        assert!(matches!(
            cycle.execution_order(),
            Err(TestPlanError::Cycle(_))
        ));

        let mut invalid = TestPlan::new();
        invalid.push(TestStep::new(
            "bad",
            TestKind::Unit,
            TestCommand::new("cargo test", Vec::<String>::new()),
            PathBuf::from("/workspace/app"),
        ));
        assert!(matches!(
            invalid.execution_order(),
            Err(TestPlanError::InvalidCommand(_))
        ));
    }

    #[test]
    fn creates_executor_request_inside_environment_workspace() {
        let environment = EnvironmentSpec::new(
            "devcore-rust",
            "quay.io/devcore/rust@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            PathBuf::from("/workspaces/devcore"),
            vec![Toolchain::Rust],
            NetworkPolicy::Disabled,
        )
        .unwrap();
        let step = TestStep::new(
            "unit",
            TestKind::Unit,
            TestCommand::new("cargo", ["test", "--workspace"]),
            PathBuf::from("/workspaces/devcore/app"),
        );

        let request = step.execution_request("unit-job", &environment).unwrap();

        assert_eq!(request.id, "unit-job");
        assert_eq!(request.argv[0..3], ["podman", "run", "--rm"]);
        assert!(
            request
                .argv
                .iter()
                .any(|argument| argument == "/workspace/app")
        );
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
    fn wraps_test_request_in_its_resource_scope() {
        let environment = EnvironmentSpec::new(
            "devcore-rust",
            "quay.io/devcore/rust@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            PathBuf::from("/workspaces/devcore"),
            vec![Toolchain::Rust],
            NetworkPolicy::Disabled,
        )
        .unwrap();
        let step = TestStep::new(
            "unit",
            TestKind::Unit,
            TestCommand::new("cargo", ["test", "--workspace"]),
            PathBuf::from("/workspaces/devcore"),
        );
        let scope = snapshot(MachineProfile::Low)
            .workload_scope("unit-job", WorkloadClass::Build)
            .unwrap();

        let request = step
            .execution_request_with_scope("unit-job", &environment, &scope)
            .unwrap();

        assert_eq!(request.argv[0..3], ["systemd-run", "--user", "--scope"]);
        assert!(request.argv.iter().any(|item| item == "--"));
        assert!(request.argv.iter().any(|item| item == "cargo"));
    }

    #[test]
    fn rejects_a_scope_for_the_wrong_test_workload() {
        let environment = EnvironmentSpec::new(
            "devcore-rust",
            "quay.io/devcore/rust@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            PathBuf::from("/workspaces/devcore"),
            vec![Toolchain::Rust],
            NetworkPolicy::Disabled,
        )
        .unwrap();
        let step = step("emulator", TestKind::Emulator);
        let scope = snapshot(MachineProfile::Low)
            .workload_scope("emulator-job", WorkloadClass::Build)
            .unwrap();

        assert!(matches!(
            step.execution_request_with_scope("emulator-job", &environment, &scope),
            Err(TestPlanError::WrongWorkload {
                expected: WorkloadClass::Emulator,
                actual: WorkloadClass::Build,
            })
        ));
    }
}
