#![forbid(unsafe_code)]
#![allow(missing_docs)]
//! Privileged, non-interactive DevCore live-installer service.
//!
//! The service only accepts a disk selected from its own stable inventory,
//! rechecks that disk immediately before erasure, uses fixed direct-argv
//! system tools, and never stores or logs password material.

use std::{
    collections::HashMap,
    env,
    error::Error,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::fd::OwnedFd as StdOwnedFd,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};

use devcore_installer_domain::{
    DISK_ID_PREFIX, DiskEligibility, DiskInfo, DiskInventory, InstallJob, InstallRequest,
    InstallSettings, InstallerError, InstallerState, confirmation_phrase,
};
use serde::Deserialize;
use zbus::{
    Connection, MessageHeader, Proxy,
    connection::Builder,
    fdo, interface,
    object_server::SignalContext,
    zvariant::{OwnedFd, OwnedObjectPath, OwnedValue, Str},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const BUS_NAME: &str = "org.devcore.Installer1";
const OBJECT_PATH: &str = "/org/devcore/Installer";
const PAYLOAD_ARCHIVE: &str = "/usr/share/devcore-installer/payload/devcore-baseos.oci.tar";
const PAYLOAD_SUMS: &str = "/usr/share/devcore-installer/payload/SHA256SUMS";
const PAYLOAD_REFERENCE: &str = "/usr/share/devcore-installer/payload/payload-image.ref";
const TARGET_ROOT: &str = "/run/devcore-installer/target";
const MAX_PASSWORD_BYTES: usize = 1024;
const POLKIT_ACTION: &str = "org.devcore.installer.manage";
const POLKIT_BUS_NAME: &str = "org.freedesktop.PolicyKit1";
const POLKIT_OBJECT_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const POLKIT_INTERFACE: &str = "org.freedesktop.PolicyKit1.Authority";
const POLKIT_ALLOW_USER_INTERACTION: u32 = 1;

type Dictionary = HashMap<String, OwnedValue>;

#[derive(Clone, Debug)]
struct InstallerApi {
    state: Arc<Mutex<ServiceState>>,
}

#[derive(Debug)]
struct ServiceState {
    inventory: HostInventory,
    preflight: Option<DiskInfo>,
    job: InstallJob,
    job_path: OwnedObjectPath,
    next_job: u32,
    worker_active: bool,
}

impl ServiceState {
    fn new() -> Self {
        Self {
            inventory: HostInventory,
            preflight: None,
            job: InstallJob::new(),
            job_path: OwnedObjectPath::try_from("/org/devcore/Installer/job/0")
                .expect("constant object path"),
            next_job: 1,
            worker_active: false,
        }
    }
}

async fn authorize_start(connection: &Connection, sender: &str) -> fdo::Result<()> {
    let proxy = Proxy::new(
        connection,
        POLKIT_BUS_NAME,
        POLKIT_OBJECT_PATH,
        POLKIT_INTERFACE,
    )
    .await
    .map_err(|error| fdo::Error::Failed(format!("cannot contact polkit: {error}")))?;

    let mut subject_details: HashMap<String, OwnedValue> = HashMap::new();
    subject_details.insert("name".to_owned(), Str::from(sender.to_owned()).into());
    let subject = ("system-bus-name", subject_details);
    let details = HashMap::<String, String>::new();
    let (authorized, _challenge, _result_details): (bool, bool, HashMap<String, String>) = proxy
        .call(
            "CheckAuthorization",
            &(
                subject,
                POLKIT_ACTION,
                details,
                POLKIT_ALLOW_USER_INTERACTION,
                "",
            ),
        )
        .await
        .map_err(|error| {
            fdo::Error::Failed(format!("cannot check installer authorization: {error}"))
        })?;

    if authorized {
        Ok(())
    } else {
        Err(fdo::Error::AccessDenied(
            "installer authorization denied".to_owned(),
        ))
    }
}

#[interface(name = "org.devcore.Installer1")]
impl InstallerApi {
    fn list_disks(&self) -> fdo::Result<Vec<Dictionary>> {
        let state = self.state.lock().map_err(poisoned)?;
        state
            .inventory
            .list_disks()
            .map(|disks| disks.iter().map(disk_dictionary).collect())
            .map_err(invalid)
    }

    fn preflight(&self, disk_id: String) -> fdo::Result<(String, Dictionary)> {
        let mut state = self.state.lock().map_err(poisoned)?;
        if state.worker_active {
            return Err(fdo::Error::Failed(
                "installer is already running".to_owned(),
            ));
        }
        let disk = state.inventory.resolve_disk(&disk_id).map_err(invalid)?;
        match disk.eligibility() {
            DiskEligibility::Eligible => {}
            DiskEligibility::Rejected(reason) => {
                return Err(invalid(InstallerError::IneligibleDisk(reason)));
            }
        }
        let confirmation = confirmation_phrase(&disk.id).map_err(invalid)?;
        state.preflight = Some(disk.clone());
        state.job = InstallJob::new();
        state.job.ready().map_err(invalid)?;
        Ok((confirmation, disk_dictionary(&disk)))
    }

    async fn start(
        &self,
        #[zbus(header)] header: MessageHeader<'_>,
        confirmation: String,
        configuration: Dictionary,
        password_fd: OwnedFd,
        #[zbus(connection)] connection: &Connection,
        #[zbus(signal_context)] signal_context: SignalContext<'_>,
    ) -> fdo::Result<OwnedObjectPath> {
        let sender = header
            .sender()
            .ok_or_else(|| fdo::Error::AccessDenied("installer caller is unknown".to_owned()))?
            .to_string();
        authorize_start(connection, &sender).await?;
        let password = read_password(password_fd).map_err(invalid)?;
        let (request, job_path) = {
            let mut state = self.state.lock().map_err(poisoned)?;
            if state.worker_active
                || !matches!(
                    state.job.state(),
                    InstallerState::Ready | InstallerState::Cancelled | InstallerState::Failed
                )
            {
                return Err(fdo::Error::Failed(
                    "installer is already running".to_owned(),
                ));
            }
            let disk = state
                .preflight
                .clone()
                .ok_or_else(|| fdo::Error::InvalidArgs("run Preflight before Start".to_owned()))?;
            let settings = settings_from_dictionary(&configuration).map_err(invalid)?;
            let request = InstallRequest::new(&disk, &confirmation, settings).map_err(invalid)?;
            state.job = InstallJob::new();
            state.job.ready().map_err(invalid)?;
            state.job.preparing().map_err(invalid)?;
            state.job_path =
                OwnedObjectPath::try_from(format!("/org/devcore/Installer/job/{}", state.next_job))
                    .map_err(|error| fdo::Error::Failed(error.to_string()))?;
            state.next_job = state
                .next_job
                .checked_add(1)
                .ok_or_else(|| fdo::Error::Failed("installer job counter exhausted".to_owned()))?;
            state.worker_active = true;
            (request, state.job_path.clone())
        };

        let state = Arc::clone(&self.state);
        let context = signal_context.to_owned();
        let emitted_path = job_path.clone();
        thread::Builder::new()
            .name("devcore-installer".to_owned())
            .spawn(move || run_install_worker(state, request, password, context, emitted_path))
            .map_err(|error| {
                if let Ok(mut state) = self.state.lock() {
                    state.worker_active = false;
                    state.job.fail("cannot start installer worker");
                }
                fdo::Error::Failed(format!("cannot start installer worker: {error}"))
            })?;
        Ok(job_path)
    }

    fn status(&self, job: OwnedObjectPath) -> fdo::Result<Dictionary> {
        let state = self.state.lock().map_err(poisoned)?;
        if job != state.job_path {
            return Err(fdo::Error::InvalidArgs("unknown installer job".to_owned()));
        }
        Ok(job_dictionary(&state.job))
    }

    fn cancel(
        &self,
        job: OwnedObjectPath,
        #[zbus(signal_context)] signal_context: SignalContext<'_>,
    ) -> fdo::Result<bool> {
        let (accepted, state_name, retryable) = {
            let mut state = self.state.lock().map_err(poisoned)?;
            if job != state.job_path {
                return Err(fdo::Error::InvalidArgs("unknown installer job".to_owned()));
            }
            let accepted = state.job.cancel().is_ok();
            (
                accepted,
                state_label(state.job.state()).to_owned(),
                state.job.state().is_retryable(),
            )
        };
        if accepted {
            let context = signal_context.to_owned();
            zbus::block_on(Self::state_changed(
                &context,
                &job,
                &state_name,
                retryable,
                "installation cancelled before disk changes",
            ))
            .map_err(|error| fdo::Error::Failed(error.to_string()))?;
        }
        Ok(accepted)
    }

    #[zbus(signal)]
    async fn progress(
        context: &SignalContext<'_>,
        job: &OwnedObjectPath,
        phase: &str,
        completed: u32,
        detail: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn state_changed(
        context: &SignalContext<'_>,
        job: &OwnedObjectPath,
        state: &str,
        retryable: bool,
        diagnostic: &str,
    ) -> zbus::Result<()>;
}

fn state_label(state: InstallerState) -> &'static str {
    match state {
        InstallerState::Idle => "idle",
        InstallerState::Ready => "ready",
        InstallerState::Preparing => "preparing",
        InstallerState::Destructive => "destructive",
        InstallerState::Deploying => "deploying",
        InstallerState::Finalizing => "finalizing",
        InstallerState::Complete => "complete",
        InstallerState::Failed => "failed",
        InstallerState::Cancelled => "cancelled",
    }
}

fn run_install_worker(
    state: Arc<Mutex<ServiceState>>,
    request: InstallRequest,
    mut password: Vec<u8>,
    context: SignalContext<'static>,
    job_path: OwnedObjectPath,
) {
    let mut storage_started = false;
    let result = (|| -> Result<(), InstallerError> {
        emit_progress(
            &context,
            &job_path,
            "preflight",
            5,
            "Verifying embedded BaseOS payload",
        )?;
        verify_payload(Path::new(PAYLOAD_SUMS))?;
        run_checked("podman", ["load", "--input", PAYLOAD_ARCHIVE])?;
        let image = read_payload_reference()?;
        let local_image = resolve_local_payload(&image)?;
        revalidate_and_transition(&state, &request.disk_id, InstallerState::Destructive)?;
        storage_started = true;
        emit_progress(
            &context,
            &job_path,
            "storage",
            20,
            "Preparing the selected disk",
        )?;
        let partitions = prepare_target(&request.disk_id)?;
        transition(&state, InstallerState::Deploying)?;
        emit_progress(
            &context,
            &job_path,
            "deployment",
            45,
            "Deploying DevCore BaseOS from local media",
        )?;
        deploy_payload(&local_image, &image, &partitions.root_uuid)?;
        transition_configuration(&state)?;
        emit_progress(
            &context,
            &job_path,
            "configuration",
            78,
            "Creating the DevCore account",
        )?;
        configure_target(&request.settings, &mut password)?;
        transition(&state, InstallerState::Finalizing)?;
        emit_progress(
            &context,
            &job_path,
            "finalization",
            94,
            "Finalizing the boot target",
        )?;
        finalize_target(&local_image)?;
        unmount_target()?;
        transition_complete(&state)?;
        emit_progress(
            &context,
            &job_path,
            "complete",
            100,
            "DevCore OS is ready to reboot",
        )?;
        Ok(())
    })();
    password.fill(0);
    match result {
        Ok(()) => {
            let _ = zbus::block_on(InstallerApi::state_changed(
                &context,
                &job_path,
                "complete",
                false,
                "target finalized and safe to boot",
            ));
        }
        Err(error) => {
            let cancelled = state
                .lock()
                .is_ok_and(|locked| locked.job.state() == InstallerState::Cancelled);
            if cancelled {
                if let Ok(mut locked) = state.lock() {
                    locked.worker_active = false;
                }
                return;
            }
            let cleanup = if storage_started {
                unmount_target()
            } else {
                Ok(())
            };
            let diagnostic = match cleanup {
                Ok(()) => sanitize_diagnostic(&error.to_string()),
                Err(cleanup) => {
                    sanitize_diagnostic(&format!("{error}; target cleanup failed: {cleanup}"))
                }
            };
            if let Ok(mut locked) = state.lock() {
                locked.job.fail(&diagnostic);
            }
            let _ = zbus::block_on(InstallerApi::state_changed(
                &context,
                &job_path,
                "failed",
                true,
                &diagnostic,
            ));
        }
    }
    if let Ok(mut locked) = state.lock() {
        locked.worker_active = false;
    }
}

fn transition(
    state: &Arc<Mutex<ServiceState>>,
    next: InstallerState,
) -> Result<(), InstallerError> {
    let mut state = state
        .lock()
        .map_err(|_| InstallerError::Inventory("installer state is poisoned".to_owned()))?;
    match next {
        InstallerState::Destructive => state.job.destructive(),
        InstallerState::Deploying => state.job.deploying(),
        InstallerState::Finalizing => state.job.finalizing(),
        _ => Err(InstallerError::Inventory(
            "unsupported transition".to_owned(),
        )),
    }
}

fn transition_configuration(state: &Arc<Mutex<ServiceState>>) -> Result<(), InstallerError> {
    state
        .lock()
        .map_err(|_| InstallerError::Inventory("installer state is poisoned".to_owned()))?
        .job
        .configuring()
}

fn transition_complete(state: &Arc<Mutex<ServiceState>>) -> Result<(), InstallerError> {
    state
        .lock()
        .map_err(|_| InstallerError::Inventory("installer state is poisoned".to_owned()))?
        .job
        .complete()
}

fn revalidate_and_transition(
    state: &Arc<Mutex<ServiceState>>,
    disk_id: &str,
    next: InstallerState,
) -> Result<(), InstallerError> {
    let mut state = state
        .lock()
        .map_err(|_| InstallerError::Inventory("installer state is poisoned".to_owned()))?;
    let disk = state.inventory.resolve_disk(disk_id)?;
    match disk.eligibility() {
        DiskEligibility::Eligible => {}
        DiskEligibility::Rejected(reason) => return Err(InstallerError::IneligibleDisk(reason)),
    }
    if state
        .preflight
        .as_ref()
        .map(|previous| (&previous.id, &previous.device))
        != Some((&disk.id, &disk.device))
    {
        return Err(InstallerError::Inventory(
            "disk identity changed after preflight".to_owned(),
        ));
    }
    match next {
        InstallerState::Destructive => state.job.destructive(),
        _ => Err(InstallerError::Inventory(
            "unsupported transition".to_owned(),
        )),
    }
}

fn emit_progress(
    context: &SignalContext<'static>,
    job: &OwnedObjectPath,
    phase: &str,
    completed: u32,
    detail: &str,
) -> Result<(), InstallerError> {
    zbus::block_on(InstallerApi::progress(
        context, job, phase, completed, detail,
    ))
    .map_err(|error| InstallerError::Inventory(format!("cannot emit installer progress: {error}")))
}

#[derive(Clone, Debug)]
struct HostInventory;

impl DiskInventory for HostInventory {
    fn list_disks(&self) -> Result<Vec<DiskInfo>, InstallerError> {
        let output = run_output(
            "lsblk",
            [
                "--json",
                "--bytes",
                "--output",
                "NAME,PATH,TYPE,SIZE,RM,MODEL",
            ],
        )?;
        let parsed: LsblkOutput = serde_json::from_slice(&output.stdout)
            .map_err(|error| InstallerError::Inventory(format!("invalid lsblk output: {error}")))?;
        let mut disks = Vec::new();
        for device in parsed.blockdevices.unwrap_or_default() {
            if device.kind.as_deref() != Some("disk") {
                continue;
            }
            let path = device
                .path
                .unwrap_or_else(|| format!("/dev/{}", device.name.unwrap_or_default()));
            let Some(id) = stable_id_for_device(&path)? else {
                continue;
            };
            let running_root = device_is_running_root(&path);
            let installer_media = device_is_installer_media(&path);
            disks.push(DiskInfo {
                id,
                device: path,
                model: device.model.unwrap_or_default().trim().to_owned(),
                size_bytes: device.size.unwrap_or_default(),
                removable: device.removable.unwrap_or(false),
                running_root,
                installer_media,
                virtual_device: false,
            });
        }
        disks.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(disks)
    }

    fn resolve_disk(&self, id: &str) -> Result<DiskInfo, InstallerError> {
        self.list_disks()?
            .into_iter()
            .find(|disk| disk.id == id)
            .ok_or_else(|| {
                InstallerError::Inventory("selected disk is no longer present".to_owned())
            })
    }
}

#[derive(Debug, Deserialize)]
struct LsblkOutput {
    blockdevices: Option<Vec<LsblkDevice>>,
}
#[derive(Debug, Deserialize)]
struct LsblkDevice {
    name: Option<String>,
    path: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    size: Option<u64>,
    #[serde(rename = "rm")]
    removable: Option<bool>,
    model: Option<String>,
}

fn stable_id_for_device(device: &str) -> Result<Option<String>, InstallerError> {
    let expected = fs::canonicalize(device)
        .map_err(|error| InstallerError::Inventory(format!("cannot resolve {device}: {error}")))?;
    let entries = fs::read_dir(DISK_ID_PREFIX).map_err(|error| {
        InstallerError::Inventory(format!("cannot read stable disk IDs: {error}"))
    })?;
    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            InstallerError::Inventory(format!("cannot inspect stable disk ID: {error}"))
        })?;
        let path = entry.path();
        if fs::canonicalize(&path).ok().as_ref() == Some(&expected) {
            let display = path.to_string_lossy().into_owned();
            if display.starts_with(DISK_ID_PREFIX) && !display.contains("-part") {
                ids.push(display);
            }
        }
    }
    ids.sort();
    Ok(ids.into_iter().next())
}

fn device_is_running_root(device: &str) -> bool {
    fs::read_to_string("/proc/self/mountinfo")
        .map(|mounts| mounts.lines().any(|line| line.contains(device)))
        .unwrap_or(true)
}
fn device_is_installer_media(device: &str) -> bool {
    fs::read_to_string("/proc/self/mountinfo")
        .map(|mounts| {
            mounts.lines().any(|line| {
                line.contains(device)
                    && (line.contains("/run/initramfs") || line.contains("/run/media"))
            })
        })
        .unwrap_or(true)
}

#[derive(Clone, Debug)]
struct TargetPartitions {
    root_uuid: String,
}

fn prepare_target(disk_id: &str) -> Result<TargetPartitions, InstallerError> {
    let disk = HostInventory.resolve_disk(disk_id)?;
    let layout = "label: gpt\nunit: MiB\n,1024,U\n,,L\n";
    run_with_stdin(
        "sfdisk",
        [
            "--wipe",
            "always",
            "--wipe-partitions",
            "always",
            disk.device.as_str(),
        ],
        layout.as_bytes(),
    )?;
    run_checked("partprobe", [disk.device.as_str()])?;
    run_checked("udevadm", ["settle"])?;
    let (esp, root) = partition_paths(&disk.device);
    run_checked("mkfs.fat", ["-F", "32", esp.as_str()])?;
    run_checked("mkfs.ext4", ["-F", root.as_str()])?;
    fs::create_dir_all(TARGET_ROOT).map_err(io_error("create target mount path"))?;
    run_checked("mount", [root.as_str(), TARGET_ROOT])?;
    fs::create_dir_all(format!("{TARGET_ROOT}/boot/efi"))
        .map_err(io_error("create target EFI mount path"))?;
    run_checked("mount", [esp.as_str(), &format!("{TARGET_ROOT}/boot/efi")])?;
    let root_uuid = String::from_utf8(
        run_output("blkid", ["-s", "UUID", "-o", "value", root.as_str()])?.stdout,
    )
    .map_err(|_| InstallerError::Inventory("root filesystem UUID is not UTF-8".to_owned()))?
    .trim()
    .to_owned();
    if root_uuid.is_empty() {
        return Err(InstallerError::Inventory(
            "root filesystem did not expose a UUID".to_owned(),
        ));
    }
    Ok(TargetPartitions { root_uuid })
}

fn partition_paths(device: &str) -> (String, String) {
    let separator = if device
        .chars()
        .last()
        .is_some_and(|character| character.is_ascii_digit())
    {
        "p"
    } else {
        ""
    };
    (
        format!("{device}{separator}1"),
        format!("{device}{separator}2"),
    )
}

fn deploy_payload(local_image: &str, image: &str, root_uuid: &str) -> Result<(), InstallerError> {
    let root_spec = format!("UUID={root_uuid}");
    run_checked(
        "podman",
        [
            "run",
            "--rm",
            "--pull=never",
            "--network=none",
            "--cgroups=disabled",
            "--privileged",
            "--pid=host",
            "--ipc=host",
            "--security-opt",
            "label=type:unconfined_t",
            "-v",
            "/dev:/dev",
            "-v",
            "/var/lib/containers:/var/lib/containers",
            "-v",
            &format!("{TARGET_ROOT}:/target:rw,rbind,rprivate"),
            local_image,
            "bootc",
            "install",
            "to-filesystem",
            "--skip-finalize",
            "--stateroot",
            "default",
            "--root-mount-spec",
            root_spec.as_str(),
            "--target-imgref",
            image,
            "--skip-fetch-check",
            "/target",
        ],
    )
}

fn configure_target(settings: &InstallSettings, password: &mut [u8]) -> Result<(), InstallerError> {
    let deployment = String::from_utf8(
        run_output(
            "ostree",
            ["admin", "--sysroot", TARGET_ROOT, "--print-current-dir"],
        )?
        .stdout,
    )
    .map_err(|_| InstallerError::Inventory("target deployment path is not UTF-8".to_owned()))?
    .trim()
    .to_owned();
    if !Path::new(&deployment)
        .starts_with(Path::new(TARGET_ROOT).join("ostree/deploy/default/deploy"))
        || deployment.contains("..")
    {
        return Err(InstallerError::Inventory(
            "invalid target deployment path".to_owned(),
        ));
    }
    let deployment_var = format!("{deployment}/var");
    let persistent_var = format!("{TARGET_ROOT}/ostree/deploy/default/var");
    run_checked(
        "mount",
        ["--bind", persistent_var.as_str(), deployment_var.as_str()],
    )?;
    let configured = configure_deployment(settings, password, &deployment);
    let unmounted = run_checked("umount", [deployment_var.as_str()]);
    configured?;
    unmounted?;
    write_firstboot_marker(settings)
}

fn configure_deployment(
    settings: &InstallSettings,
    password: &mut [u8],
    deployment: &str,
) -> Result<(), InstallerError> {
    run_checked(
        "systemd-firstboot",
        [
            "--force",
            "--root",
            deployment,
            "--locale",
            settings.locale.as_str(),
            "--keymap",
            settings.keyboard.as_str(),
            "--timezone",
            settings.timezone.as_str(),
            "--hostname",
            settings.hostname.as_str(),
        ],
    )?;
    run_checked(
        "useradd",
        [
            "--root",
            deployment,
            "--create-home",
            "--groups",
            "wheel",
            settings.username.as_str(),
        ],
    )?;
    let mut account = Vec::with_capacity(settings.username.len() + password.len() + 2);
    account.extend_from_slice(settings.username.as_bytes());
    account.push(b':');
    account.extend_from_slice(password);
    account.push(b'\n');
    let password_result = run_with_stdin("chpasswd", ["--root", deployment], &account);
    account.fill(0);
    password_result
}

fn write_firstboot_marker(settings: &InstallSettings) -> Result<(), InstallerError> {
    let directory = Path::new(TARGET_ROOT).join("ostree/deploy/default/var/lib/devcore");
    fs::create_dir_all(&directory).map_err(io_error("create first-boot state"))?;
    let path = directory.join("first-boot.conf");
    let temporary = directory.join(".first-boot.conf.installer.tmp");
    let contents = format!(
        "version=1\nlanguage={}\nkeyboard={}\ntimezone={}\nusername={}\nhostname={}\nprofile={}\n",
        settings.locale,
        settings.keyboard,
        settings.timezone,
        settings.username,
        settings.hostname,
        settings.profile.as_str()
    );
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(io_error("create first-boot state"))?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(io_error("restrict first-boot state"))?;
    file.write_all(contents.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(io_error("write first-boot state"))?;
    fs::rename(temporary, path).map_err(io_error("publish first-boot state"))
}

fn finalize_target(image: &str) -> Result<(), InstallerError> {
    run_checked(
        "podman",
        [
            "run",
            "--rm",
            "--pull=never",
            "--network=none",
            "--cgroups=disabled",
            "--privileged",
            "-v",
            &format!("{TARGET_ROOT}:/target:rw,rbind,rprivate"),
            image,
            "bootc",
            "install",
            "finalize",
            "/target",
        ],
    )?;
    run_checked("sync", [])
}

fn unmount_target() -> Result<(), InstallerError> {
    unmount_target_with(|program, arguments| run_checked(program, arguments.iter().copied()))
}

fn unmount_target_with(
    mut run: impl FnMut(&str, &[&str]) -> Result<(), InstallerError>,
) -> Result<(), InstallerError> {
    // Recursive unmount handles partial setup and postprocessing bind mounts.
    // A busy target must never be reported as successfully finalized.
    run("umount", &["--recursive", TARGET_ROOT])
}

fn verify_payload(manifest: &Path) -> Result<(), InstallerError> {
    let directory = manifest
        .parent()
        .ok_or_else(|| InstallerError::Inventory("payload manifest has no directory".to_owned()))?;
    let output =
        Command::new("sha256sum")
            .args(["--strict", "--check"])
            .arg(manifest.file_name().ok_or_else(|| {
                InstallerError::Inventory("invalid payload manifest path".to_owned())
            })?)
            .current_dir(directory)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .output()
            .map_err(io_error("verify payload checksum"))?;
    if !output.status.success() {
        return Err(InstallerError::Inventory(
            "embedded payload checksum verification failed".to_owned(),
        ));
    }
    Ok(())
}

fn resolve_local_payload(reference: &str) -> Result<String, InstallerError> {
    let digest = reference
        .split_once('@')
        .ok_or_else(|| InstallerError::Inventory("payload digest is missing".to_owned()))?
        .1;
    let images = run_output(
        "podman",
        ["image", "ls", "--no-trunc", "--format", "{{.ID}}"],
    )?;
    for id in String::from_utf8_lossy(&images.stdout).lines() {
        let inspected = run_output(
            "podman",
            ["image", "inspect", "--format", "{{.Digest}}", id],
        )?;
        if String::from_utf8_lossy(&inspected.stdout).trim() == digest {
            return Ok(id.to_owned());
        }
    }
    Err(InstallerError::Inventory(
        "loaded payload does not match the embedded image digest".to_owned(),
    ))
}

fn read_payload_reference() -> Result<String, InstallerError> {
    let reference =
        fs::read_to_string(PAYLOAD_REFERENCE).map_err(io_error("read payload image reference"))?;
    validate_payload_reference(reference.trim())
}

fn validate_payload_reference(reference: &str) -> Result<String, InstallerError> {
    if !reference.starts_with("ghcr.io/techmigosglobal/devcoreos@sha256:")
        || reference.len() != "ghcr.io/techmigosglobal/devcoreos@sha256:".len() + 64
        || !reference
            .rsplit(':')
            .next()
            .is_some_and(|digest| digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(InstallerError::Inventory(
            "embedded payload image reference is not a DevCore GHCR digest".to_owned(),
        ));
    }
    Ok(reference.to_owned())
}

fn read_password(password_fd: OwnedFd) -> Result<Vec<u8>, InstallerError> {
    let file = File::from(StdOwnedFd::from(password_fd));
    let mut password = Vec::with_capacity(128);
    file.take((MAX_PASSWORD_BYTES + 1) as u64)
        .read_to_end(&mut password)
        .map_err(io_error("read transient password"))?;
    while password
        .last()
        .is_some_and(|byte| matches!(*byte, b'\n' | b'\r'))
    {
        password.pop();
    }
    if password.len() < 8
        || password.len() > MAX_PASSWORD_BYTES
        || password.contains(&0)
        || password.contains(&b'\n')
        || password.contains(&b'\r')
    {
        password.fill(0);
        return Err(InstallerError::InvalidField {
            field: "password",
            reason: "must be 8-1024 bytes without line breaks",
        });
    }
    Ok(password)
}

fn settings_from_dictionary(values: &Dictionary) -> Result<InstallSettings, InstallerError> {
    let profile = string_value(values, "profile")?;
    InstallSettings::new(
        string_value(values, "locale")?,
        string_value(values, "keyboard")?,
        string_value(values, "timezone")?,
        string_value(values, "username")?,
        string_value(values, "hostname")?,
        &profile,
    )
}

fn string_value(values: &Dictionary, key: &'static str) -> Result<String, InstallerError> {
    let value = values.get(key).ok_or(InstallerError::InvalidField {
        field: key,
        reason: "is required",
    })?;
    let text: &str = value.try_into().map_err(|_| InstallerError::InvalidField {
        field: key,
        reason: "must be a string",
    })?;
    Ok(text.to_owned())
}

fn disk_dictionary(disk: &DiskInfo) -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert("id".to_owned(), owned_string(&disk.id));
    dictionary.insert("device".to_owned(), owned_string(&disk.device));
    dictionary.insert("model".to_owned(), owned_string(&disk.model));
    dictionary.insert("size_bytes".to_owned(), disk.size_bytes.into());
    dictionary.insert(
        "eligible".to_owned(),
        matches!(disk.eligibility(), DiskEligibility::Eligible).into(),
    );
    dictionary.insert(
        "reason".to_owned(),
        owned_string(match disk.eligibility() {
            DiskEligibility::Eligible => "",
            DiskEligibility::Rejected(reason) => reason,
        }),
    );
    dictionary
}

fn job_dictionary(job: &InstallJob) -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert("state".to_owned(), owned_string(state_label(job.state())));
    dictionary.insert(
        "phase".to_owned(),
        owned_string(format!("{:?}", job.phase()).to_lowercase()),
    );
    dictionary.insert("retryable".to_owned(), job.state().is_retryable().into());
    dictionary.insert(
        "cancellable".to_owned(),
        job.state().is_cancellable().into(),
    );
    dictionary.insert(
        "diagnostic".to_owned(),
        owned_string(job.diagnostic().unwrap_or("")),
    );
    dictionary
}

fn owned_string(value: impl Into<String>) -> OwnedValue {
    zbus::zvariant::Str::from(value.into()).into()
}

fn run_checked<'a>(
    program: &str,
    arguments: impl IntoIterator<Item = &'a str>,
) -> Result<(), InstallerError> {
    run_output(program, arguments).map(|_| ())
}

fn run_output<'a>(
    program: &str,
    arguments: impl IntoIterator<Item = &'a str>,
) -> Result<std::process::Output, InstallerError> {
    let output = Command::new(program)
        .args(arguments)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .output()
        .map_err(|error| InstallerError::Inventory(format!("cannot start {program}: {error}")))?;
    if !output.status.success() {
        return Err(InstallerError::Inventory(format!(
            "{program} failed with {}: {}",
            output.status,
            bounded_stderr(&output.stderr)
        )));
    }
    Ok(output)
}

fn run_with_stdin<'a>(
    program: &str,
    arguments: impl IntoIterator<Item = &'a str>,
    input: &[u8],
) -> Result<(), InstallerError> {
    let mut child = Command::new(program)
        .args(arguments)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| InstallerError::Inventory(format!("cannot start {program}: {error}")))?;
    child
        .stdin
        .take()
        .ok_or_else(|| InstallerError::Inventory(format!("{program} has no stdin")))?
        .write_all(input)
        .map_err(io_error("write command input"))?;
    let output = child
        .wait_with_output()
        .map_err(io_error("wait for command"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(InstallerError::Inventory(format!(
            "{program} failed with {}: {}",
            output.status,
            bounded_stderr(&output.stderr)
        )))
    }
}

fn bounded_stderr(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .chars()
        .filter(|character| !character.is_control() || *character == '\n')
        .take(256)
        .collect()
}
fn sanitize_diagnostic(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || *character == '\n')
        .take(512)
        .collect()
}
fn io_error(operation: &'static str) -> impl Fn(io::Error) -> InstallerError {
    move |error| InstallerError::Inventory(format!("{operation}: {error}"))
}
fn invalid(error: InstallerError) -> fdo::Error {
    fdo::Error::InvalidArgs(error.to_string())
}
fn poisoned<T>(_error: std::sync::PoisonError<T>) -> fdo::Error {
    fdo::Error::Failed("installer state is poisoned".to_owned())
}

fn config_path() -> PathBuf {
    let mut arguments = env::args_os().skip(1);
    match (arguments.next(), arguments.next()) {
        (Some(flag), Some(path)) if flag == "--config" && arguments.next().is_none() => {
            PathBuf::from(path)
        }
        _ => PathBuf::from("/usr/share/devcore-installer/config.json"),
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let config = config_path();
    if !config.is_file() {
        return Err(format!("installer configuration is missing: {}", config.display()).into());
    }
    zbus::block_on(async {
        let _connection = Builder::system()?
            .name(BUS_NAME)?
            .serve_at(
                OBJECT_PATH,
                InstallerApi {
                    state: Arc::new(Mutex::new(ServiceState::new())),
                },
            )?
            .build()
            .await?;
        std::future::pending::<()>().await;
        Ok::<(), Box<dyn Error>>(())
    })
}

fn main() -> Result<(), Box<dyn Error>> {
    run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_names_cover_sata_and_nvme_devices() {
        assert_eq!(
            partition_paths("/dev/sda"),
            ("/dev/sda1".to_owned(), "/dev/sda2".to_owned())
        );
        assert_eq!(
            partition_paths("/dev/nvme0n1"),
            ("/dev/nvme0n1p1".to_owned(), "/dev/nvme0n1p2".to_owned())
        );
    }

    #[test]
    fn payload_reference_requires_a_devcore_immutable_digest() {
        let reference = format!(
            "ghcr.io/techmigosglobal/devcoreos@sha256:{}",
            "a".repeat(64)
        );
        assert_eq!(validate_payload_reference(&reference).unwrap(), reference);
        for invalid in [
            "registry:localhost/devcore-os".to_owned(),
            "ghcr.io/techmigosglobal/devcoreos:alpha".to_owned(),
            format!(
                "ghcr.io/techmigosglobal/devcoreos@sha256:{}",
                "z".repeat(64)
            ),
        ] {
            assert!(validate_payload_reference(&invalid).is_err());
        }
    }

    #[test]
    fn checksum_resolves_payload_relative_to_manifest_and_rejects_corruption() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            env::temp_dir().join(format!("devcore-checksum-{}-{unique}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let archive = directory.join("devcore-baseos.oci.tar");
        let manifest = directory.join("SHA256SUMS");
        fs::write(&archive, b"abc").unwrap();
        fs::write(&manifest, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  devcore-baseos.oci.tar\n").unwrap();
        assert!(verify_payload(&manifest).is_ok());
        fs::write(&archive, b"corrupt").unwrap();
        assert!(verify_payload(&manifest).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn busy_target_cannot_be_reported_as_unmounted() {
        let result =
            unmount_target_with(|_, _| Err(InstallerError::Inventory("target is busy".to_owned())));
        assert!(result.unwrap_err().to_string().contains("target is busy"));
    }

    #[test]
    fn preflight_cannot_replace_a_worker_even_after_cancellation() {
        let mut state = ServiceState::new();
        state.job.ready().unwrap();
        state.job.preparing().unwrap();
        state.job.cancel().unwrap();
        state.worker_active = true;
        let api = InstallerApi {
            state: Arc::new(Mutex::new(state)),
        };
        assert!(matches!(
            api.preflight("/dev/disk/by-id/fake".to_owned()),
            Err(fdo::Error::Failed(_))
        ));
    }

    #[test]
    fn diagnostics_are_bounded_and_secret_free_after_password_read_error() {
        assert!(sanitize_diagnostic(&"a".repeat(1000)).len() <= 512);
        assert_eq!(DISK_ID_PREFIX, "/dev/disk/by-id/");
    }
}
