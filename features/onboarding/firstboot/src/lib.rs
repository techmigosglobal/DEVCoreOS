#![forbid(unsafe_code)]
//! Validated first-boot configuration and system-setup boundary.
//!
//! This crate owns only non-secret setup state and a narrow backend contract.
//! Password material is accepted as a transient byte slice, is never part of
//! [`SetupConfig`], and is never written to the state file or command argv.
//! The production backend delegates account/password handling to the existing
//! system `useradd`/`chpasswd` and systemd locale/time/hostname tools.

use std::{
    error::Error,
    fmt,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use devcore_core::MachineProfile;

/// Default directory for the non-secret first-boot completion state.
pub const DEFAULT_STATE_DIR: &str = "/var/lib/devcore";
const STATE_FILE: &str = "first-boot.conf";
const MAX_PASSWORD_BYTES: usize = 1024;

/// Non-secret values collected by the first-boot flow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupConfig {
    /// Locale identifier passed to `localectl`.
    pub language: String,
    /// Keyboard layout identifier passed to `localectl`.
    pub keyboard: String,
    /// Timezone identifier passed to `timedatectl`.
    pub timezone: String,
    /// New local account name.
    pub username: String,
    /// Hostname assigned through `hostnamectl`.
    pub hostname: String,
    /// Conservative development-resource profile selected by the user.
    pub profile: MachineProfile,
}

impl SetupConfig {
    /// Creates and validates a first-boot configuration.
    pub fn new(
        language: impl Into<String>,
        keyboard: impl Into<String>,
        timezone: impl Into<String>,
        username: impl Into<String>,
        hostname: impl Into<String>,
        profile: &str,
    ) -> Result<Self, FirstBootError> {
        let config = Self {
            language: language.into(),
            keyboard: keyboard.into(),
            timezone: timezone.into(),
            username: username.into(),
            hostname: hostname.into(),
            profile: parse_profile(profile)?,
        };
        config.validate()?;
        Ok(config)
    }

    /// Validates all values before any privileged operation is attempted.
    pub fn validate(&self) -> Result<(), FirstBootError> {
        validate_identifier("language", &self.language, 64, |character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })?;
        validate_identifier("keyboard", &self.keyboard, 64, |character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
        })?;
        if self.timezone.is_empty()
            || self.timezone.len() > 128
            || self.timezone.starts_with('/')
            || self.timezone.contains("..")
            || !self.timezone.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '/' | '_' | '+' | '-')
            })
        {
            return Err(FirstBootError::InvalidValue {
                field: "timezone",
                reason: "must be a bounded timezone identifier without traversal".to_owned(),
            });
        }
        if self.username.is_empty()
            || self.username.len() > 32
            || !self
                .username
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_lowercase() || character == '_')
            || !self.username.chars().all(|character| {
                character.is_ascii_lowercase()
                    || character.is_ascii_digit()
                    || matches!(character, '_' | '-')
            })
            || matches!(
                self.username.as_str(),
                "root" | "daemon" | "bin" | "devcore"
            )
        {
            return Err(FirstBootError::InvalidValue {
                field: "username",
                reason: "must be a non-reserved Linux username".to_owned(),
            });
        }
        if self.hostname.is_empty()
            || self.hostname.len() > 63
            || self.hostname.starts_with('-')
            || self.hostname.ends_with('-')
            || !self
                .hostname
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
        {
            return Err(FirstBootError::InvalidValue {
                field: "hostname",
                reason: "must be a bounded hostname without dots or separators".to_owned(),
            });
        }
        Ok(())
    }

    fn encode(&self) -> String {
        format!(
            "version=1\nlanguage={}\nkeyboard={}\ntimezone={}\nusername={}\nhostname={}\nprofile={}\n",
            self.language,
            self.keyboard,
            self.timezone,
            self.username,
            self.hostname,
            self.profile.as_str(),
        )
    }
}

fn validate_identifier(
    field: &'static str,
    value: &str,
    max_bytes: usize,
    allowed: impl Fn(char) -> bool,
) -> Result<(), FirstBootError> {
    if value.is_empty() || value.len() > max_bytes || !value.chars().all(allowed) {
        return Err(FirstBootError::InvalidValue {
            field,
            reason: "must contain only bounded safe identifier characters".to_owned(),
        });
    }
    Ok(())
}

/// Parses the stable profile identifier used by configuration and D-Bus.
pub fn parse_profile(value: &str) -> Result<MachineProfile, FirstBootError> {
    match value {
        "low" => Ok(MachineProfile::Low),
        "balanced" => Ok(MachineProfile::Balanced),
        "standard" => Ok(MachineProfile::Standard),
        "workstation" => Ok(MachineProfile::Workstation),
        _ => Err(FirstBootError::InvalidValue {
            field: "profile",
            reason: format!("unknown profile {value:?}"),
        }),
    }
}

fn validate_password(password: &[u8]) -> Result<(), FirstBootError> {
    if password.is_empty()
        || password.len() < 8
        || password.len() > MAX_PASSWORD_BYTES
        || password.contains(&0)
        || password.contains(&b'\n')
        || password.contains(&b'\r')
    {
        return Err(FirstBootError::InvalidPassword);
    }
    Ok(())
}

/// State and atomic persistence for first-boot completion.
#[derive(Clone, Debug)]
pub struct FirstBootStore {
    root: PathBuf,
}

impl FirstBootStore {
    /// Opens a state directory, creating only that directory if absent.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, FirstBootError> {
        let root = root.into();
        if root.as_os_str().is_empty() || root == Path::new("/") {
            return Err(FirstBootError::InvalidStatePath(root));
        }
        fs::create_dir_all(&root).map_err(|source| FirstBootError::StateIo {
            operation: "create first-boot state directory",
            path: root.clone(),
            source,
        })?;
        #[cfg(unix)]
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|source| {
            FirstBootError::StateIo {
                operation: "restrict first-boot state directory",
                path: root.clone(),
                source,
            }
        })?;
        Ok(Self { root })
    }

    /// Returns whether a regular completion file exists.
    pub fn is_complete(&self) -> Result<bool, FirstBootError> {
        let path = self.path();
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => Ok(true),
            Ok(_) => Err(FirstBootError::InvalidStatePath(path)),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(FirstBootError::StateIo {
                operation: "inspect first-boot completion",
                path,
                source,
            }),
        }
    }

    /// Atomically publishes validated non-secret setup values.
    pub fn commit(&self, config: &SetupConfig) -> Result<(), FirstBootError> {
        config.validate()?;
        if self.is_complete()? {
            return Err(FirstBootError::AlreadyComplete);
        }
        let path = self.path();
        let temporary = self
            .root
            .join(format!(".{STATE_FILE}.{}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| FirstBootError::StateIo {
                operation: "create first-boot temporary state",
                path: temporary.clone(),
                source,
            })?;
        #[cfg(unix)]
        if let Err(source) = file.set_permissions(fs::Permissions::from_mode(0o600)) {
            drop(file);
            let _ = fs::remove_file(&temporary);
            return Err(FirstBootError::StateIo {
                operation: "restrict first-boot state permissions",
                path: temporary,
                source,
            });
        }
        let write_result = file
            .write_all(config.encode().as_bytes())
            .and_then(|()| file.sync_all());
        if let Err(source) = write_result {
            drop(file);
            let _ = fs::remove_file(&temporary);
            return Err(FirstBootError::StateIo {
                operation: "write first-boot state",
                path: temporary,
                source,
            });
        }
        drop(file);
        if let Err(source) = fs::rename(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(FirstBootError::StateIo {
                operation: "publish first-boot state",
                path,
                source,
            });
        }
        Ok(())
    }

    fn path(&self) -> PathBuf {
        self.root.join(STATE_FILE)
    }
}

/// Failure produced by the fixed system setup commands.
#[derive(Debug)]
pub enum SetupBackendError {
    /// The command could not be started or fed its transient input.
    Io {
        /// Absolute command path used by the backend.
        program: String,
        /// Underlying process or pipe error.
        source: io::Error,
    },
    /// The command exited unsuccessfully. Its output is deliberately omitted.
    Failed {
        /// Absolute command path used by the backend.
        program: String,
        /// Exit status, when the process returned normally.
        status: Option<i32>,
    },
}

impl fmt::Display for SetupBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { program, source } => {
                write!(formatter, "{program} failed to start: {source}")
            }
            Self::Failed { program, status } => {
                write!(formatter, "{program} exited unsuccessfully ({status:?})")
            }
        }
    }
}

impl Error for SetupBackendError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Failed { .. } => None,
        }
    }
}

/// Narrow backend contract for applying validated setup.
pub trait SetupBackend: std::fmt::Debug {
    /// Applies locale, keyboard, timezone, hostname, and account settings.
    fn apply(&mut self, config: &SetupConfig, password: &[u8]) -> Result<(), SetupBackendError>;
}

/// Production backend using existing Linux/systemd account and configuration tools.
#[derive(Debug, Default)]
pub struct SystemBackend;

impl SystemBackend {
    fn run(program: &str, args: &[String]) -> Result<(), SetupBackendError> {
        let status = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|source| SetupBackendError::Io {
                program: program.to_owned(),
                source,
            })?;
        if status.success() {
            Ok(())
        } else {
            Err(SetupBackendError::Failed {
                program: program.to_owned(),
                status: status.code(),
            })
        }
    }

    fn run_password(program: &str, args: &[String], input: &[u8]) -> Result<(), SetupBackendError> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| SetupBackendError::Io {
                program: program.to_owned(),
                source,
            })?;
        let write_result = child
            .stdin
            .take()
            .ok_or_else(|| SetupBackendError::Io {
                program: program.to_owned(),
                source: io::Error::other("password stdin was not available"),
            })
            .and_then(|mut stdin| {
                stdin
                    .write_all(input)
                    .map_err(|source| SetupBackendError::Io {
                        program: program.to_owned(),
                        source,
                    })
            });
        if let Err(error) = write_result {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let status = child.wait().map_err(|source| SetupBackendError::Io {
            program: program.to_owned(),
            source,
        })?;
        if status.success() {
            Ok(())
        } else {
            Err(SetupBackendError::Failed {
                program: program.to_owned(),
                status: status.code(),
            })
        }
    }
}

impl SetupBackend for SystemBackend {
    fn apply(&mut self, config: &SetupConfig, password: &[u8]) -> Result<(), SetupBackendError> {
        Self::run(
            "/usr/bin/localectl",
            &["set-locale".to_owned(), format!("LANG={}", config.language)],
        )?;
        Self::run(
            "/usr/bin/localectl",
            &["set-keymap".to_owned(), config.keyboard.clone()],
        )?;
        Self::run(
            "/usr/bin/timedatectl",
            &["set-timezone".to_owned(), config.timezone.clone()],
        )?;
        Self::run(
            "/usr/bin/hostnamectl",
            &["set-hostname".to_owned(), config.hostname.clone()],
        )?;
        Self::run(
            "/usr/sbin/useradd",
            &[
                "--create-home".to_owned(),
                "--user-group".to_owned(),
                "--shell".to_owned(),
                "/bin/bash".to_owned(),
                config.username.clone(),
            ],
        )?;

        let mut input = Vec::with_capacity(config.username.len() + password.len() + 2);
        input.extend_from_slice(config.username.as_bytes());
        input.push(b':');
        input.extend_from_slice(password);
        input.push(b'\n');
        let result = Self::run_password("/usr/sbin/chpasswd", &[], &input);
        input.fill(0);
        result
    }
}

/// First-boot state machine around a selected setup backend.
#[derive(Debug)]
pub struct FirstBootCoordinator<B> {
    store: FirstBootStore,
    backend: B,
    suggested_profile: MachineProfile,
}

impl<B: SetupBackend> FirstBootCoordinator<B> {
    /// Creates a coordinator with a conservative hardware-derived suggestion.
    pub fn new(store: FirstBootStore, backend: B, suggested_profile: MachineProfile) -> Self {
        Self {
            store,
            backend,
            suggested_profile,
        }
    }

    /// Returns whether setup is still required and the suggested profile.
    pub fn status(&self) -> Result<(bool, MachineProfile), FirstBootError> {
        Ok((!self.store.is_complete()?, self.suggested_profile))
    }

    /// Applies setup and commits non-secret state only after all commands succeed.
    pub fn apply(&mut self, config: SetupConfig, password: &[u8]) -> Result<(), FirstBootError> {
        config.validate()?;
        validate_password(password)?;
        if self.store.is_complete()? {
            return Err(FirstBootError::AlreadyComplete);
        }
        self.backend
            .apply(&config, password)
            .map_err(FirstBootError::Backend)?;
        self.store.commit(&config)
    }
}

/// First-boot validation, state, or backend failure.
#[derive(Debug)]
pub enum FirstBootError {
    /// A setup value failed its strict field validation.
    InvalidValue {
        /// Stable field identifier.
        field: &'static str,
        /// Safe explanation suitable for a UI.
        reason: String,
    },
    /// Password input was empty, too short/large, or contained line delimiters.
    InvalidPassword,
    /// A state path was not an acceptable private file location.
    InvalidStatePath(PathBuf),
    /// A state file operation failed.
    StateIo {
        /// Operation being performed.
        operation: &'static str,
        /// Path involved in the operation.
        path: PathBuf,
        /// Underlying I/O failure.
        source: io::Error,
    },
    /// Setup was already completed.
    AlreadyComplete,
    /// The fixed system backend failed.
    Backend(SetupBackendError),
}

impl fmt::Display for FirstBootError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidValue { field, reason } => write!(formatter, "invalid {field}: {reason}"),
            Self::InvalidPassword => {
                formatter.write_str("password must be 8-1024 bytes without line delimiters")
            }
            Self::InvalidStatePath(path) => write!(
                formatter,
                "invalid first-boot state path {}",
                path.display()
            ),
            Self::StateIo {
                operation,
                path,
                source,
            } => {
                write!(formatter, "{operation} {} failed: {source}", path.display())
            }
            Self::AlreadyComplete => formatter.write_str("first-boot setup is already complete"),
            Self::Backend(error) => error.fmt(formatter),
        }
    }
}

impl Error for FirstBootError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::StateIo { source, .. } => Some(source),
            Self::Backend(error) => Some(error),
            Self::InvalidValue { .. }
            | Self::InvalidPassword
            | Self::InvalidStatePath(_)
            | Self::AlreadyComplete => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::{Arc, Mutex},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        FirstBootCoordinator, FirstBootError, FirstBootStore, MachineProfile, SetupBackend,
        SetupBackendError, SetupConfig,
    };

    #[derive(Clone, Debug, Default)]
    struct RecordingBackend {
        calls: Arc<Mutex<Vec<String>>>,
        password: Arc<Mutex<Vec<u8>>>,
    }

    impl SetupBackend for RecordingBackend {
        fn apply(
            &mut self,
            config: &SetupConfig,
            password: &[u8],
        ) -> Result<(), SetupBackendError> {
            self.calls.lock().unwrap().push(config.username.clone());
            self.password.lock().unwrap().extend_from_slice(password);
            Ok(())
        }
    }

    fn state_root(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("devcore-firstboot-{label}-{nanos}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn config() -> SetupConfig {
        SetupConfig::new(
            "en_US.UTF-8",
            "us",
            "Asia/Kolkata",
            "developer",
            "devcore-lab",
            "balanced",
        )
        .unwrap()
    }

    #[test]
    fn validates_setup_values_and_rejects_reserved_or_traversal_inputs() {
        assert_eq!(config().profile, MachineProfile::Balanced);
        assert!(SetupConfig::new("en", "us", "../etc", "developer", "host", "low").is_err());
        assert!(SetupConfig::new("en", "us", "UTC", "root", "host", "low").is_err());
        assert!(SetupConfig::new("en", "us", "UTC", "Developer", "host", "low").is_err());
        assert!(SetupConfig::new("en", "us", "UTC", "developer", "host-name", "unknown").is_err());
    }

    #[test]
    fn status_is_incomplete_until_non_secret_config_is_atomically_committed() {
        let root = state_root("state");
        let store = FirstBootStore::open(&root).unwrap();
        assert!(!store.is_complete().unwrap());
        store.commit(&config()).unwrap();
        assert!(store.is_complete().unwrap());
        let bytes = fs::read(root.join("first-boot.conf")).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("username=developer"));
        assert!(!text.contains("password"));
        assert!(store.commit(&config()).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn coordinator_forwards_password_transiently_and_commits_only_after_backend() {
        let root = state_root("apply");
        let backend = RecordingBackend::default();
        let seen_password = Arc::clone(&backend.password);
        let mut coordinator = FirstBootCoordinator::new(
            FirstBootStore::open(&root).unwrap(),
            backend,
            MachineProfile::Balanced,
        );
        let (required, profile) = coordinator.status().unwrap();
        assert!(required);
        assert_eq!(profile, MachineProfile::Balanced);
        coordinator.apply(config(), b"transient-password").unwrap();
        assert_eq!(&*seen_password.lock().unwrap(), b"transient-password");
        assert!(!coordinator.status().unwrap().0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_short_or_delimited_passwords_before_backend_call() {
        let root = state_root("password");
        let backend = RecordingBackend::default();
        let calls = Arc::clone(&backend.calls);
        let mut coordinator = FirstBootCoordinator::new(
            FirstBootStore::open(&root).unwrap(),
            backend,
            MachineProfile::Low,
        );
        for password in [b"short".as_slice(), b"line\npassword".as_slice()] {
            assert!(matches!(
                coordinator.apply(config(), password),
                Err(FirstBootError::InvalidPassword)
            ));
        }
        assert!(calls.lock().unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
