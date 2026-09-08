#![forbid(unsafe_code)]
//! Rootless OCI environment command planning.
//!
//! The manager creates argv vectors for Podman but does not execute them. A
//! the user-session work service owns execution, cancellation, logs, and state
//! while keeping project commands inside the selected environment.

use std::{
    error::Error,
    fmt,
    path::{Component, Path, PathBuf},
};

use devcore_execution::ExecutionRequest;

/// Language/toolchain families supported by DevCore environments.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Toolchain {
    /// Rust and Cargo development.
    Rust,
    /// Go development.
    Go,
    /// Python development.
    Python,
    /// Node.js and npm/pnpm development.
    Node,
    /// Flutter and Dart development.
    Flutter,
    /// Android SDK and Gradle development.
    AndroidSdk,
    /// Java and Gradle development.
    Java,
    /// Qt development.
    Qt,
    /// CMake and native C/C++ development.
    CMake,
}

impl Toolchain {
    /// Returns the stable toolchain identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Python => "python",
            Self::Node => "node",
            Self::Flutter => "flutter",
            Self::AndroidSdk => "android-sdk",
            Self::Java => "java",
            Self::Qt => "qt",
            Self::CMake => "cmake",
        }
    }
}

/// Network policy for an environment invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkPolicy {
    /// Do not expose host or external networking to the environment.
    Disabled,
    /// Allow networking only when a caller explicitly needs dependency fetches.
    Enabled,
}

/// A validated environment definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvironmentSpec {
    /// Stable environment name used for Podman lifecycle commands.
    pub name: String,
    /// OCI image reference, preferably digest pinned.
    pub image: String,
    /// One workspace directory mounted read-write at `/workspace`.
    pub workspace: PathBuf,
    /// Language/toolchain families expected by the workspace.
    pub toolchains: Vec<Toolchain>,
    /// Explicit network policy for commands in this environment.
    pub network: NetworkPolicy,
}

impl EnvironmentSpec {
    /// Creates and validates an environment definition.
    pub fn new(
        name: impl Into<String>,
        image: impl Into<String>,
        workspace: PathBuf,
        toolchains: Vec<Toolchain>,
        network: NetworkPolicy,
    ) -> Result<Self, EnvironmentError> {
        let spec = Self {
            name: name.into(),
            image: image.into(),
            workspace,
            toolchains,
            network,
        };
        spec.validate()?;
        Ok(spec)
    }

    /// Validates names and mount paths before any Podman command is generated.
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        if self.name.is_empty()
            || self.name.len() > 63
            || !self.name.chars().all(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
            })
            || self.name.starts_with('-')
        {
            return Err(EnvironmentError::InvalidName(self.name.clone()));
        }
        if self.image.is_empty() || self.image.chars().any(char::is_whitespace) {
            return Err(EnvironmentError::InvalidImage(self.image.clone()));
        }
        if !valid_workspace(&self.workspace) {
            return Err(EnvironmentError::InvalidWorkspace(self.workspace.clone()));
        }
        if self.toolchains.is_empty() {
            return Err(EnvironmentError::NoToolchain);
        }
        Ok(())
    }

    /// Whether this environment can be reproduced without a mutable image tag.
    #[must_use]
    pub fn is_digest_pinned(&self) -> bool {
        let Some((_, digest)) = self.image.rsplit_once("@sha256:") else {
            return false;
        };
        digest.len() == 64
            && digest
                .chars()
                .all(|character| character.is_ascii_hexdigit())
    }

    /// Builds a rootless Podman command to create the environment.
    #[must_use]
    pub fn create_command(&self) -> Vec<String> {
        let mut command = vec![
            "podman".to_owned(),
            "run".to_owned(),
            "--detach".to_owned(),
            "--name".to_owned(),
            self.name.clone(),
            "--userns=keep-id".to_owned(),
            "--cap-drop=ALL".to_owned(),
            "--security-opt=no-new-privileges".to_owned(),
            "--read-only".to_owned(),
            "--tmpfs".to_owned(),
            "/tmp:rw,noexec,nosuid,nodev".to_owned(),
            "--mount".to_owned(),
            format!(
                "type=bind,src={},dst=/workspace,relabel=private",
                self.workspace.display()
            ),
            "--workdir".to_owned(),
            "/workspace".to_owned(),
        ];
        if self.network == NetworkPolicy::Disabled {
            command.push("--network=none".to_owned());
        }
        command.push(self.image.clone());
        command.push("sleep".to_owned());
        command.push("infinity".to_owned());
        command
    }

    /// Builds a direct command invocation inside the running environment.
    #[must_use]
    pub fn exec_command(&self, program: impl Into<String>, args: &[String]) -> Vec<String> {
        let mut command = vec![
            "podman".to_owned(),
            "exec".to_owned(),
            self.name.clone(),
            program.into(),
        ];
        command.extend(args.iter().cloned());
        command
    }

    /// Builds a direct command with a host workspace-relative working path.
    pub fn exec_command_at(
        &self,
        working_directory: &Path,
        program: impl Into<String>,
        args: &[String],
    ) -> Result<Vec<String>, EnvironmentError> {
        let container_directory = self.container_working_directory(working_directory)?;
        let mut command = vec![
            "podman".to_owned(),
            "exec".to_owned(),
            "--workdir".to_owned(),
            container_directory,
            self.name.clone(),
            program.into(),
        ];
        command.extend(args.iter().cloned());
        Ok(command)
    }

    /// Builds a lazy, disposable rootless command for one workspace operation.
    ///
    /// Unlike [`Self::exec_command_at`], this does not require a named
    /// container to have been created or started. The image is run only for
    /// the operation and is removed when the command exits.
    pub fn run_command_at(
        &self,
        working_directory: &Path,
        program: impl Into<String>,
        args: &[String],
    ) -> Result<Vec<String>, EnvironmentError> {
        let container_directory = self.container_working_directory(working_directory)?;
        let mut command = vec![
            "podman".to_owned(),
            "run".to_owned(),
            "--rm".to_owned(),
            "--userns=keep-id".to_owned(),
            "--cap-drop=ALL".to_owned(),
            "--security-opt=no-new-privileges".to_owned(),
            "--read-only".to_owned(),
            "--tmpfs".to_owned(),
            "/tmp:rw,noexec,nosuid,nodev".to_owned(),
            "--mount".to_owned(),
            format!(
                "type=bind,src={},dst=/workspace,relabel=private",
                self.workspace.display()
            ),
            "--workdir".to_owned(),
            container_directory,
        ];
        if self.network == NetworkPolicy::Disabled {
            command.push("--network=none".to_owned());
        }
        command.push(self.image.clone());
        command.push(program.into());
        command.extend(args.iter().cloned());
        Ok(command)
    }

    fn container_working_directory(
        &self,
        working_directory: &Path,
    ) -> Result<String, EnvironmentError> {
        let relative = working_directory
            .strip_prefix(&self.workspace)
            .map_err(|_| {
                EnvironmentError::InvalidWorkingDirectory(working_directory.to_path_buf())
            })?;
        let mut container_directory = PathBuf::from("/workspace");
        for component in relative.components() {
            match component {
                Component::Normal(part) => container_directory.push(part),
                Component::CurDir
                | Component::ParentDir
                | Component::RootDir
                | Component::Prefix(_) => {
                    return Err(EnvironmentError::InvalidWorkingDirectory(
                        working_directory.to_path_buf(),
                    ));
                }
            }
        }
        container_directory
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| {
                EnvironmentError::InvalidWorkingDirectory(working_directory.to_path_buf())
            })
    }

    /// Creates an executor request for starting this environment.
    ///
    /// The request is declarative. The environment service must authorize the
    /// lifecycle transition and run it through the bounded executor.
    pub fn create_execution_request(
        &self,
        job_id: impl Into<String>,
    ) -> Result<ExecutionRequest, EnvironmentError> {
        self.validate()?;
        Ok(ExecutionRequest::from_argv(job_id, self.create_command()))
    }

    /// Creates an executor request for one command in this environment.
    pub fn exec_execution_request(
        &self,
        job_id: impl Into<String>,
        working_directory: &Path,
        program: impl Into<String>,
        args: &[String],
    ) -> Result<ExecutionRequest, EnvironmentError> {
        self.validate()?;
        let command = self.exec_command_at(working_directory, program, args)?;
        Ok(ExecutionRequest::from_argv(job_id, command))
    }

    /// Creates an executor request for a lazy disposable workspace command.
    pub fn run_execution_request_at(
        &self,
        job_id: impl Into<String>,
        working_directory: &Path,
        program: impl Into<String>,
        args: &[String],
    ) -> Result<ExecutionRequest, EnvironmentError> {
        self.validate()?;
        let command = self.run_command_at(working_directory, program, args)?;
        Ok(ExecutionRequest::from_argv(job_id, command))
    }

    /// Creates an executor request for stopping this environment.
    pub fn stop_execution_request(
        &self,
        job_id: impl Into<String>,
    ) -> Result<ExecutionRequest, EnvironmentError> {
        self.validate()?;
        Ok(ExecutionRequest::from_argv(job_id, self.stop_command()))
    }

    /// Creates an executor request for removing this stopped environment.
    pub fn remove_execution_request(
        &self,
        job_id: impl Into<String>,
    ) -> Result<ExecutionRequest, EnvironmentError> {
        self.validate()?;
        Ok(ExecutionRequest::from_argv(job_id, self.remove_command()))
    }

    /// Builds a stop command for the named environment.
    #[must_use]
    pub fn stop_command(&self) -> Vec<String> {
        vec!["podman".to_owned(), "stop".to_owned(), self.name.clone()]
    }

    /// Builds a remove command for a stopped environment.
    #[must_use]
    pub fn remove_command(&self) -> Vec<String> {
        vec!["podman".to_owned(), "rm".to_owned(), self.name.clone()]
    }
}

fn valid_workspace(path: &Path) -> bool {
    path.is_absolute()
        && path != Path::new("/")
        && !path.to_string_lossy().contains([',', ':'])
        && path
            .components()
            .all(|component| !matches!(component, Component::ParentDir | Component::CurDir))
}

/// An invalid environment definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentError {
    /// Name is not safe for a container identifier.
    InvalidName(String),
    /// Image reference is empty or contains whitespace.
    InvalidImage(String),
    /// Workspace is not an absolute, non-root, normalized path.
    InvalidWorkspace(PathBuf),
    /// At least one explicit toolchain is required.
    NoToolchain,
    /// Working directory is outside the mounted workspace or cannot be represented safely.
    InvalidWorkingDirectory(PathBuf),
}

impl fmt::Display for EnvironmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(name) => write!(formatter, "invalid environment name {name:?}"),
            Self::InvalidImage(image) => write!(formatter, "invalid environment image {image:?}"),
            Self::InvalidWorkspace(path) => {
                write!(
                    formatter,
                    "invalid environment workspace {}",
                    path.display()
                )
            }
            Self::NoToolchain => formatter.write_str("environment has no toolchain"),
            Self::InvalidWorkingDirectory(path) => write!(
                formatter,
                "working directory is outside the environment workspace: {}",
                path.display()
            ),
        }
    }
}

impl Error for EnvironmentError {}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{EnvironmentError, EnvironmentSpec, NetworkPolicy, Toolchain};

    fn spec(network: NetworkPolicy) -> EnvironmentSpec {
        EnvironmentSpec::new(
            "devcore-rust",
            "quay.io/devcore/rust@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            PathBuf::from("/workspaces/devcore"),
            vec![Toolchain::Rust],
            network,
        )
        .unwrap()
    }

    #[test]
    fn create_command_is_rootless_read_only_and_workspace_scoped() {
        let command = spec(NetworkPolicy::Disabled).create_command();
        let joined = command.join(" ");

        assert!(joined.contains("--userns=keep-id"));
        assert!(joined.contains("--cap-drop=ALL"));
        assert!(joined.contains("--read-only"));
        assert!(joined.contains("--network=none"));
        assert!(joined.contains("dst=/workspace"));
        assert!(!joined.contains("--privileged"));
        assert!(!joined.contains("--pid=host"));
    }

    #[test]
    fn enabled_network_is_explicit() {
        let command = spec(NetworkPolicy::Enabled).create_command();

        assert!(!command.iter().any(|argument| argument == "--network=none"));
        assert!(spec(NetworkPolicy::Enabled).is_digest_pinned());

        let mut tagged = spec(NetworkPolicy::Enabled);
        tagged.image = "quay.io/devcore/rust:latest".to_owned();
        assert!(!tagged.is_digest_pinned());

        let mut malformed = spec(NetworkPolicy::Enabled);
        malformed.image = "quay.io/devcore/rust@sha256:not-a-digest".to_owned();
        assert!(!malformed.is_digest_pinned());
    }

    #[test]
    fn exec_preserves_arguments_without_shell_interpolation() {
        let args = vec![
            "build".to_owned(),
            "--profile".to_owned(),
            "dev mode".to_owned(),
        ];
        let command = spec(NetworkPolicy::Disabled).exec_command("cargo", &args);

        assert_eq!(command[0], "podman");
        assert_eq!(command[3], "cargo");
        assert_eq!(command[6], "dev mode");
    }

    #[test]
    fn exec_maps_only_workspace_relative_directories() {
        let environment = spec(NetworkPolicy::Disabled);
        let args = vec!["build".to_owned()];
        let command = environment
            .exec_command_at(Path::new("/workspaces/devcore/crates/core"), "cargo", &args)
            .unwrap();

        assert_eq!(
            command,
            vec![
                "podman",
                "exec",
                "--workdir",
                "/workspace/crates/core",
                "devcore-rust",
                "cargo",
                "build"
            ]
        );
        assert!(
            environment
                .exec_command_at(Path::new("/workspaces/other"), "cargo", &args)
                .is_err()
        );
    }

    #[test]
    fn run_maps_workspace_and_keeps_the_container_disposable() {
        let environment = spec(NetworkPolicy::Disabled);
        let args = vec!["test".to_owned()];
        let command = environment
            .run_command_at(Path::new("/workspaces/devcore/crates/core"), "cargo", &args)
            .unwrap();

        assert_eq!(command[0..3], ["podman", "run", "--rm"]);
        assert!(
            command
                .iter()
                .any(|argument| argument == "/workspace/crates/core")
        );
        assert!(command.iter().any(|argument| argument == "--network=none"));
        assert_eq!(command[command.len() - 2..], ["cargo", "test"]);
        assert!(
            environment
                .run_command_at(Path::new("/workspaces/other"), "cargo", &args)
                .is_err()
        );
    }

    #[test]
    fn creates_lifecycle_requests_without_invoking_podman() {
        let environment = spec(NetworkPolicy::Disabled);
        let args = vec!["test".to_owned()];

        let create = environment.create_execution_request("create-job").unwrap();
        let exec = environment
            .exec_execution_request("exec-job", Path::new("/workspaces/devcore"), "cargo", &args)
            .unwrap();
        let run = environment
            .run_execution_request_at("run-job", Path::new("/workspaces/devcore"), "cargo", &args)
            .unwrap();
        let stop = environment.stop_execution_request("stop-job").unwrap();
        let remove = environment.remove_execution_request("remove-job").unwrap();

        assert_eq!(create.argv[0..3], ["podman", "run", "--detach"]);
        assert_eq!(exec.argv[0..2], ["podman", "exec"]);
        assert_eq!(run.argv[0..3], ["podman", "run", "--rm"]);
        assert_eq!(stop.argv, ["podman", "stop", "devcore-rust"]);
        assert_eq!(remove.argv, ["podman", "rm", "devcore-rust"]);
    }

    #[test]
    fn all_prompt_toolchains_have_stable_identifiers() {
        let toolchains = [
            Toolchain::Rust,
            Toolchain::Go,
            Toolchain::Python,
            Toolchain::Node,
            Toolchain::Flutter,
            Toolchain::AndroidSdk,
            Toolchain::Java,
            Toolchain::Qt,
            Toolchain::CMake,
        ];
        let identifiers = toolchains.map(Toolchain::as_str);

        assert_eq!(
            identifiers,
            [
                "rust",
                "go",
                "python",
                "node",
                "flutter",
                "android-sdk",
                "java",
                "qt",
                "cmake",
            ]
        );
    }

    #[test]
    fn lifecycle_requests_revalidate_the_environment() {
        let mut environment = spec(NetworkPolicy::Disabled);
        environment.name = "Bad Name".to_owned();

        assert!(matches!(
            environment.create_execution_request("create-job"),
            Err(EnvironmentError::InvalidName(_))
        ));
        assert!(matches!(
            environment.stop_execution_request("stop-job"),
            Err(EnvironmentError::InvalidName(_))
        ));
    }

    #[test]
    fn invalid_definitions_are_rejected() {
        let result = EnvironmentSpec::new(
            "Bad Name",
            "image:latest",
            PathBuf::from("/workspace/../host"),
            Vec::new(),
            NetworkPolicy::Disabled,
        );

        assert!(matches!(result, Err(EnvironmentError::InvalidName(_))));

        let result = EnvironmentSpec::new(
            "devcore-rust",
            "image:latest",
            PathBuf::from("/workspace,unsafe"),
            vec![Toolchain::Rust],
            NetworkPolicy::Disabled,
        );
        assert!(matches!(result, Err(EnvironmentError::InvalidWorkspace(_))));
    }
}
