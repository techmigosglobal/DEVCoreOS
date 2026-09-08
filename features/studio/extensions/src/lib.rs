#![forbid(unsafe_code)]
//! Safe command planning for an optional VS Code-compatible extension host.
//!
//! DevCore's native editor remains independent from a Node/Electron runtime.
//! When a user has an approved compatible host installed, this crate produces
//! direct argv for opening a workspace and managing extensions through that
//! host. It never downloads an extension, starts a shell, or chooses a
//! caller-provided executable.

use std::{
    error::Error,
    fmt,
    path::{Component, Path, PathBuf},
};

/// Fixed optional hosts checked in priority order.
pub const HOST_PATHS: &[&str] = &[
    "/usr/bin/code",
    "/usr/bin/code-oss",
    "/usr/bin/codium",
    "/usr/local/bin/code",
];

const MAX_EXTENSION_SOURCE_LENGTH: usize = 512;
const MAX_WORKSPACE_PATH_LENGTH: usize = 4096;

/// A validated fixed-path VS Code-compatible host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtensionHost {
    program: PathBuf,
}

impl ExtensionHost {
    /// Creates a host descriptor for one absolute executable path.
    pub fn new(program: impl Into<PathBuf>) -> Result<Self, ExtensionError> {
        let program = program.into();
        validate_absolute_path(&program, "extension host")?;
        Ok(Self { program })
    }

    /// Finds the first installed host from the fixed allowlist.
    #[must_use]
    pub fn discover() -> Option<Self> {
        HOST_PATHS
            .iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
            .map(|program| Self { program })
    }

    /// Returns the fixed executable path.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Plans opening one existing absolute workspace in the host.
    pub fn open_workspace_command(&self, workspace: &Path) -> Result<Vec<String>, ExtensionError> {
        validate_workspace_path(workspace)?;
        Ok(vec![
            self.program.to_string_lossy().into_owned(),
            "--reuse-window".to_owned(),
            workspace.to_string_lossy().into_owned(),
        ])
    }

    /// Plans a read-only extension listing.
    #[must_use]
    pub fn list_command(&self) -> Vec<String> {
        vec![
            self.program.to_string_lossy().into_owned(),
            "--list-extensions".to_owned(),
        ]
    }

    /// Plans an explicit extension installation.
    ///
    /// The source may be a marketplace identifier or an absolute .vsix path.
    /// Network access, if the host chooses to use it for an identifier, is an
    /// explicit user action rather than a boot-time service.
    pub fn install_command(&self, source: &str) -> Result<Vec<String>, ExtensionError> {
        validate_extension_source(source)?;
        Ok(vec![
            self.program.to_string_lossy().into_owned(),
            "--install-extension".to_owned(),
            source.to_owned(),
            "--force".to_owned(),
        ])
    }

    /// Plans uninstalling one marketplace extension identifier.
    pub fn uninstall_command(&self, identifier: &str) -> Result<Vec<String>, ExtensionError> {
        validate_extension_identifier(identifier)?;
        Ok(vec![
            self.program.to_string_lossy().into_owned(),
            "--uninstall-extension".to_owned(),
            identifier.to_owned(),
        ])
    }
}

fn validate_extension_source(source: &str) -> Result<(), ExtensionError> {
    if source.is_empty()
        || source.len() > MAX_EXTENSION_SOURCE_LENGTH
        || source
            .chars()
            .any(|character| character == '\0' || character.is_whitespace())
    {
        return Err(ExtensionError::InvalidSource(source.to_owned()));
    }
    if source.ends_with(".vsix") || source.ends_with(".VSIX") {
        let path = Path::new(source);
        if !path.is_absolute()
            || path
                .components()
                .any(|component| component == Component::ParentDir)
        {
            return Err(ExtensionError::InvalidSource(source.to_owned()));
        }
        return Ok(());
    }
    validate_extension_identifier(source)
}

fn validate_extension_identifier(identifier: &str) -> Result<(), ExtensionError> {
    let mut parts = identifier.split('.');
    let publisher = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if publisher.is_empty()
        || name.is_empty()
        || parts.next().is_some()
        || identifier.len() > MAX_EXTENSION_SOURCE_LENGTH
        || !publisher.chars().all(valid_identifier_character)
        || !name.chars().all(valid_identifier_character)
    {
        return Err(ExtensionError::InvalidIdentifier(identifier.to_owned()));
    }
    Ok(())
}

fn valid_identifier_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
}

fn validate_workspace_path(path: &Path) -> Result<(), ExtensionError> {
    validate_absolute_path(path, "workspace")?;
    if path.as_os_str().len() > MAX_WORKSPACE_PATH_LENGTH
        || path
            .components()
            .any(|component| component == Component::ParentDir)
        || !path.is_dir()
    {
        return Err(ExtensionError::InvalidWorkspace(path.to_path_buf()));
    }
    Ok(())
}

fn validate_absolute_path(path: &Path, kind: &'static str) -> Result<(), ExtensionError> {
    if !path.is_absolute()
        || path.as_os_str().len() > MAX_WORKSPACE_PATH_LENGTH
        || path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(ExtensionError::InvalidPath {
            kind,
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

/// A failure while validating an extension host request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExtensionError {
    /// A fixed path was not absolute or was too long.
    InvalidPath {
        /// Path category.
        kind: &'static str,
        /// Rejected path.
        path: PathBuf,
    },
    /// The workspace was not a safe existing directory.
    InvalidWorkspace(PathBuf),
    /// The extension source was neither a valid identifier nor an absolute
    /// .vsix path.
    InvalidSource(String),
    /// The extension identifier was malformed.
    InvalidIdentifier(String),
}

impl fmt::Display for ExtensionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { kind, path } => write!(
                formatter,
                "{kind} path is not a safe absolute path: {}",
                path.display()
            ),
            Self::InvalidWorkspace(path) => write!(
                formatter,
                "workspace is not a safe existing directory: {}",
                path.display()
            ),
            Self::InvalidSource(source) => write!(formatter, "invalid extension source {source:?}"),
            Self::InvalidIdentifier(identifier) => {
                write!(formatter, "invalid extension identifier {identifier:?}")
            }
        }
    }
}

impl Error for ExtensionError {}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::{ExtensionError, ExtensionHost};

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

    fn workspace() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "devcore-extensions-test-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn host_plans_direct_workspace_and_extension_commands() {
        let host = ExtensionHost::new("/usr/bin/code-oss").unwrap();
        let root = workspace();
        assert_eq!(
            host.open_workspace_command(&root).unwrap(),
            vec![
                "/usr/bin/code-oss".to_owned(),
                "--reuse-window".to_owned(),
                root.to_string_lossy().into_owned()
            ]
        );
        assert_eq!(
            host.list_command(),
            vec!["/usr/bin/code-oss", "--list-extensions"]
        );
        assert_eq!(
            host.install_command("rust-lang.rust-analyzer").unwrap(),
            vec![
                "/usr/bin/code-oss",
                "--install-extension",
                "rust-lang.rust-analyzer",
                "--force"
            ]
        );
        assert_eq!(
            host.uninstall_command("rust-lang.rust-analyzer").unwrap(),
            vec![
                "/usr/bin/code-oss",
                "--uninstall-extension",
                "rust-lang.rust-analyzer"
            ]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn extension_sources_reject_shell_fragments_and_unsafe_vsix_paths() {
        let host = ExtensionHost::new("/usr/bin/code").unwrap();
        assert!(matches!(
            host.install_command("publisher.ext; rm -rf /"),
            Err(ExtensionError::InvalidSource(_)) | Err(ExtensionError::InvalidIdentifier(_))
        ));
        assert!(host.install_command("/tmp/../extension.vsix").is_err());
        assert!(host.uninstall_command("publisher").is_err());
        assert!(host.uninstall_command("publisher.ext.more").is_err());
    }

    #[test]
    fn workspace_requests_require_existing_absolute_directories() {
        let host = ExtensionHost::new("/usr/bin/code").unwrap();
        assert!(
            host.open_workspace_command(&PathBuf::from("workspace"))
                .is_err()
        );
        assert!(
            host.open_workspace_command(&PathBuf::from("/tmp/devcore-no-such-extension-workspace"))
                .is_err()
        );
    }
}
