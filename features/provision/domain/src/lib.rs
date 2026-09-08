#![forbid(unsafe_code)]
//! Explicit, resource-aware planning for software installed after first boot.
//!
//! DevCore keeps the immutable image small. This crate contains only catalog
//! metadata and direct-argv plans; it never runs a package manager, contacts a
//! network, or writes user state. The user-session `devcore-provisiond`
//! service owns consent, network checks, execution, and resumable state.

use std::{error::Error, fmt};

use devcore_core::MachineProfile;
use serde::{Deserialize, Serialize};

const FLATHUB_REPO: &str = "https://dl.flathub.org/repo/flathub.flatpakrepo";
const MIN_DISK_RESERVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const GIB: u64 = 1024 * 1024 * 1024;

/// Fixed command paths used by post-install plans.
pub const FLATPAK_PROGRAM: &str = "/usr/bin/flatpak";
/// Fixed command path used to fetch an OCI development environment.
pub const PODMAN_PROGRAM: &str = "/usr/bin/podman";
/// Fixed NetworkManager readiness probe.
pub const NETWORK_PROBE_PROGRAM: &str = "/usr/bin/nm-online";

/// How a bundle is delivered after installation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum BundleKind {
    /// A user-scoped Flatpak application.
    Flatpak,
    /// A digest-pinned rootless OCI environment used by Studio.
    OciEnvironment,
    /// A reviewed provider-specific action that DevCore does not download.
    ExternalProvider,
}

impl BundleKind {
    /// Stable JSON/display identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Flatpak => "flatpak",
            Self::OciEnvironment => "oci-environment",
            Self::ExternalProvider => "external-provider",
        }
    }
}

/// A catalog item shown to the user before any network access.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Bundle {
    /// Stable selection identifier.
    pub id: &'static str,
    /// User-facing name.
    pub display_name: &'static str,
    /// Delivery mechanism.
    pub kind: BundleKind,
    /// Short explanation of what will be downloaded/configured.
    pub description: &'static str,
    /// Conservative download/storage estimate in bytes.
    pub estimated_bytes: u64,
    /// Lowest profile that should receive this default.
    pub minimum_profile: MachineProfile,
    /// Marketplace/application ID, when this is a Flatpak.
    pub source: Option<&'static str>,
}

/// The reviewed post-install catalog.
pub const CATALOG: &[Bundle] = &[
    Bundle {
        id: "rust",
        display_name: "Rust development environment",
        kind: BundleKind::OciEnvironment,
        description: "Pull a digest-pinned OCI environment for Rust and Cargo.",
        estimated_bytes: 2 * GIB,
        minimum_profile: MachineProfile::Low,
        source: None,
    },
    Bundle {
        id: "go",
        display_name: "Go development environment",
        kind: BundleKind::OciEnvironment,
        description: "Pull a digest-pinned OCI environment for Go.",
        estimated_bytes: GIB,
        minimum_profile: MachineProfile::Low,
        source: None,
    },
    Bundle {
        id: "python",
        display_name: "Python development environment",
        kind: BundleKind::OciEnvironment,
        description: "Pull a digest-pinned OCI environment for Python.",
        estimated_bytes: GIB,
        minimum_profile: MachineProfile::Low,
        source: None,
    },
    Bundle {
        id: "node",
        display_name: "Node.js development environment",
        kind: BundleKind::OciEnvironment,
        description: "Pull a digest-pinned OCI environment for Node.js, npm, or pnpm.",
        estimated_bytes: 2 * GIB,
        minimum_profile: MachineProfile::Balanced,
        source: None,
    },
    Bundle {
        id: "flutter",
        display_name: "Flutter development environment",
        kind: BundleKind::OciEnvironment,
        description: "Pull a digest-pinned OCI environment for Flutter and Dart.",
        estimated_bytes: 5 * GIB,
        minimum_profile: MachineProfile::Standard,
        source: None,
    },
    Bundle {
        id: "android-sdk",
        display_name: "Android SDK environment",
        kind: BundleKind::OciEnvironment,
        description: "Pull a digest-pinned OCI environment for Android SDK and Gradle builds.",
        estimated_bytes: 8 * GIB,
        minimum_profile: MachineProfile::Standard,
        source: None,
    },
    Bundle {
        id: "android-studio",
        display_name: "Android Studio",
        kind: BundleKind::Flatpak,
        description: "Install Android Studio for the current user from Flathub.",
        estimated_bytes: 3 * GIB,
        minimum_profile: MachineProfile::Standard,
        source: Some("com.google.AndroidStudio"),
    },
    Bundle {
        id: "chrome",
        display_name: "Google Chrome",
        kind: BundleKind::Flatpak,
        description: "Install the reviewed Chrome Flatpak source for the current user.",
        estimated_bytes: GIB,
        minimum_profile: MachineProfile::Balanced,
        source: Some("com.google.Chrome"),
    },
    Bundle {
        id: "java",
        display_name: "Java development environment",
        kind: BundleKind::OciEnvironment,
        description: "Pull a digest-pinned OCI environment for Java and Gradle.",
        estimated_bytes: 2 * GIB,
        minimum_profile: MachineProfile::Balanced,
        source: None,
    },
    Bundle {
        id: "qt",
        display_name: "Qt development environment",
        kind: BundleKind::OciEnvironment,
        description: "Pull a digest-pinned OCI environment for Qt development.",
        estimated_bytes: 3 * GIB,
        minimum_profile: MachineProfile::Standard,
        source: None,
    },
    Bundle {
        id: "cmake",
        display_name: "C/C++ and CMake environment",
        kind: BundleKind::OciEnvironment,
        description: "Pull a digest-pinned OCI environment for C/C++ and CMake.",
        estimated_bytes: 2 * GIB,
        minimum_profile: MachineProfile::Balanced,
        source: None,
    },
    Bundle {
        id: "codex",
        display_name: "Codex provider",
        kind: BundleKind::ExternalProvider,
        description: "Configure Codex from a provider-approved package or binary supplied by the user.",
        estimated_bytes: 0,
        minimum_profile: MachineProfile::Low,
        source: None,
    },
];

/// Network status reported by the service before a download plan runs.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum NetworkState {
    /// NetworkManager reports an active usable connection.
    Connected,
    /// NetworkManager reports that no active connection is ready.
    Offline,
    /// The probe could not be executed or returned an unexpected result.
    Unknown,
}

impl NetworkState {
    /// Stable JSON/display identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Offline => "offline",
            Self::Unknown => "unknown",
        }
    }
}

/// A direct command to be executed by the bounded service supervisor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProvisionOperation {
    /// Catalog bundle owning this operation.
    pub bundle_id: String,
    /// Human-readable step label.
    pub description: String,
    /// Program plus arguments; no shell string is permitted.
    pub argv: Vec<String>,
}

/// A validated plan produced after the user has explicitly selected bundles.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProvisionPlan {
    /// Selected bundle IDs in stable order.
    pub bundle_ids: Vec<String>,
    /// Profile used for resource admission.
    pub profile: MachineProfile,
    /// Sum of conservative download/storage estimates.
    pub estimated_bytes: u64,
    /// Whether a network probe must pass before execution.
    pub network_required: bool,
    /// Steps that can be sent to the bounded direct-argv executor.
    pub operations: Vec<ProvisionOperation>,
    /// Items requiring a user-configured external provider instead of a DevCore download.
    pub external_provider_bundle_ids: Vec<String>,
}

/// A user-selected request. `consent` must be true; there is no implicit install.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProvisionRequest {
    /// Selected catalog IDs.
    pub bundle_ids: Vec<String>,
    /// Hardware profile selected during first boot.
    pub profile: MachineProfile,
    /// Free bytes on the target user's data filesystem.
    pub available_disk_bytes: u64,
    /// Explicit user confirmation for the selected downloads.
    pub consent: bool,
    /// One digest-pinned image used for selected OCI environments.
    pub environment_image: Option<String>,
}

impl ProvisionRequest {
    /// Validates selection, storage headroom, profile suitability, and sources.
    ///
    /// Planning is deliberately allowed before consent so the UI can show the
    /// exact download estimate. Execution callers must also call
    /// [`Self::validate_consent`] before starting the returned operations.
    pub fn plan(&self) -> Result<ProvisionPlan, ProvisionError> {
        if self.bundle_ids.is_empty() {
            return Err(ProvisionError::NoBundles);
        }

        let mut selected = Vec::with_capacity(self.bundle_ids.len());
        for id in &self.bundle_ids {
            let bundle =
                catalog_item(id).ok_or_else(|| ProvisionError::UnknownBundle(id.clone()))?;
            if selected
                .iter()
                .any(|selected: &&Bundle| selected.id == bundle.id)
            {
                continue;
            }
            if profile_rank(self.profile) < profile_rank(bundle.minimum_profile) {
                return Err(ProvisionError::ProfileTooSmall {
                    bundle: bundle.id,
                    profile: self.profile,
                    minimum: bundle.minimum_profile,
                });
            }
            selected.push(bundle);
        }

        let selected_ids = selected
            .iter()
            .map(|bundle| bundle.id.to_owned())
            .collect::<Vec<_>>();
        let estimated_bytes = selected.iter().try_fold(0_u64, |total, bundle| {
            total
                .checked_add(bundle.estimated_bytes)
                .ok_or(ProvisionError::EstimateOverflow)
        })?;
        let required = estimated_bytes
            .checked_add(MIN_DISK_RESERVE_BYTES)
            .ok_or(ProvisionError::EstimateOverflow)?;
        if self.available_disk_bytes < required {
            return Err(ProvisionError::InsufficientDisk {
                available: self.available_disk_bytes,
                required,
            });
        }

        let needs_environment = selected
            .iter()
            .any(|bundle| bundle.kind == BundleKind::OciEnvironment);
        if needs_environment {
            let image = self
                .environment_image
                .as_deref()
                .ok_or(ProvisionError::EnvironmentImageRequired)?;
            validate_digest_image(image)?;
        }

        let mut operations = Vec::new();
        let has_flatpak = selected
            .iter()
            .any(|bundle| bundle.kind == BundleKind::Flatpak);
        if has_flatpak {
            operations.push(ProvisionOperation {
                bundle_id: "flatpak-runtime".to_owned(),
                description: "Configure the user Flathub remote if it is not already present"
                    .to_owned(),
                argv: vec![
                    FLATPAK_PROGRAM.to_owned(),
                    "remote-add".to_owned(),
                    "--user".to_owned(),
                    "--if-not-exists".to_owned(),
                    "flathub".to_owned(),
                    FLATHUB_REPO.to_owned(),
                ],
            });
        }
        let mut external_provider_bundle_ids = Vec::new();
        for bundle in selected {
            match bundle.kind {
                BundleKind::Flatpak => operations.push(ProvisionOperation {
                    bundle_id: bundle.id.to_owned(),
                    description: format!("Install {} for the current user", bundle.display_name),
                    argv: vec![
                        FLATPAK_PROGRAM.to_owned(),
                        "install".to_owned(),
                        "--user".to_owned(),
                        "--noninteractive".to_owned(),
                        "flathub".to_owned(),
                        bundle
                            .source
                            .expect("catalog Flatpak source is present")
                            .to_owned(),
                    ],
                }),
                BundleKind::OciEnvironment => operations.push(ProvisionOperation {
                    bundle_id: bundle.id.to_owned(),
                    description: format!(
                        "Pull the selected OCI environment for {}",
                        bundle.display_name
                    ),
                    argv: vec![
                        PODMAN_PROGRAM.to_owned(),
                        "pull".to_owned(),
                        self.environment_image
                            .as_deref()
                            .expect("validated environment image")
                            .to_owned(),
                    ],
                }),
                BundleKind::ExternalProvider => {
                    external_provider_bundle_ids.push(bundle.id.to_owned())
                }
            }
        }

        Ok(ProvisionPlan {
            bundle_ids: selected_ids,
            profile: self.profile,
            estimated_bytes,
            network_required: !operations.is_empty(),
            operations,
            external_provider_bundle_ids,
        })
    }

    /// Validates the explicit confirmation required before execution.
    pub fn validate_consent(&self) -> Result<(), ProvisionError> {
        if self.consent {
            Ok(())
        } else {
            Err(ProvisionError::ConsentRequired)
        }
    }
}

/// Creates a compact JSON catalog for UI clients.
pub fn catalog_json() -> Result<String, serde_json::Error> {
    serde_json::to_string(CATALOG)
}

/// Returns one catalog item by stable ID.
#[must_use]
pub fn catalog_item(id: &str) -> Option<&'static Bundle> {
    CATALOG.iter().find(|bundle| bundle.id == id)
}

/// Builds the fixed NetworkManager readiness probe.
#[must_use]
pub fn network_probe_argv() -> Vec<String> {
    vec![
        NETWORK_PROBE_PROGRAM.to_owned(),
        "--quiet".to_owned(),
        "--timeout=5".to_owned(),
    ]
}

fn profile_rank(profile: MachineProfile) -> u8 {
    match profile {
        MachineProfile::Low => 0,
        MachineProfile::Balanced => 1,
        MachineProfile::Standard => 2,
        MachineProfile::Workstation => 3,
    }
}

fn validate_digest_image(image: &str) -> Result<(), ProvisionError> {
    let Some((name, digest)) = image.rsplit_once("@sha256:") else {
        return Err(ProvisionError::InvalidEnvironmentImage(image.to_owned()));
    };
    if name.is_empty()
        || name.chars().any(char::is_whitespace)
        || digest.len() != 64
        || !digest
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(ProvisionError::InvalidEnvironmentImage(image.to_owned()));
    }
    Ok(())
}

/// Errors returned before any process or network operation is started.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProvisionError {
    /// User confirmation was not supplied.
    ConsentRequired,
    /// No bundles were selected.
    NoBundles,
    /// Selection is not in the reviewed catalog.
    UnknownBundle(String),
    /// The machine profile is too constrained for the selected bundle.
    ProfileTooSmall {
        /// Bundle that was rejected.
        bundle: &'static str,
        /// Current profile.
        profile: MachineProfile,
        /// Required profile.
        minimum: MachineProfile,
    },
    /// Arithmetic overflow while summing estimates.
    EstimateOverflow,
    /// Free disk space does not meet estimate plus reserve.
    InsufficientDisk {
        /// Observed free bytes.
        available: u64,
        /// Required free bytes.
        required: u64,
    },
    /// At least one OCI environment was selected without a source image.
    EnvironmentImageRequired,
    /// Environment image is not digest pinned.
    InvalidEnvironmentImage(String),
}

impl fmt::Display for ProvisionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConsentRequired => write!(formatter, "explicit user consent is required"),
            Self::NoBundles => write!(formatter, "select at least one developer bundle"),
            Self::UnknownBundle(id) => write!(formatter, "unknown developer bundle {id:?}"),
            Self::ProfileTooSmall {
                bundle,
                profile,
                minimum,
            } => write!(
                formatter,
                "bundle {bundle} requires profile {}, current profile is {}",
                minimum.as_str(),
                profile.as_str()
            ),
            Self::EstimateOverflow => {
                write!(formatter, "developer bundle size estimate overflowed")
            }
            Self::InsufficientDisk {
                available,
                required,
            } => write!(
                formatter,
                "insufficient disk space: {available} bytes available, {required} required including reserve"
            ),
            Self::EnvironmentImageRequired => write!(
                formatter,
                "OCI bundles require a digest-pinned environment image"
            ),
            Self::InvalidEnvironmentImage(image) => write!(
                formatter,
                "invalid digest-pinned environment image {image:?}"
            ),
        }
    }
}

impl Error for ProvisionError {}

/// A JSON-safe snapshot of the immutable catalog and readiness requirements.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProvisionReadiness {
    /// Network state at the time of the status read.
    pub network: NetworkState,
    /// Whether user consent is required for a start.
    pub consent_required: bool,
    /// Minimum free disk reserve in bytes.
    pub minimum_disk_reserve_bytes: u64,
}

/// Returns the fixed readiness metadata shown before selection.
#[must_use]
pub const fn readiness(network: NetworkState) -> ProvisionReadiness {
    ProvisionReadiness {
        network,
        consent_required: true,
        minimum_disk_reserve_bytes: MIN_DISK_RESERVE_BYTES,
    }
}

/// Converts a plan to bounded JSON for D-Bus clients.
pub fn plan_json(plan: &ProvisionPlan) -> Result<String, serde_json::Error> {
    serde_json::to_string(plan)
}

/// Converts a plan error to a stable user-facing status string.
#[must_use]
pub fn plan_error(error: &ProvisionError) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        BundleKind, CATALOG, FLATPAK_PROGRAM, NETWORK_PROBE_PROGRAM, PODMAN_PROGRAM,
        ProvisionError, ProvisionRequest, catalog_json, network_probe_argv,
    };
    use devcore_core::MachineProfile;

    const IMAGE: &str = "quay.io/devcore/toolchains@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const ENOUGH_DISK: u64 = 64 * 1024 * 1024 * 1024;

    #[test]
    fn catalog_keeps_large_payloads_out_of_the_base_image() {
        assert!(CATALOG.iter().any(|bundle| bundle.id == "flutter"));
        assert!(CATALOG.iter().any(|bundle| bundle.id == "android-studio"));
        assert!(CATALOG.iter().any(|bundle| bundle.id == "chrome"));
        assert!(
            CATALOG
                .iter()
                .filter(|bundle| bundle.kind == BundleKind::Flatpak)
                .all(|bundle| bundle.source.is_some())
        );
    }

    #[test]
    fn plan_requires_consent_and_digest_pinned_oci_source() {
        let without_consent = ProvisionRequest {
            bundle_ids: vec!["flutter".to_owned()],
            profile: MachineProfile::Standard,
            available_disk_bytes: ENOUGH_DISK,
            consent: false,
            environment_image: Some(IMAGE.to_owned()),
        };
        assert_eq!(
            without_consent.validate_consent(),
            Err(ProvisionError::ConsentRequired)
        );
        assert!(without_consent.plan().is_ok());

        let without_image = ProvisionRequest {
            consent: true,
            environment_image: None,
            ..without_consent
        };
        assert_eq!(
            without_image.plan(),
            Err(ProvisionError::EnvironmentImageRequired)
        );
    }

    #[test]
    fn profile_and_storage_admission_protect_low_end_machines() {
        let low_machine = ProvisionRequest {
            bundle_ids: vec!["android-studio".to_owned()],
            profile: MachineProfile::Low,
            available_disk_bytes: ENOUGH_DISK,
            consent: true,
            environment_image: None,
        };
        assert!(matches!(
            low_machine.plan(),
            Err(ProvisionError::ProfileTooSmall { .. })
        ));

        let low_disk = ProvisionRequest {
            bundle_ids: vec!["chrome".to_owned()],
            profile: MachineProfile::Balanced,
            available_disk_bytes: 1,
            consent: true,
            environment_image: None,
        };
        assert!(matches!(
            low_disk.plan(),
            Err(ProvisionError::InsufficientDisk { .. })
        ));
    }

    #[test]
    fn plan_uses_only_fixed_direct_argv_and_reports_external_provider() {
        let plan = ProvisionRequest {
            bundle_ids: vec!["chrome".to_owned(), "codex".to_owned(), "rust".to_owned()],
            profile: MachineProfile::Workstation,
            available_disk_bytes: ENOUGH_DISK,
            consent: true,
            environment_image: Some(IMAGE.to_owned()),
        }
        .plan()
        .unwrap();
        assert_eq!(plan.external_provider_bundle_ids, vec!["codex"]);
        assert!(
            plan.operations
                .iter()
                .all(|operation| !operation.argv.iter().any(|arg| arg.contains("sh -c")))
        );
        assert!(
            plan.operations
                .iter()
                .any(|operation| operation.argv[0] == FLATPAK_PROGRAM)
        );
        assert!(
            plan.operations
                .iter()
                .any(|operation| operation.argv[0] == PODMAN_PROGRAM)
        );
        assert_eq!(network_probe_argv()[0], NETWORK_PROBE_PROGRAM);
        assert!(catalog_json().unwrap().contains("android-studio"));
    }

    #[test]
    fn storage_estimate_is_visible_in_the_plan() {
        let plan = ProvisionRequest {
            bundle_ids: vec!["chrome".to_owned()],
            profile: MachineProfile::Balanced,
            available_disk_bytes: ENOUGH_DISK,
            consent: true,
            environment_image: None,
        }
        .plan()
        .unwrap();
        assert!(plan.estimated_bytes > 0);
        assert!(plan.network_required);
    }
}
