#![forbid(unsafe_code)]
//! Validated, side-effect-free state and input model for the live installer.
//!
//! This crate intentionally cannot execute commands, inspect `/dev`, or retain
//! password bytes.  The privileged daemon owns those boundaries and supplies
//! only validated inventory and transitions to this domain model.

use std::fmt;

use devcore_core::MachineProfile;

/// Minimum target capacity for a supported DevCore installation.
pub const MINIMUM_DISK_BYTES: u64 = 32 * 1024 * 1024 * 1024;
/// Required whole-disk identifier prefix.
pub const DISK_ID_PREFIX: &str = "/dev/disk/by-id/";

/// A physical-disk fact collected by the privileged inventory adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiskInfo {
    /// Stable whole-disk identity selected by the user.
    pub id: String,
    /// Kernel block-device path resolved immediately before use.
    pub device: String,
    /// Human-readable model, if udev reported one.
    pub model: String,
    /// Capacity in bytes.
    pub size_bytes: u64,
    /// Whether the media is removable.
    pub removable: bool,
    /// Whether the disk backs the running live environment.
    pub running_root: bool,
    /// Whether the disk contains the booted installer media.
    pub installer_media: bool,
    /// Whether the device is a loop, ram, or mapper pseudo-device.
    pub virtual_device: bool,
}

impl DiskInfo {
    /// Returns whether this is a supported destructive-installation target.
    pub fn eligibility(&self) -> DiskEligibility {
        if !is_stable_disk_id(&self.id) {
            DiskEligibility::Rejected("disk identity must be a canonical /dev/disk/by-id path")
        } else if self.device.is_empty() || !self.device.starts_with("/dev/") {
            DiskEligibility::Rejected("disk does not resolve to a block-device path")
        } else if self.removable {
            DiskEligibility::Rejected("removable media cannot be erased")
        } else if self.running_root {
            DiskEligibility::Rejected("the running live root cannot be erased")
        } else if self.installer_media {
            DiskEligibility::Rejected("the installer media cannot be erased")
        } else if self.virtual_device {
            DiskEligibility::Rejected("loop, ram, and mapped devices are unsupported")
        } else if self.size_bytes < MINIMUM_DISK_BYTES {
            DiskEligibility::Rejected("disk is smaller than the 32 GiB minimum")
        } else {
            DiskEligibility::Eligible
        }
    }
}

/// Eligibility decision shown by the installer before it can start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiskEligibility {
    /// The target may continue to preflight.
    Eligible,
    /// The target is never offered for destructive installation.
    Rejected(&'static str),
}

/// Non-secret configuration that becomes target-local state after deployment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallSettings {
    /// Locale passed to target configuration.
    pub locale: String,
    /// Keyboard layout passed to target configuration.
    pub keyboard: String,
    /// Time-zone identifier passed to target configuration.
    pub timezone: String,
    /// First normal-user name.
    pub username: String,
    /// Hostname for the installed system.
    pub hostname: String,
    /// Resource profile selected for the target.
    pub profile: MachineProfile,
}

impl InstallSettings {
    /// Validates installation configuration before storage is changed.
    pub fn new(
        locale: impl Into<String>,
        keyboard: impl Into<String>,
        timezone: impl Into<String>,
        username: impl Into<String>,
        hostname: impl Into<String>,
        profile: &str,
    ) -> Result<Self, InstallerError> {
        let settings = Self {
            locale: locale.into(),
            keyboard: keyboard.into(),
            timezone: timezone.into(),
            username: username.into(),
            hostname: hostname.into(),
            profile: match profile {
                "low" => MachineProfile::Low,
                "balanced" => MachineProfile::Balanced,
                "standard" => MachineProfile::Standard,
                "workstation" => MachineProfile::Workstation,
                _ => {
                    return Err(InstallerError::InvalidField {
                        field: "profile",
                        reason: "must be low, balanced, standard, or workstation",
                    });
                }
            },
        };
        settings.validate()?;
        Ok(settings)
    }

    /// Revalidates every field at the privileged boundary.
    pub fn validate(&self) -> Result<(), InstallerError> {
        validate_identifier("locale", &self.locale, 64, |c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
        })?;
        validate_identifier("keyboard", &self.keyboard, 64, |c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_')
        })?;
        if self.timezone.is_empty()
            || self.timezone.len() > 128
            || self.timezone.starts_with('/')
            || self.timezone.contains("..")
            || !self
                .timezone
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '+' | '-'))
        {
            return Err(InstallerError::InvalidField {
                field: "timezone",
                reason: "must be a bounded time-zone identifier without traversal",
            });
        }
        if self.username.is_empty()
            || self.username.len() > 32
            || !self
                .username
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
            || !self
                .username
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
            || matches!(
                self.username.as_str(),
                "root" | "daemon" | "bin" | "devcore" | "devcore-live"
            )
        {
            return Err(InstallerError::InvalidField {
                field: "username",
                reason: "must be a non-reserved Linux username",
            });
        }
        if self.hostname.is_empty()
            || self.hostname.len() > 63
            || self.hostname.starts_with('-')
            || self.hostname.ends_with('-')
            || !self
                .hostname
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(InstallerError::InvalidField {
                field: "hostname",
                reason: "must be a bounded hostname without separators",
            });
        }
        Ok(())
    }
}

/// A validated, non-secret request to install to one selected disk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallRequest {
    /// Stable target disk ID.
    pub disk_id: String,
    /// Non-secret target settings.
    pub settings: InstallSettings,
}

impl InstallRequest {
    /// Validates the selected disk, explicit phrase, and target settings.
    pub fn new(
        disk: &DiskInfo,
        confirmation: &str,
        settings: InstallSettings,
    ) -> Result<Self, InstallerError> {
        match disk.eligibility() {
            DiskEligibility::Eligible => {}
            DiskEligibility::Rejected(reason) => {
                return Err(InstallerError::IneligibleDisk(reason));
            }
        }
        let expected = confirmation_phrase(&disk.id)?;
        if confirmation != expected {
            return Err(InstallerError::InvalidConfirmation);
        }
        settings.validate()?;
        Ok(Self {
            disk_id: disk.id.clone(),
            settings,
        })
    }
}

/// Installation lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallerState {
    /// The user has not started an install.
    Idle,
    /// Inputs passed preflight and cancellation remains safe.
    Ready,
    /// Archive verification or immutable image loading is running.
    Preparing,
    /// Disk changes have begun; cancellation is intentionally refused.
    Destructive,
    /// Target deployment and configuration are running.
    Deploying,
    /// Target finalization and unmounting are running.
    Finalizing,
    /// The target was finalized and is safe to boot.
    Complete,
    /// Failure requires review/retry; the target is never described as bootable.
    Failed,
    /// Cancellation happened before disk writes.
    Cancelled,
}

impl InstallerState {
    /// Returns whether a cancel request is safe in this state.
    pub const fn is_cancellable(self) -> bool {
        matches!(self, Self::Ready | Self::Preparing)
    }

    /// Returns whether a restart/retry may be offered.
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Failed | Self::Cancelled)
    }
}

/// Visible progress phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallPhase {
    /// Validate disk identity and payload archive.
    Preflight,
    /// Prepare partitions and filesystems.
    Storage,
    /// Deploy the immutable payload.
    Deployment,
    /// Apply account and target-local configuration.
    Configuration,
    /// Finalize the bootable target.
    Finalization,
}

/// Side-effect-free lifecycle guard used by the D-Bus service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallJob {
    state: InstallerState,
    phase: InstallPhase,
    diagnostic: Option<String>,
}

impl Default for InstallJob {
    fn default() -> Self {
        Self::new()
    }
}

impl InstallJob {
    /// Creates an idle job.
    pub const fn new() -> Self {
        Self {
            state: InstallerState::Idle,
            phase: InstallPhase::Preflight,
            diagnostic: None,
        }
    }

    /// Current installer state.
    pub const fn state(&self) -> InstallerState {
        self.state
    }
    /// Current progress phase.
    pub const fn phase(&self) -> InstallPhase {
        self.phase
    }
    /// Sanitized failure diagnostic, if any.
    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }

    /// Marks inputs ready after a successful preflight.
    pub fn ready(&mut self) -> Result<(), InstallerError> {
        self.transition(InstallerState::Ready, InstallPhase::Preflight)
    }
    /// Starts non-destructive archive preparation.
    pub fn preparing(&mut self) -> Result<(), InstallerError> {
        self.transition(InstallerState::Preparing, InstallPhase::Preflight)
    }
    /// Records the irreversible storage boundary.
    pub fn destructive(&mut self) -> Result<(), InstallerError> {
        self.transition(InstallerState::Destructive, InstallPhase::Storage)
    }
    /// Records image deployment.
    pub fn deploying(&mut self) -> Result<(), InstallerError> {
        self.transition(InstallerState::Deploying, InstallPhase::Deployment)
    }
    /// Records account and target setup.
    pub fn configuring(&mut self) -> Result<(), InstallerError> {
        self.transition(InstallerState::Deploying, InstallPhase::Configuration)
    }
    /// Records boot target finalization.
    pub fn finalizing(&mut self) -> Result<(), InstallerError> {
        self.transition(InstallerState::Finalizing, InstallPhase::Finalization)
    }
    /// Declares success only after target finalization completes.
    pub fn complete(&mut self) -> Result<(), InstallerError> {
        self.transition(InstallerState::Complete, InstallPhase::Finalization)
    }

    /// Cancels only before the destructive boundary.
    pub fn cancel(&mut self) -> Result<(), InstallerError> {
        if !self.state.is_cancellable() {
            return Err(InstallerError::CancellationUnavailable(self.state));
        }
        self.state = InstallerState::Cancelled;
        Ok(())
    }

    /// Records a bounded, non-secret diagnostic for a failed job.
    pub fn fail(&mut self, message: impl AsRef<str>) {
        let message = message.as_ref();
        let bounded = message
            .chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .take(512)
            .collect();
        self.diagnostic = Some(bounded);
        self.state = InstallerState::Failed;
    }

    fn transition(
        &mut self,
        state: InstallerState,
        phase: InstallPhase,
    ) -> Result<(), InstallerError> {
        let allowed = matches!(
            (self.state, state),
            (InstallerState::Idle, InstallerState::Ready)
                | (InstallerState::Ready, InstallerState::Preparing)
                | (InstallerState::Preparing, InstallerState::Destructive)
                | (InstallerState::Destructive, InstallerState::Deploying)
                | (InstallerState::Deploying, InstallerState::Deploying)
                | (InstallerState::Deploying, InstallerState::Finalizing)
                | (InstallerState::Finalizing, InstallerState::Complete)
        );
        if !allowed {
            return Err(InstallerError::InvalidTransition {
                from: self.state,
                to: state,
            });
        }
        self.state = state;
        self.phase = phase;
        Ok(())
    }
}

/// Read-only inventory boundary; implementations live in the privileged daemon.
pub trait DiskInventory {
    /// Lists physical-disk facts without changing the host.
    fn list_disks(&self) -> Result<Vec<DiskInfo>, InstallerError>;
    /// Resolves a stable ID again immediately before destructive work.
    fn resolve_disk(&self, id: &str) -> Result<DiskInfo, InstallerError>;
}

/// Installer validation error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstallerError {
    /// A stable identifier is invalid.
    InvalidDiskId,
    /// The selected disk is unsupported.
    IneligibleDisk(&'static str),
    /// The typed erase phrase did not match the current disk.
    InvalidConfirmation,
    /// A target setting is unsafe or malformed.
    InvalidField {
        /// Field that did not satisfy its target-safe validation rule.
        field: &'static str,
        /// Stable description of the validation rule.
        reason: &'static str,
    },
    /// Cancellation was requested too late.
    CancellationUnavailable(InstallerState),
    /// A state transition bypassed a safety boundary.
    InvalidTransition {
        /// Current lifecycle state.
        from: InstallerState,
        /// Unsafe requested lifecycle state.
        to: InstallerState,
    },
    /// An inventory adapter failed without a safe domain result.
    Inventory(String),
}

impl fmt::Display for InstallerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDiskId => f.write_str("invalid stable disk identifier"),
            Self::IneligibleDisk(reason) => write!(f, "selected disk is ineligible: {reason}"),
            Self::InvalidConfirmation => {
                f.write_str("destructive confirmation does not match selected disk")
            }
            Self::InvalidField { field, reason } => write!(f, "invalid {field}: {reason}"),
            Self::CancellationUnavailable(state) => {
                write!(f, "cancellation is unavailable during {state:?}")
            }
            Self::InvalidTransition { from, to } => {
                write!(f, "invalid installer state transition: {from:?} -> {to:?}")
            }
            Self::Inventory(message) => write!(f, "disk inventory failed: {message}"),
        }
    }
}

impl std::error::Error for InstallerError {}

/// Creates the required phrase for the exact stable disk identity.
pub fn confirmation_phrase(disk_id: &str) -> Result<String, InstallerError> {
    if !is_stable_disk_id(disk_id) {
        return Err(InstallerError::InvalidDiskId);
    }
    let suffix = disk_id
        .rsplit('/')
        .next()
        .ok_or(InstallerError::InvalidDiskId)?;
    let suffix: String = suffix
        .chars()
        .rev()
        .take(8)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    Ok(format!("ERASE {suffix}"))
}

/// Validates a canonical, non-traversing whole-disk symlink path.
pub fn is_stable_disk_id(value: &str) -> bool {
    value.starts_with(DISK_ID_PREFIX)
        && value.len() > DISK_ID_PREFIX.len()
        && !value.contains("..")
        && !value.contains("//")
        && value[DISK_ID_PREFIX.len()..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
}

fn validate_identifier(
    field: &'static str,
    value: &str,
    max_bytes: usize,
    allowed: impl Fn(char) -> bool,
) -> Result<(), InstallerError> {
    if value.is_empty() || value.len() > max_bytes || !value.chars().all(allowed) {
        return Err(InstallerError::InvalidField {
            field,
            reason: "contains unsupported characters or exceeds its limit",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk() -> DiskInfo {
        DiskInfo {
            id: "/dev/disk/by-id/wwn-0x5000c500a1b2c3d4".into(),
            device: "/dev/nvme0n1".into(),
            model: "Test NVMe".into(),
            size_bytes: MINIMUM_DISK_BYTES,
            removable: false,
            running_root: false,
            installer_media: false,
            virtual_device: false,
        }
    }

    fn settings() -> InstallSettings {
        InstallSettings::new(
            "en_US.UTF-8",
            "us",
            "Asia/Kolkata",
            "developer",
            "devcore",
            "balanced",
        )
        .unwrap()
    }

    #[test]
    fn only_stable_large_internal_disks_are_eligible() {
        assert_eq!(disk().eligibility(), DiskEligibility::Eligible);
        for mutate in [
            |d: &mut DiskInfo| d.removable = true,
            |d: &mut DiskInfo| d.running_root = true,
            |d: &mut DiskInfo| d.installer_media = true,
            |d: &mut DiskInfo| d.virtual_device = true,
            |d: &mut DiskInfo| d.size_bytes = MINIMUM_DISK_BYTES - 1,
        ] {
            let mut candidate = disk();
            mutate(&mut candidate);
            assert!(matches!(
                candidate.eligibility(),
                DiskEligibility::Rejected(_)
            ));
        }
    }

    #[test]
    fn request_requires_the_disk_bound_erase_phrase() {
        let disk = disk();
        let phrase = confirmation_phrase(&disk.id).unwrap();
        assert!(InstallRequest::new(&disk, &phrase, settings()).is_ok());
        assert_eq!(
            InstallRequest::new(&disk, "ERASE nvme0n1", settings()),
            Err(InstallerError::InvalidConfirmation)
        );
    }

    #[test]
    fn state_machine_refuses_late_cancellation_and_early_success() {
        let mut job = InstallJob::new();
        assert!(job.complete().is_err());
        job.ready().unwrap();
        job.preparing().unwrap();
        assert!(job.cancel().is_ok());
        assert_eq!(job.state(), InstallerState::Cancelled);
        let mut job = InstallJob::new();
        job.ready().unwrap();
        job.preparing().unwrap();
        job.destructive().unwrap();
        assert_eq!(
            job.cancel(),
            Err(InstallerError::CancellationUnavailable(
                InstallerState::Destructive
            ))
        );
    }

    #[test]
    fn configuration_is_revalidated_at_the_domain_boundary() {
        assert!(
            InstallSettings::new(
                "en_US.UTF-8",
                "us",
                "../UTC",
                "developer",
                "devcore",
                "balanced"
            )
            .is_err()
        );
        assert!(
            InstallSettings::new("en_US.UTF-8", "us", "UTC", "Root", "devcore", "balanced")
                .is_err()
        );
        assert!(
            InstallSettings::new(
                "en_US.UTF-8",
                "us",
                "UTC",
                "developer",
                "invalid.host",
                "balanced"
            )
            .is_err()
        );
    }
}
