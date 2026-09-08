#![forbid(unsafe_code)]
//! Safe, shell-free planning for image-based DevCore updates.
//!
//! The planner validates release channels and immutable image references, then
//! produces direct `bootc` argv vectors. It never executes an update, reboots
//! a machine, changes a deployment, or accepts a caller-provided command.

use std::{error::Error, fmt};

/// Fixed bootc executable used by the privileged update boundary.
pub const BOOTC_PATH: &str = "/usr/bin/bootc";

/// DevCore release channels.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ReleaseChannel {
    /// Continuously published development images.
    Nightly,
    /// Early integration images.
    Alpha,
    /// Feature-complete pre-release images.
    Beta,
    /// Stable supported images.
    Stable,
}

impl ReleaseChannel {
    /// Returns the stable channel identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nightly => "nightly",
            Self::Alpha => "alpha",
            Self::Beta => "beta",
            Self::Stable => "stable",
        }
    }

    /// Parses a release channel identifier.
    pub fn parse(value: &str) -> Result<Self, UpdateError> {
        match value {
            "nightly" => Ok(Self::Nightly),
            "alpha" => Ok(Self::Alpha),
            "beta" => Ok(Self::Beta),
            "stable" => Ok(Self::Stable),
            _ => Err(UpdateError::InvalidChannel(value.to_owned())),
        }
    }
}

/// Mutating or read-only operation understood by the update boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateOperation {
    /// Inspect the active and staged deployments.
    Status,
    /// Ask bootc to check for an available update without applying it.
    Check,
    /// Switch to a validated immutable target image.
    Apply,
    /// Select the previous deployment without rebooting.
    Rollback,
}

impl UpdateOperation {
    /// Returns the stable operation identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Check => "check",
            Self::Apply => "apply",
            Self::Rollback => "rollback",
        }
    }

    /// Parses an operation identifier.
    pub fn parse(value: &str) -> Result<Self, UpdateError> {
        match value {
            "status" => Ok(Self::Status),
            "check" => Ok(Self::Check),
            "apply" => Ok(Self::Apply),
            "rollback" => Ok(Self::Rollback),
            _ => Err(UpdateError::InvalidOperation(value.to_owned())),
        }
    }
}

/// A validated channel and immutable image target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateTarget {
    /// Release channel selected by the user or policy.
    pub channel: ReleaseChannel,
    /// Digest-pinned OCI image reference.
    pub image: String,
}

impl UpdateTarget {
    /// Creates a target that cannot resolve to a mutable image tag.
    pub fn new(channel: ReleaseChannel, image: impl Into<String>) -> Result<Self, UpdateError> {
        let target = Self {
            channel,
            image: image.into(),
        };
        target.validate()?;
        Ok(target)
    }

    /// Validates the image reference before it reaches bootc.
    pub fn validate(&self) -> Result<(), UpdateError> {
        if self.image.is_empty()
            || self.image.len() > 512
            || self.image.starts_with('-')
            || self.image.chars().any(|character| {
                character.is_whitespace() || character == '\0' || character == '\n'
            })
        {
            return Err(UpdateError::InvalidImage(self.image.clone()));
        }
        let Some((_, digest)) = self.image.rsplit_once("@sha256:") else {
            return Err(UpdateError::InvalidImage(self.image.clone()));
        };
        if digest.len() != 64
            || !digest
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            return Err(UpdateError::InvalidImage(self.image.clone()));
        }
        Ok(())
    }
}

/// A validated direct-argv update plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdatePlan {
    operation: UpdateOperation,
    channel: Option<ReleaseChannel>,
    argv: Vec<String>,
}

impl UpdatePlan {
    /// Creates a plan for a read-only operation or a validated target switch.
    pub fn for_operation(
        operation: UpdateOperation,
        target: Option<&UpdateTarget>,
    ) -> Result<Self, UpdateError> {
        let (channel, argv) = match operation {
            UpdateOperation::Status => (
                None,
                vec![
                    BOOTC_PATH.to_owned(),
                    "status".to_owned(),
                    "--json".to_owned(),
                ],
            ),
            UpdateOperation::Check => (
                None,
                vec![
                    BOOTC_PATH.to_owned(),
                    "upgrade".to_owned(),
                    "--check".to_owned(),
                ],
            ),
            UpdateOperation::Rollback => (None, vec![BOOTC_PATH.to_owned(), "rollback".to_owned()]),
            UpdateOperation::Apply => {
                let target = target.ok_or(UpdateError::MissingTarget(UpdateOperation::Apply))?;
                target.validate()?;
                (
                    Some(target.channel),
                    vec![
                        BOOTC_PATH.to_owned(),
                        "switch".to_owned(),
                        "--transport".to_owned(),
                        "ostree-container".to_owned(),
                        target.image.clone(),
                    ],
                )
            }
        };
        Ok(Self {
            operation,
            channel,
            argv,
        })
    }

    /// Returns the planned operation.
    #[must_use]
    pub const fn operation(&self) -> UpdateOperation {
        self.operation
    }

    /// Returns the target channel for an apply plan, if present.
    #[must_use]
    pub const fn channel(&self) -> Option<ReleaseChannel> {
        self.channel
    }

    /// Returns the direct argv vector for the authorized update boundary.
    #[must_use]
    pub fn argv(&self) -> &[String] {
        &self.argv
    }
}

/// Failure while validating a release or constructing a bootc plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateError {
    /// The channel identifier is not one of the supported channels.
    InvalidChannel(String),
    /// The image is not a safe digest-pinned reference.
    InvalidImage(String),
    /// The operation identifier is unknown.
    InvalidOperation(String),
    /// An apply plan was requested without a validated target.
    MissingTarget(UpdateOperation),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidChannel(channel) => {
                write!(formatter, "invalid release channel {channel:?}")
            }
            Self::InvalidImage(image) => write!(formatter, "invalid update image {image:?}"),
            Self::InvalidOperation(operation) => {
                write!(formatter, "invalid update operation {operation:?}")
            }
            Self::MissingTarget(operation) => {
                write!(
                    formatter,
                    "update operation {} requires an image target",
                    operation.as_str()
                )
            }
        }
    }
}

impl Error for UpdateError {}

#[cfg(test)]
mod tests {
    use super::{
        BOOTC_PATH, ReleaseChannel, UpdateError, UpdateOperation, UpdatePlan, UpdateTarget,
    };

    const IMAGE: &str = "quay.io/devcore/os@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn parses_all_release_channels() {
        assert_eq!(
            ReleaseChannel::parse("nightly").unwrap().as_str(),
            "nightly"
        );
        assert_eq!(ReleaseChannel::parse("alpha").unwrap().as_str(), "alpha");
        assert_eq!(ReleaseChannel::parse("beta").unwrap().as_str(), "beta");
        assert_eq!(ReleaseChannel::parse("stable").unwrap().as_str(), "stable");
        assert!(matches!(
            ReleaseChannel::parse("latest"),
            Err(UpdateError::InvalidChannel(_))
        ));
    }

    #[test]
    fn apply_plan_is_digest_pinned_and_shell_free() {
        let target = UpdateTarget::new(ReleaseChannel::Alpha, IMAGE).unwrap();
        let plan = UpdatePlan::for_operation(UpdateOperation::Apply, Some(&target)).unwrap();

        assert_eq!(plan.argv()[0], BOOTC_PATH);
        assert_eq!(plan.channel(), Some(ReleaseChannel::Alpha));
        assert!(plan.argv().contains(&"ostree-container".to_owned()));
        assert!(!plan.argv().iter().any(|argument| argument.contains(' ')));
    }

    #[test]
    fn read_only_plans_do_not_need_a_target() {
        let status = UpdatePlan::for_operation(UpdateOperation::Status, None).unwrap();
        let check = UpdatePlan::for_operation(UpdateOperation::Check, None).unwrap();
        let rollback = UpdatePlan::for_operation(UpdateOperation::Rollback, None).unwrap();

        assert_eq!(status.argv(), [BOOTC_PATH, "status", "--json"]);
        assert_eq!(check.argv(), [BOOTC_PATH, "upgrade", "--check"]);
        assert_eq!(rollback.argv(), [BOOTC_PATH, "rollback"]);
    }

    #[test]
    fn rejects_missing_or_mutable_targets() {
        assert!(matches!(
            UpdatePlan::for_operation(UpdateOperation::Apply, None),
            Err(UpdateError::MissingTarget(UpdateOperation::Apply))
        ));
        assert!(matches!(
            UpdateTarget::new(ReleaseChannel::Stable, "quay.io/devcore/os:stable"),
            Err(UpdateError::InvalidImage(_))
        ));
        assert!(matches!(
            UpdateTarget::new(ReleaseChannel::Stable, "quay.io/devcore/os@sha256:bad"),
            Err(UpdateError::InvalidImage(_))
        ));
    }
}
