#![forbid(unsafe_code)]
#![allow(missing_docs)]
//! On-demand user-session D-Bus service for DevCore work.
//!
//! This process intentionally runs on the session bus. It owns typed
//! Environment/Build/Test submission and delegates actual child supervision to
//! `devcore-execution`; no privileged system operation is exposed here.

use std::{env, error::Error, future::pending, path::PathBuf, sync::Arc};

use devcore_core::collect_snapshot;
use devcore_environment::{EnvironmentSpec, NetworkPolicy, Toolchain};
use devcore_execution::{JobLifecycle, JobStore};
use devcore_workd::{SharedWorkState, TestSubmission, WorkState};
use zbus::{connection::Builder, fdo, interface};

const BUS_NAME: &str = "org.devcore.Work1";
const OBJECT_PATH: &str = "/org/devcore/Work";

#[derive(Clone, Debug)]
struct EnvironmentApi {
    state: SharedWorkState,
}

#[interface(name = "org.devcore.Environment1")]
impl EnvironmentApi {
    fn define(
        &self,
        name: String,
        image: String,
        workspace: String,
        toolchains: Vec<String>,
        network_enabled: bool,
    ) -> fdo::Result<()> {
        let toolchains = parse_toolchains(&toolchains)?;
        let network = if network_enabled {
            NetworkPolicy::Enabled
        } else {
            NetworkPolicy::Disabled
        };
        let environment = EnvironmentSpec::new(name, image, workspace.into(), toolchains, network)
            .map_err(invalid_args)?;
        self.state
            .lock()
            .map_err(|_| failed("work state is poisoned"))?
            .define_environment(environment)
            .map_err(invalid_args)
    }

    fn ensure(
        &self,
        name: String,
        image: String,
        workspace: String,
        toolchains: Vec<String>,
        network_enabled: bool,
    ) -> fdo::Result<()> {
        let toolchains = parse_toolchains(&toolchains)?;
        let network = if network_enabled {
            NetworkPolicy::Enabled
        } else {
            NetworkPolicy::Disabled
        };
        let environment = EnvironmentSpec::new(name, image, workspace.into(), toolchains, network)
            .map_err(invalid_args)?;
        self.state
            .lock()
            .map_err(|_| failed("work state is poisoned"))?
            .ensure_environment(environment)
            .map_err(invalid_args)
    }

    fn create(&self, environment: &str, job_id: &str) -> fdo::Result<String> {
        self.state
            .lock()
            .map_err(|_| failed("work state is poisoned"))?
            .create_environment(environment, job_id)
            .map_err(invalid_args)
    }

    fn exec(
        &self,
        environment: &str,
        job_id: &str,
        working_directory: String,
        program: String,
        args: Vec<String>,
    ) -> fdo::Result<String> {
        self.state
            .lock()
            .map_err(|_| failed("work state is poisoned"))?
            .exec_environment(environment, job_id, working_directory.into(), program, args)
            .map_err(invalid_args)
    }

    fn stop(&self, environment: &str, job_id: &str) -> fdo::Result<String> {
        self.state
            .lock()
            .map_err(|_| failed("work state is poisoned"))?
            .stop_environment(environment, job_id)
            .map_err(invalid_args)
    }

    fn remove(&self, environment: &str, job_id: &str) -> fdo::Result<String> {
        self.state
            .lock()
            .map_err(|_| failed("work state is poisoned"))?
            .remove_environment(environment, job_id)
            .map_err(invalid_args)
    }
}

#[derive(Clone, Debug)]
struct BuildApi {
    state: SharedWorkState,
}

#[interface(name = "org.devcore.Build1")]
impl BuildApi {
    fn start_build(
        &self,
        environment: &str,
        job_id: &str,
        step_id: String,
        adapter: &str,
        working_directory: String,
    ) -> fdo::Result<String> {
        self.state
            .lock()
            .map_err(|_| failed("work state is poisoned"))?
            .start_build(
                environment,
                job_id,
                step_id,
                adapter,
                working_directory.into(),
            )
            .map_err(invalid_args)
    }
}

#[derive(Clone, Debug)]
struct TestApi {
    state: SharedWorkState,
}

#[interface(name = "org.devcore.Test1")]
impl TestApi {
    #[allow(clippy::too_many_arguments)]
    fn start_test(
        &self,
        environment: &str,
        job_id: &str,
        step_id: String,
        kind: &str,
        program: String,
        args: Vec<String>,
        working_directory: String,
    ) -> fdo::Result<String> {
        self.state
            .lock()
            .map_err(|_| failed("work state is poisoned"))?
            .start_test(
                environment,
                job_id,
                TestSubmission {
                    step_id,
                    kind: kind.to_owned(),
                    program,
                    args,
                    working_directory: working_directory.into(),
                },
            )
            .map_err(invalid_args)
    }
}

#[derive(Clone, Debug)]
struct WorkspaceApi {
    state: SharedWorkState,
}

#[interface(name = "org.devcore.Workspace1")]
impl WorkspaceApi {
    fn list(
        &self,
        environment: &str,
        directory: &str,
    ) -> fdo::Result<Vec<(String, bool, bool, u64)>> {
        let entries = self
            .state
            .lock()
            .map_err(|_| failed("work state is poisoned"))?
            .list_workspace(environment, directory)
            .map_err(invalid_args)?;
        Ok(entries
            .into_iter()
            .map(|entry| (entry.path, entry.directory, entry.symlink, entry.bytes))
            .collect())
    }

    fn read_text(&self, environment: &str, path: &str) -> fdo::Result<(String, String)> {
        let file = self
            .state
            .lock()
            .map_err(|_| failed("work state is poisoned"))?
            .read_workspace_file(environment, path)
            .map_err(invalid_args)?;
        Ok((file.path, file.contents))
    }

    fn write_text(
        &self,
        environment: &str,
        path: &str,
        contents: &str,
    ) -> fdo::Result<(String, u64)> {
        let file = self
            .state
            .lock()
            .map_err(|_| failed("work state is poisoned"))?
            .write_workspace_file(environment, path, contents)
            .map_err(invalid_args)?;
        Ok((file.path, file.contents.len() as u64))
    }
}

#[derive(Clone, Debug)]
struct GitApi {
    state: SharedWorkState,
}

#[interface(name = "org.devcore.Git1")]
impl GitApi {
    fn start_status(
        &self,
        environment: &str,
        job_id: &str,
        working_directory: String,
    ) -> fdo::Result<String> {
        self.state
            .lock()
            .map_err(|_| failed("work state is poisoned"))?
            .start_git_status(environment, job_id, working_directory.into())
            .map_err(invalid_args)
    }

    fn start_diff(
        &self,
        environment: &str,
        job_id: &str,
        working_directory: String,
    ) -> fdo::Result<String> {
        self.state
            .lock()
            .map_err(|_| failed("work state is poisoned"))?
            .start_git_diff(environment, job_id, working_directory.into())
            .map_err(invalid_args)
    }

    fn start_history(
        &self,
        environment: &str,
        job_id: &str,
        working_directory: String,
    ) -> fdo::Result<String> {
        self.state
            .lock()
            .map_err(|_| failed("work state is poisoned"))?
            .start_git_history(environment, job_id, working_directory.into())
            .map_err(invalid_args)
    }
}

#[derive(Clone, Debug)]
struct JobApi {
    state: SharedWorkState,
}

type JobStatusTuple = (String, String, Option<i32>, bool, u64, Option<String>);

#[interface(name = "org.devcore.Job1")]
impl JobApi {
    fn status(&self, job_id: &str) -> fdo::Result<JobStatusTuple> {
        let status = self
            .state
            .lock()
            .map_err(|_| failed("work state is poisoned"))?
            .status(job_id)
            .map_err(invalid_args)?;
        Ok((
            status.id,
            lifecycle_name(status.lifecycle).to_owned(),
            status.exit_code,
            status.output_truncated,
            status.duration_millis,
            status.error,
        ))
    }

    fn cancel(&self, job_id: &str) -> fdo::Result<()> {
        self.state
            .lock()
            .map_err(|_| failed("work state is poisoned"))?
            .cancel(job_id)
            .map_err(invalid_args)
    }

    fn read_log(
        &self,
        job_id: &str,
        stream: &str,
        offset: u64,
        max_bytes: u64,
    ) -> fdo::Result<(Vec<u8>, bool, u64)> {
        let chunk = self
            .state
            .lock()
            .map_err(|_| failed("work state is poisoned"))?
            .read_log(job_id, stream, offset, max_bytes)
            .map_err(invalid_args)?;
        Ok((chunk.bytes, chunk.end_of_file, chunk.total_bytes))
    }
}

fn parse_toolchains(values: &[String]) -> Result<Vec<Toolchain>, fdo::Error> {
    values
        .iter()
        .map(|value| match value.as_str() {
            "rust" => Ok(Toolchain::Rust),
            "go" => Ok(Toolchain::Go),
            "python" => Ok(Toolchain::Python),
            "node" | "nodejs" => Ok(Toolchain::Node),
            "flutter" | "dart" => Ok(Toolchain::Flutter),
            "android" | "android-sdk" => Ok(Toolchain::AndroidSdk),
            "java" => Ok(Toolchain::Java),
            "qt" => Ok(Toolchain::Qt),
            "cmake" | "c-cpp" => Ok(Toolchain::CMake),
            _ => Err(invalid_args(format!("unknown toolchain {value:?}"))),
        })
        .collect()
}

fn lifecycle_name(lifecycle: JobLifecycle) -> &'static str {
    match lifecycle {
        JobLifecycle::Queued => "queued",
        JobLifecycle::Running => "running",
        JobLifecycle::Succeeded => "succeeded",
        JobLifecycle::Failed => "failed",
        JobLifecycle::Cancelled => "cancelled",
    }
}

fn invalid_args(error: impl ToString) -> fdo::Error {
    fdo::Error::InvalidArgs(error.to_string())
}

fn failed(message: impl Into<String>) -> fdo::Error {
    fdo::Error::Failed(message.into())
}

fn state_root() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(path).join("devcore/jobs"));
    }
    env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".local/state/devcore/jobs"))
        .ok_or_else(|| "XDG_STATE_HOME or HOME is required for job persistence".to_owned())
}

fn main() -> Result<(), Box<dyn Error>> {
    zbus::block_on(async {
        let snapshot = collect_snapshot().map_err(|error| error.to_string())?;
        let store = JobStore::open(state_root()?).map_err(|error| error.to_string())?;
        let state = WorkState::new(snapshot, store).shared();
        let _connection = Builder::session()
            .map_err(|error| error.to_string())?
            .name(BUS_NAME)
            .map_err(|error| error.to_string())?
            .serve_at(
                OBJECT_PATH,
                EnvironmentApi {
                    state: Arc::clone(&state),
                },
            )
            .map_err(|error| error.to_string())?
            .serve_at(
                OBJECT_PATH,
                BuildApi {
                    state: Arc::clone(&state),
                },
            )
            .map_err(|error| error.to_string())?
            .serve_at(
                OBJECT_PATH,
                TestApi {
                    state: Arc::clone(&state),
                },
            )
            .map_err(|error| error.to_string())?
            .serve_at(
                OBJECT_PATH,
                WorkspaceApi {
                    state: Arc::clone(&state),
                },
            )
            .map_err(|error| error.to_string())?
            .serve_at(
                OBJECT_PATH,
                GitApi {
                    state: Arc::clone(&state),
                },
            )
            .map_err(|error| error.to_string())?
            .serve_at(OBJECT_PATH, JobApi { state })
            .map_err(|error| error.to_string())?
            .build()
            .await
            .map_err(|error| error.to_string())?;

        pending::<()>().await;
        Ok::<(), String>(())
    })?;
    Ok(())
}
