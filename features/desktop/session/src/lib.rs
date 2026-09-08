#![forbid(unsafe_code)]
//! Minimal greetd IPC client and DevCore session launch contract.
//!
//! Authentication remains entirely inside greetd's PAM stack. This crate only
//! sends the username, displays opaque PAM conversation messages, forwards the
//! user's response without persisting it, and requests the approved session
//! command after greetd reports success.

use std::{
    env,
    error::Error,
    fmt,
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// The message kind requested by the PAM conversation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthMessageKind {
    /// The response may be displayed to the user.
    Visible,
    /// The response must be kept hidden by the greeter UI.
    Secret,
    /// Informational text that does not require a response.
    Info,
    /// An authentication error that should be shown to the user.
    Error,
}

/// A response returned by greetd after a request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GreetdResponse {
    /// The request succeeded.
    Success,
    /// PAM needs another conversation response.
    AuthMessage {
        /// The required presentation/response mode.
        kind: AuthMessageKind,
        /// Message supplied by the PAM stack.
        message: String,
    },
    /// greetd rejected the request.
    Error {
        /// Protocol error identifier.
        error_type: String,
        /// Human-readable description from greetd.
        description: String,
    },
}

/// A validated command and environment for the user session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionLaunchSpec {
    /// Compositor executable, normally `/usr/bin/devcore-compositor`.
    pub compositor: PathBuf,
    /// Shell executable started by the compositor.
    pub shell: PathBuf,
    /// Non-secret environment entries passed to greetd.
    pub environment: Vec<String>,
}

impl SessionLaunchSpec {
    /// Creates the default DevCore Wayland session launch contract.
    #[must_use]
    pub fn devcore() -> Self {
        Self {
            compositor: PathBuf::from("/usr/bin/devcore-compositor"),
            shell: PathBuf::from("/usr/bin/devcore-shell"),
            environment: vec![
                "XDG_CURRENT_DESKTOP=DevCore".to_owned(),
                "XDG_SESSION_DESKTOP=devcore".to_owned(),
                "XDG_SESSION_TYPE=wayland".to_owned(),
                "GDK_BACKEND=wayland".to_owned(),
                "QT_QPA_PLATFORM=wayland".to_owned(),
                "SDL_VIDEODRIVER=wayland".to_owned(),
            ],
        }
    }

    /// Validates executable paths and environment entries before launch.
    pub fn validate(&self) -> Result<(), SessionError> {
        if !is_absolute_executable(&self.compositor) {
            return Err(SessionError::InvalidExecutable(self.compositor.clone()));
        }
        if !is_absolute_executable(&self.shell) {
            return Err(SessionError::InvalidExecutable(self.shell.clone()));
        }
        if self.environment.iter().any(|entry| {
            entry.is_empty()
                || entry.starts_with('=')
                || entry.contains('\0')
                || !entry.contains('=')
        }) {
            return Err(SessionError::InvalidEnvironment);
        }
        Ok(())
    }

    /// Returns the direct argv used to start the compositor and shell.
    pub fn command(&self) -> Result<Vec<String>, SessionError> {
        self.validate()?;
        Ok(vec![
            self.compositor.to_string_lossy().into_owned(),
            "--backend".to_owned(),
            "drm".to_owned(),
            "--shell".to_owned(),
            self.shell.to_string_lossy().into_owned(),
        ])
    }
}

fn is_absolute_executable(path: &Path) -> bool {
    path.is_absolute()
        && path != Path::new("/")
        && path.to_str().is_some_and(|value| !value.contains('\0'))
}

/// A connected greetd conversation.
#[derive(Debug)]
pub struct GreetdSession {
    stream: UnixStream,
}

impl GreetdSession {
    /// Connects to the socket named by `GREETD_SOCK`.
    pub fn connect_from_environment() -> Result<Self, SessionError> {
        let socket = env::var_os("GREETD_SOCK").ok_or(SessionError::SocketNotConfigured)?;
        Self::connect(Path::new(&socket))
    }

    /// Connects to an explicit greetd UNIX socket.
    pub fn connect(socket: &Path) -> Result<Self, SessionError> {
        if socket.as_os_str().is_empty() {
            return Err(SessionError::InvalidSocket(socket.to_path_buf()));
        }
        Ok(Self {
            stream: UnixStream::connect(socket).map_err(SessionError::Io)?,
        })
    }

    /// Starts a PAM login conversation for a username.
    pub fn create_session(
        &mut self,
        username: impl Into<String>,
    ) -> Result<GreetdResponse, SessionError> {
        self.request(&Request::CreateSession {
            username: username.into(),
        })
    }

    /// Forwards one opaque answer to the latest PAM conversation message.
    ///
    /// The response is written to the socket and is not retained by this
    /// object. Callers must clear their UI input after this call returns.
    pub fn post_auth_message_response(
        &mut self,
        response: Option<String>,
    ) -> Result<GreetdResponse, SessionError> {
        self.request(&Request::PostAuthMessageResponse { response })
    }

    /// Requests the authenticated session to start.
    pub fn start_session(
        &mut self,
        launch: &SessionLaunchSpec,
    ) -> Result<GreetdResponse, SessionError> {
        let command = launch.command()?;
        self.request(&Request::StartSession {
            cmd: command,
            env: launch.environment.clone(),
        })
    }

    /// Cancels the pending login conversation.
    pub fn cancel_session(&mut self) -> Result<GreetdResponse, SessionError> {
        self.request(&Request::CancelSession)
    }

    fn request(&mut self, request: &Request) -> Result<GreetdResponse, SessionError> {
        write_frame(&mut self.stream, request).map_err(SessionError::Io)?;
        let response =
            read_frame::<_, Response>(&mut self.stream).map_err(SessionError::Protocol)?;
        response.try_into()
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    CreateSession { username: String },
    PostAuthMessageResponse { response: Option<String> },
    StartSession { cmd: Vec<String>, env: Vec<String> },
    CancelSession,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Response {
    Success,
    Error {
        error_type: String,
        description: String,
    },
    AuthMessage {
        #[serde(rename = "message_type")]
        message_type: WireAuthMessageKind,
        message: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum WireAuthMessageKind {
    Visible,
    Secret,
    Info,
    Error,
}

impl TryFrom<Response> for GreetdResponse {
    type Error = SessionError;

    fn try_from(response: Response) -> Result<Self, SessionError> {
        Ok(match response {
            Response::Success => Self::Success,
            Response::Error {
                error_type,
                description,
            } => Self::Error {
                error_type,
                description,
            },
            Response::AuthMessage {
                message_type,
                message,
            } => Self::AuthMessage {
                kind: match message_type {
                    WireAuthMessageKind::Visible => AuthMessageKind::Visible,
                    WireAuthMessageKind::Secret => AuthMessageKind::Secret,
                    WireAuthMessageKind::Info => AuthMessageKind::Info,
                    WireAuthMessageKind::Error => AuthMessageKind::Error,
                },
                message,
            },
        })
    }
}

fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> io::Result<()> {
    let payload = serde_json::to_vec(value).map_err(io::Error::other)?;
    let length = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "greetd frame is too large"))?;
    writer.write_all(&length.to_ne_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()
}

fn read_frame<R: Read, T: for<'de> Deserialize<'de>>(
    reader: &mut R,
) -> Result<T, serde_json::Error> {
    let mut length_bytes = [0_u8; 4];
    reader
        .read_exact(&mut length_bytes)
        .map_err(serde_json::Error::io)?;
    let length = u32::from_ne_bytes(length_bytes) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(serde_json::Error::io(io::Error::new(
            io::ErrorKind::InvalidData,
            "greetd frame exceeds safety limit",
        )));
    }
    let mut payload = vec![0_u8; length];
    reader
        .read_exact(&mut payload)
        .map_err(serde_json::Error::io)?;
    serde_json::from_slice(&payload)
}

/// A session protocol or launch failure.
#[derive(Debug)]
pub enum SessionError {
    /// The `GREETD_SOCK` environment variable was missing.
    SocketNotConfigured,
    /// The configured socket path was empty.
    InvalidSocket(PathBuf),
    /// A session executable was relative, root, or not valid UTF-8.
    InvalidExecutable(PathBuf),
    /// An environment entry was malformed or potentially ambiguous.
    InvalidEnvironment,
    /// The UNIX socket operation failed.
    Io(io::Error),
    /// The greetd frame was malformed or exceeded the safety limit.
    Protocol(serde_json::Error),
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SocketNotConfigured => formatter.write_str("GREETD_SOCK is not configured"),
            Self::InvalidSocket(path) => {
                write!(formatter, "invalid greetd socket {}", path.display())
            }
            Self::InvalidExecutable(path) => {
                write!(formatter, "invalid session executable {}", path.display())
            }
            Self::InvalidEnvironment => formatter.write_str("invalid session environment entry"),
            Self::Io(error) => write!(formatter, "greetd I/O error: {error}"),
            Self::Protocol(error) => write!(formatter, "greetd protocol error: {error}"),
        }
    }
}

impl Error for SessionError {}

#[cfg(test)]
mod tests {
    use std::{io::Cursor, os::unix::net::UnixStream, thread};

    use super::{
        AuthMessageKind, GreetdResponse, SessionLaunchSpec, WireAuthMessageKind, read_frame,
        write_frame,
    };

    #[test]
    fn default_session_contract_is_wayland_and_direct() {
        let launch = SessionLaunchSpec::devcore();

        assert_eq!(
            launch.command().unwrap(),
            vec![
                "/usr/bin/devcore-compositor",
                "--backend",
                "drm",
                "--shell",
                "/usr/bin/devcore-shell"
            ]
        );
        assert!(
            launch
                .environment
                .iter()
                .any(|entry| entry == "XDG_SESSION_TYPE=wayland")
        );
    }

    #[test]
    fn launch_rejects_relative_executables_and_malformed_environment() {
        let mut launch = SessionLaunchSpec::devcore();
        launch.shell = "devcore-shell".into();
        assert!(launch.command().is_err());

        let mut launch = SessionLaunchSpec::devcore();
        launch.environment.push("XDG_TEST=value".to_owned());
        assert!(launch.validate().is_ok());
        launch.environment.push("malformed".to_owned());
        assert!(launch.validate().is_err());
    }

    #[test]
    fn native_endian_frames_round_trip_without_auth_assumptions() {
        let value = super::Response::AuthMessage {
            message_type: WireAuthMessageKind::Secret,
            message: "Password:".to_owned(),
        };
        let mut encoded = Cursor::new(Vec::new());
        write_frame(&mut encoded, &value).unwrap();
        encoded.set_position(0);

        let decoded: super::Response = read_frame(&mut encoded).unwrap();
        let response = GreetdResponse::try_from(decoded).unwrap();
        assert_eq!(
            response,
            GreetdResponse::AuthMessage {
                kind: AuthMessageKind::Secret,
                message: "Password:".to_owned(),
            }
        );
    }

    #[test]
    fn session_methods_follow_the_greetd_conversation() {
        let (mut server, client) = UnixStream::pair().unwrap();
        let server_thread = thread::spawn(move || {
            let create: super::Request = read_frame(&mut server).unwrap();
            assert!(matches!(
                create,
                super::Request::CreateSession { username } if username == "developer"
            ));
            write_frame(
                &mut server,
                &super::Response::AuthMessage {
                    message_type: WireAuthMessageKind::Secret,
                    message: "Authentication response:".to_owned(),
                },
            )
            .unwrap();

            let response: super::Request = read_frame(&mut server).unwrap();
            assert!(matches!(
                response,
                super::Request::PostAuthMessageResponse { response: Some(value) }
                    if value == "transient-response"
            ));
            write_frame(&mut server, &super::Response::Success).unwrap();
        });

        let mut session = super::GreetdSession { stream: client };
        assert_eq!(
            session.create_session("developer").unwrap(),
            GreetdResponse::AuthMessage {
                kind: AuthMessageKind::Secret,
                message: "Authentication response:".to_owned(),
            }
        );
        assert_eq!(
            session
                .post_auth_message_response(Some("transient-response".to_owned()))
                .unwrap(),
            GreetdResponse::Success
        );
        server_thread.join().unwrap();
    }
}
