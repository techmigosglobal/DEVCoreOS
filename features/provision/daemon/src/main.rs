#![forbid(unsafe_code)]
#![allow(missing_docs)]
//! On-demand user-session provisioning boundary.
//!
//! The daemon is started by session-bus activation only when the user opens
//! the post-install panel. It never downloads at boot, accepts only catalog
//! IDs and digest-pinned OCI sources, and runs every package operation through
//! the bounded direct-argv executor. NetworkManager owns Wi-Fi credentials;
//! this service only probes readiness and never receives a password.

use std::{
    env,
    error::Error,
    fs::{self, OpenOptions},
    future::pending,
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
};

use devcore_core::MachineProfile;
use devcore_execution::{
    CancellationToken, ExecutionEngine, ExecutionLimits, ExecutionRequest, JobState,
};
use devcore_provision::{
    NetworkState, ProvisionPlan, ProvisionRequest, catalog_json, network_probe_argv, plan_json,
    readiness,
};
use serde::{Deserialize, Serialize};
use zbus::{connection::Builder, fdo, interface};

const BUS_NAME: &str = "org.devcore.Provision1";
const OBJECT_PATH: &str = "/org/devcore/Provision";
const MAX_STATUS_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PersistedState {
    request_id: Option<String>,
    phase: String,
    network: NetworkState,
    profile: Option<MachineProfile>,
    selected_bundles: Vec<String>,
    completed_bundles: Vec<String>,
    current_bundle: Option<String>,
    last_error: Option<String>,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            request_id: None,
            phase: "idle".to_owned(),
            network: NetworkState::Unknown,
            profile: None,
            selected_bundles: Vec::new(),
            completed_bundles: Vec::new(),
            current_bundle: None,
            last_error: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct StatusDocument {
    state: PersistedState,
    readiness: devcore_provision::ProvisionReadiness,
}

#[derive(Clone, Debug)]
struct Runtime {
    state: Arc<Mutex<PersistedState>>,
    state_path: PathBuf,
    active: Arc<Mutex<Option<(String, CancellationToken)>>>,
}

impl Runtime {
    fn load(state_path: PathBuf) -> Result<Self, String> {
        if let Some(parent) = state_path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("state directory: {error}"))?;
        }
        let mut state = if state_path.is_file() {
            let bytes = fs::read(&state_path).map_err(|error| format!("state read: {error}"))?;
            serde_json::from_slice(&bytes).map_err(|error| format!("state parse: {error}"))?
        } else {
            PersistedState::default()
        };
        if matches!(state.phase.as_str(), "running" | "waiting-network") {
            state.phase = "interrupted".to_owned();
            state.current_bundle = None;
            state.last_error =
                Some("provisioning service restarted; press Start to resume".to_owned());
        }
        let runtime = Self {
            state: Arc::new(Mutex::new(state)),
            state_path,
            active: Arc::new(Mutex::new(None)),
        };
        runtime.persist()?;
        Ok(runtime)
    }

    fn status_json(&self) -> Result<String, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "provision state is poisoned".to_owned())?
            .clone();
        let document = StatusDocument {
            readiness: readiness(state.network),
            state,
        };
        let json =
            serde_json::to_string(&document).map_err(|error| format!("status encode: {error}"))?;
        if json.len() > MAX_STATUS_BYTES {
            return Err("provision status exceeded its display limit".to_owned());
        }
        Ok(json)
    }

    fn persist(&self) -> Result<(), String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "provision state is poisoned".to_owned())?
            .clone();
        let bytes =
            serde_json::to_vec_pretty(&state).map_err(|error| format!("state encode: {error}"))?;
        let temporary = self
            .state_path
            .with_extension(format!("json.{}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|error| format!("state temporary file: {error}"))?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("state write: {error}"))?;
        fs::rename(&temporary, &self.state_path).map_err(|error| format!("state commit: {error}"))
    }

    fn set_state(&self, update: impl FnOnce(&mut PersistedState)) {
        if let Ok(mut state) = self.state.lock() {
            update(&mut state);
        }
        let _ = self.persist();
    }

    fn start(&self, request: ProvisionRequest, plan: ProvisionPlan) -> Result<String, String> {
        if self
            .active
            .lock()
            .map_err(|_| "active provisioning state is poisoned".to_owned())?
            .is_some()
        {
            return Err("another provisioning request is already running".to_owned());
        }
        let request_id = next_request_id();
        let completed = self
            .state
            .lock()
            .map_err(|_| "provision state is poisoned".to_owned())?
            .clone();
        let completed_bundles = if completed.selected_bundles == plan.bundle_ids {
            completed.completed_bundles
        } else {
            Vec::new()
        };
        self.set_state(|state| {
            state.request_id = Some(request_id.clone());
            state.phase = "waiting-network".to_owned();
            state.network = NetworkState::Unknown;
            state.profile = Some(request.profile);
            state.selected_bundles = plan.bundle_ids.clone();
            state.completed_bundles = completed_bundles;
            state.current_bundle = None;
            state.last_error = None;
        });
        let cancellation = CancellationToken::new();
        self.active
            .lock()
            .map_err(|_| "active provisioning state is poisoned".to_owned())?
            .replace((request_id.clone(), cancellation.clone()));
        let runtime = self.clone();
        let worker_request_id = request_id.clone();
        thread::spawn(move || {
            runtime.run(worker_request_id, plan, cancellation);
        });
        Ok(request_id)
    }

    fn run(&self, request_id: String, plan: ProvisionPlan, cancellation: CancellationToken) {
        if plan.operations.is_empty() {
            self.set_state(|state| {
                state.phase = "external-provider-required".to_owned();
                state.current_bundle = None;
                state.last_error = None;
            });
            self.clear_active(&request_id);
            return;
        }
        let network = probe_network();
        self.set_state(|state| state.network = network);
        if network != NetworkState::Connected {
            self.set_state(|state| {
                state.phase = "waiting-network".to_owned();
                state.last_error =
                    Some("connect Wi-Fi in NetworkManager, then press Start again".to_owned());
            });
            self.clear_active(&request_id);
            return;
        }

        self.set_state(|state| {
            state.phase = "running".to_owned();
            state.last_error = None;
        });
        let engine = ExecutionEngine::new();
        for (index, operation) in plan.operations.iter().enumerate() {
            let already_done = self
                .state
                .lock()
                .map(|state| {
                    state
                        .completed_bundles
                        .iter()
                        .any(|id| id == &operation.bundle_id)
                })
                .unwrap_or(false);
            if already_done {
                continue;
            }
            if cancellation.is_cancelled() {
                self.set_state(|state| {
                    state.phase = "cancelled".to_owned();
                    state.current_bundle = None;
                });
                self.clear_active(&request_id);
                return;
            }
            self.set_state(|state| {
                state.current_bundle = Some(operation.bundle_id.clone());
                state.last_error = None;
            });
            let job_id = format!("provision-{}-{index}", std::process::id());
            let execution = ExecutionRequest::from_argv(job_id, operation.argv.clone());
            let result = engine.run(
                &execution,
                &cancellation,
                ExecutionLimits {
                    max_output_bytes: 4096,
                },
            );
            match result {
                Ok(result) if result.state == JobState::Succeeded => {
                    self.set_state(|state| {
                        state.completed_bundles.push(operation.bundle_id.clone());
                        state.current_bundle = None;
                    });
                }
                Ok(result) if result.state == JobState::Cancelled => {
                    self.set_state(|state| {
                        state.phase = "cancelled".to_owned();
                        state.current_bundle = None;
                    });
                    self.clear_active(&request_id);
                    return;
                }
                Ok(result) => {
                    self.set_state(|state| {
                        state.phase = "failed".to_owned();
                        state.current_bundle = None;
                        state.last_error = Some(format!(
                            "{} failed with {:?}",
                            operation.bundle_id, result.exit_code
                        ));
                    });
                    self.clear_active(&request_id);
                    return;
                }
                Err(error) => {
                    self.set_state(|state| {
                        state.phase = "failed".to_owned();
                        state.current_bundle = None;
                        state.last_error =
                            Some(format!("{} execution failed: {error}", operation.bundle_id));
                    });
                    self.clear_active(&request_id);
                    return;
                }
            }
        }
        self.set_state(|state| {
            state.phase = if plan.external_provider_bundle_ids.is_empty() {
                "succeeded".to_owned()
            } else {
                "external-provider-required".to_owned()
            };
            state.current_bundle = None;
            state.last_error = None;
        });
        self.clear_active(&request_id);
    }

    fn clear_active(&self, request_id: &str) {
        if let Ok(mut active) = self.active.lock()
            && active.as_ref().is_some_and(|(id, _)| id == request_id)
        {
            *active = None;
        }
    }

    fn cancel(&self, request_id: &str) -> Result<(), String> {
        let active = self
            .active
            .lock()
            .map_err(|_| "active provisioning state is poisoned".to_owned())?;
        let Some((active_id, token)) = active.as_ref() else {
            return Err("no provisioning request is running".to_owned());
        };
        if active_id != request_id {
            return Err("provisioning request ID does not match the active request".to_owned());
        }
        token.cancel();
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct ProvisionApi {
    runtime: Runtime,
}

#[interface(name = "org.devcore.Provision1")]
impl ProvisionApi {
    fn catalog_json(&self) -> fdo::Result<String> {
        catalog_json().map_err(|error| fdo::Error::Failed(format!("catalog encode: {error}")))
    }

    fn status_json(&self) -> fdo::Result<String> {
        self.runtime.status_json().map_err(fdo::Error::Failed)
    }

    fn plan(
        &self,
        bundle_ids: Vec<String>,
        profile: &str,
        available_disk_bytes: u64,
        consent: bool,
        environment_image: &str,
    ) -> fdo::Result<String> {
        let request = build_request(
            bundle_ids,
            profile,
            available_disk_bytes,
            consent,
            environment_image,
        )?;
        let plan = request.plan().map_err(invalid_args)?;
        plan_json(&plan).map_err(|error| fdo::Error::Failed(format!("plan encode: {error}")))
    }

    fn start(
        &self,
        bundle_ids: Vec<String>,
        profile: &str,
        available_disk_bytes: u64,
        consent: bool,
        environment_image: &str,
    ) -> fdo::Result<String> {
        let request = build_request(
            bundle_ids,
            profile,
            available_disk_bytes,
            consent,
            environment_image,
        )?;
        request.validate_consent().map_err(invalid_args)?;
        let plan = request.plan().map_err(invalid_args)?;
        self.runtime
            .start(request, plan)
            .map_err(fdo::Error::Failed)
    }

    fn cancel(&self, request_id: &str) -> fdo::Result<()> {
        self.runtime.cancel(request_id).map_err(invalid_args)
    }
}

fn build_request(
    bundle_ids: Vec<String>,
    profile: &str,
    available_disk_bytes: u64,
    consent: bool,
    environment_image: &str,
) -> fdo::Result<ProvisionRequest> {
    let profile = match profile {
        "low" => MachineProfile::Low,
        "balanced" => MachineProfile::Balanced,
        "standard" => MachineProfile::Standard,
        "workstation" => MachineProfile::Workstation,
        _ => return Err(invalid_args(format!("unknown machine profile {profile:?}"))),
    };
    Ok(ProvisionRequest {
        bundle_ids,
        profile,
        available_disk_bytes,
        consent,
        environment_image: (!environment_image.is_empty()).then(|| environment_image.to_owned()),
    })
}

fn probe_network() -> NetworkState {
    let argv = network_probe_argv();
    let request = ExecutionRequest::from_argv("provision-network", argv);
    let result = ExecutionEngine::new().run(
        &request,
        &CancellationToken::new(),
        ExecutionLimits {
            max_output_bytes: 4096,
        },
    );
    match result {
        Ok(result) if result.state == JobState::Succeeded => NetworkState::Connected,
        Ok(result) if result.state == JobState::Failed && result.exit_code == Some(1) => {
            NetworkState::Offline
        }
        _ => NetworkState::Unknown,
    }
}

fn next_request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    format!(
        "provision-{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn invalid_args(error: impl ToString) -> fdo::Error {
    fdo::Error::InvalidArgs(error.to_string())
}

fn state_path() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(path).join("devcore/provisioning.json"));
    }
    env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".local/state/devcore/provisioning.json"))
        .ok_or_else(|| "XDG_STATE_HOME or HOME is required for provisioning state".to_owned())
}

fn main() -> Result<(), Box<dyn Error>> {
    zbus::block_on(async {
        let runtime = Runtime::load(state_path()?).map_err(|error| error.to_string())?;
        let _connection = Builder::session()
            .map_err(|error| error.to_string())?
            .name(BUS_NAME)
            .map_err(|error| error.to_string())?
            .serve_at(OBJECT_PATH, ProvisionApi { runtime })
            .map_err(|error| error.to_string())?
            .build()
            .await
            .map_err(|error| error.to_string())?;
        pending::<()>().await;
        Ok::<(), String>(())
    })?;
    Ok(())
}

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
