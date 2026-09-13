#![forbid(unsafe_code)]
#![allow(missing_docs)]
//! Minimal Smithay compositor boundary.
//!
//! This binary intentionally starts with a nested Winit backend. It exercises
//! the same Wayland server, XDG-shell, SHM, seat, and renderer boundaries that
//! the eventual DRM backend will use, without taking control of this
//! workstation's display or input devices. The native DRM/libseat backend is
//! feature-gated so the default developer build remains safe to run nested.

#[cfg(feature = "native-drm")]
mod native;

use std::{
    env,
    error::Error,
    os::unix::io::OwnedFd,
    path::PathBuf,
    process::{Child, Command},
    sync::Arc,
    time::Instant,
};

use ::winit::platform::pump_events::PumpStatus;
use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, Axis, AxisSource, Event, InputBackend, InputEvent, KeyState,
            KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
        },
        renderer::{
            Color32F, Frame, Renderer,
            element::{
                Kind,
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            },
            gles::GlesRenderer,
            utils::{draw_render_elements, on_commit_buffer_handler},
        },
        winit::{self, WinitEvent},
    },
    delegate_compositor, delegate_data_device, delegate_output, delegate_seat, delegate_shm,
    delegate_xdg_shell,
    desktop::{WindowSurfaceType, utils::under_from_surface_tree},
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::{FilterResult, keysyms},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::{Display, protocol::wl_seat},
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Serial, Size, Transform},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
            TraversalAction, with_surface_tree_downward,
        },
        output::OutputHandler,
        selection::{
            SelectionHandler,
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
            },
        },
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        },
        shm::{ShmHandler, ShmState},
    },
};
use wayland_protocols::xdg::shell::server::xdg_toplevel;
use wayland_server::{
    Client, ListeningSocket,
    backend::{ClientData, ClientId, DisconnectReason},
    protocol::{wl_buffer, wl_surface::WlSurface},
};

const DEFAULT_SOCKET: &str = "wayland-devcore-nested";
const WORKSPACE_COUNT: u8 = 4;

#[derive(Debug, Clone)]
struct ManagedWindow {
    id: u64,
    surface: ToplevelSurface,
    workspace: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CompositorConfig {
    backend: Backend,
    socket: String,
    shell: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Backend {
    Nested,
    NativeDrm,
}

#[derive(Debug)]
struct App {
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    shm_state: ShmState,
    seat_state: SeatState<Self>,
    data_device_state: DataDeviceState,
    seat: Seat<Self>,
    render_requested: bool,
    windows: Vec<ManagedWindow>,
    next_window_id: u64,
    active_workspace: u8,
    focused_window: Option<u64>,
}

impl App {
    fn register_window(&mut self, surface: ToplevelSurface) {
        let id = self.next_window_id;
        self.next_window_id = self.next_window_id.saturating_add(1);
        self.windows.push(ManagedWindow {
            id,
            surface,
            workspace: self.active_workspace,
        });
        self.focused_window = Some(id);
        self.render_requested = true;
    }

    fn prune_windows(&mut self) {
        self.windows.retain(|window| window.surface.alive());
        if self
            .focused_window
            .is_some_and(|id| !self.windows.iter().any(|window| window.id == id))
        {
            self.focused_window = None;
        }
    }

    fn visible_windows(&mut self) -> Vec<ToplevelSurface> {
        self.prune_windows();
        self.windows
            .iter()
            .filter(|window| window.workspace == self.active_workspace)
            .map(|window| window.surface.clone())
            .collect()
    }

    fn focused_surface(&mut self) -> Option<WlSurface> {
        self.prune_windows();
        self.windows
            .iter()
            .rev()
            .find(|window| {
                window.workspace == self.active_workspace && Some(window.id) == self.focused_window
            })
            .or_else(|| {
                self.windows
                    .iter()
                    .rev()
                    .find(|window| window.workspace == self.active_workspace)
            })
            .map(|window| window.surface.wl_surface().clone())
    }

    fn pointer_focus_at(
        &mut self,
        location: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.prune_windows();
        let visible = self
            .windows
            .iter()
            .filter(|window| window.workspace == self.active_workspace)
            .collect::<Vec<_>>();

        visible
            .iter()
            .enumerate()
            .rev()
            .find_map(|(stack_index, window)| {
                under_from_surface_tree(
                    window.surface.wl_surface(),
                    location,
                    window_location(stack_index),
                    WindowSurfaceType::ALL,
                )
                .map(|(surface, surface_location)| (surface, surface_location.to_f64()))
            })
    }

    fn focus_window_at(&mut self, location: Point<f64, Logical>, serial: Serial) {
        self.prune_windows();
        let visible = self
            .windows
            .iter()
            .filter(|window| window.workspace == self.active_workspace)
            .collect::<Vec<_>>();
        let focused = visible
            .iter()
            .enumerate()
            .rev()
            .find_map(|(stack_index, window)| {
                under_from_surface_tree(
                    window.surface.wl_surface(),
                    location,
                    window_location(stack_index),
                    WindowSurfaceType::ALL,
                )
                .map(|_| (window.id, window.surface.wl_surface().clone()))
            });

        if let Some((id, surface)) = focused {
            self.focused_window = Some(id);
            if let Some(keyboard) = self.seat.get_keyboard() {
                keyboard.set_focus(self, Some(surface), serial);
            }
        }
    }

    fn dispatch_pointer_motion(&mut self, location: Point<f64, Logical>, time: u32) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let focus = self.pointer_focus_at(location);
        pointer.motion(
            self,
            focus,
            &MotionEvent {
                location,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);

        // Keep the pre-existing keyboard behavior: pointer movement ensures the
        // active workspace's focused toplevel receives subsequent key events.
        if let Some(surface) = self.focused_surface()
            && let Some(keyboard) = self.seat.get_keyboard()
        {
            keyboard.set_focus(self, Some(surface), SERIAL_COUNTER.next_serial());
        }
    }

    fn dispatch_pointer_button<B: InputBackend>(&mut self, event: B::PointerButtonEvent) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        if event.state() == smithay::backend::input::ButtonState::Pressed {
            self.focus_window_at(pointer.current_location(), serial);
        }
        pointer.button(
            self,
            &ButtonEvent {
                serial,
                time: event.time_msec(),
                button: event.button_code(),
                state: event.state(),
            },
        );
        pointer.frame(self);
    }

    fn dispatch_pointer_axis<B: InputBackend>(&mut self, event: B::PointerAxisEvent) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let mut frame = AxisFrame::new(event.time_msec()).source(event.source());
        for axis in [Axis::Horizontal, Axis::Vertical] {
            let amount = event
                .amount(axis)
                .unwrap_or_else(|| event.amount_v120(axis).unwrap_or(0.0) * 15.0 / 120.0);
            if amount != 0.0 {
                frame = frame
                    .relative_direction(axis, event.relative_direction(axis))
                    .value(axis, amount);
                if let Some(v120) = event.amount_v120(axis) {
                    frame = frame.v120(axis, v120 as i32);
                }
            }
            if event.source() == AxisSource::Finger && event.amount(axis) == Some(0.0) {
                frame = frame.stop(axis);
            }
        }
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    fn switch_workspace(&mut self, workspace: u8) {
        self.active_workspace = workspace.clamp(1, WORKSPACE_COUNT);
        self.focused_window = self
            .windows
            .iter()
            .rev()
            .find(|window| window.workspace == self.active_workspace)
            .map(|window| window.id);
        self.render_requested = true;
    }
}

fn window_location(stack_index: usize) -> (i32, i32) {
    let offset = (stack_index as i32).saturating_mul(32);
    (offset, offset)
}

fn clamp_pointer_location(
    location: Point<f64, Logical>,
    output_size: Size<i32, Logical>,
) -> Point<f64, Logical> {
    let max_x = f64::from(output_size.w.saturating_sub(1).max(0));
    let max_y = f64::from(output_size.h.saturating_sub(1).max(0));
    (location.x.clamp(0.0, max_x), location.y.clamp(0.0, max_y)).into()
}

fn workspace_from_keysym(value: u32) -> Option<u8> {
    match value {
        keysyms::KEY_1 => Some(1),
        keysyms::KEY_2 => Some(2),
        keysyms::KEY_3 => Some(3),
        keysyms::KEY_4 => Some(4),
        _ => None,
    }
}

impl BufferHandler for App {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl XdgShellHandler for App {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();
        self.register_window(surface);
    }

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
    }
}

impl SelectionHandler for App {
    type SelectionUserData = ();
}

impl OutputHandler for App {}

impl DataDeviceHandler for App {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for App {}

impl ServerDndGrabHandler for App {
    fn send(&mut self, _mime_type: String, _fd: OwnedFd, _seat: Seat<Self>) {}
}

impl CompositorHandler for App {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<ClientState>()
            .expect("clients are inserted with compositor state")
            .compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        self.render_requested = true;
        on_commit_buffer_handler::<Self>(surface);
    }
}

impl ShmHandler for App {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl SeatHandler for App {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}

    fn cursor_image(
        &mut self,
        _seat: &Seat<Self>,
        _image: smithay::input::pointer::CursorImageStatus,
    ) {
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    } else {
        tracing_subscriber::fmt().init();
    }
    let config = parse_arguments(env::args().skip(1))?;
    match config.backend {
        Backend::Nested => run_nested(&config),
        Backend::NativeDrm => run_native(&config),
    }
}

fn run_native(config: &CompositorConfig) -> Result<(), Box<dyn Error>> {
    #[cfg(feature = "native-drm")]
    {
        native::run(config)
    }

    #[cfg(not(feature = "native-drm"))]
    {
        let _ = config;
        Err(
            "native DRM backend is disabled; rebuild devcore-compositor with --features native-drm"
                .into(),
        )
    }
}

fn parse_arguments(
    arguments: impl IntoIterator<Item = String>,
) -> Result<CompositorConfig, String> {
    let mut backend = Backend::Nested;
    let mut socket = DEFAULT_SOCKET.to_owned();
    let mut shell = None;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--backend" => {
                backend = match arguments
                    .next()
                    .ok_or_else(|| "--backend requires nested or drm".to_owned())?
                    .as_str()
                {
                    "nested" => Backend::Nested,
                    "drm" | "native-drm" => Backend::NativeDrm,
                    other => return Err(format!("unknown compositor backend {other:?}")),
                };
            }
            "--socket" => {
                socket = arguments
                    .next()
                    .ok_or_else(|| "--socket requires a name".to_owned())?;
                if socket.is_empty() || socket.contains(['/', '\0']) {
                    return Err("--socket must be a non-empty Wayland name".to_owned());
                }
            }
            "--shell" => {
                let path = PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| "--shell requires an absolute executable".to_owned())?,
                );
                if !path.is_absolute() {
                    return Err("--shell must be an absolute executable path".to_owned());
                }
                shell = Some(path);
            }
            "--help" | "-h" => {
                return Err(
                    "usage: devcore-compositor [--backend nested|drm] [--shell PATH] [--socket NAME]"
                        .to_owned(),
                );
            }
            _ => return Err(format!("unknown compositor argument {argument:?}")),
        }
    }
    Ok(CompositorConfig {
        backend,
        socket,
        shell,
    })
}

fn run_nested(config: &CompositorConfig) -> Result<(), Box<dyn Error>> {
    let mut display: Display<App> = Display::new()?;
    let display_handle = display.handle();
    let compositor_state = CompositorState::new::<App>(&display_handle);
    let shm_state = ShmState::new::<App>(&display_handle, vec![]);
    let mut seat_state = SeatState::new();
    let mut seat = seat_state.new_wl_seat(&display_handle, "devcore-seat");
    seat.add_pointer();
    let mut state = App {
        compositor_state,
        xdg_shell_state: XdgShellState::new::<App>(&display_handle),
        shm_state,
        seat_state,
        data_device_state: DataDeviceState::new::<App>(&display_handle),
        seat,
        render_requested: false,
        windows: Vec::new(),
        next_window_id: 1,
        active_workspace: 1,
        focused_window: None,
    };

    let listener = ListeningSocket::bind(&config.socket)?;
    let mut clients = Vec::new();
    let (mut backend, mut winit) = winit::init::<GlesRenderer>()?;
    let start_time = Instant::now();
    let mut shell = config
        .shell
        .as_ref()
        .map(|path| {
            let mut command = Command::new(path);
            command.env("WAYLAND_DISPLAY", &config.socket);
            command
                .spawn()
                .map(ShellProcess)
                .map_err(|error| format!("cannot launch shell {}: {error}", path.display()))
        })
        .transpose()?;
    let keyboard = state
        .seat
        .add_keyboard(Default::default(), 200, 200)
        .map_err(|error| format!("cannot create compositor keyboard: {error:?}"))?;

    loop {
        let status = winit.dispatch_new_events(|event| match event {
            WinitEvent::Resized { .. } => {}
            WinitEvent::Input(event) => match event {
                InputEvent::Keyboard { event } => {
                    let key_state = event.state();
                    let switched = keyboard.input::<(), _>(
                        &mut state,
                        event.key_code(),
                        key_state,
                        0.into(),
                        0,
                        |data, modifiers, key| {
                            if key_state == KeyState::Pressed && modifiers.logo {
                                let workspace = workspace_from_keysym(key.modified_sym().into());
                                if let Some(workspace) = workspace {
                                    data.switch_workspace(workspace);
                                    return FilterResult::Intercept(());
                                }
                            }
                            FilterResult::Forward
                        },
                    );
                    if switched.is_some()
                        && let Some(surface) = state.focused_surface()
                    {
                        keyboard.set_focus(&mut state, Some(surface), 0.into());
                    }
                }
                InputEvent::PointerMotionAbsolute { event } => {
                    let output_size = backend.window_size().to_logical(1);
                    let location = event.position_transformed(output_size);
                    state.dispatch_pointer_motion(location, event.time_msec());
                }
                InputEvent::PointerButton { event } => {
                    state.dispatch_pointer_button::<winit::WinitInput>(event)
                }
                InputEvent::PointerAxis { event } => state.dispatch_pointer_axis(event),
                _ => {}
            },
            _ => {}
        });

        match status {
            PumpStatus::Continue => {}
            PumpStatus::Exit(_) => return Ok(()),
        }

        if shell
            .as_mut()
            .map(ShellProcess::try_wait)
            .transpose()?
            .flatten()
            .is_some()
        {
            return Ok(());
        }

        let size = backend.window_size();
        let damage = Rectangle::from_size(size);
        {
            let (renderer, mut framebuffer) = backend.bind()?;
            let windows = state.visible_windows();
            let elements = windows
                .iter()
                .enumerate()
                .flat_map(|(index, surface)| {
                    render_elements_from_surface_tree(
                        renderer,
                        surface.wl_surface(),
                        window_location(index),
                        1.0,
                        1.0,
                        Kind::Unspecified,
                    )
                })
                .collect::<Vec<WaylandSurfaceRenderElement<GlesRenderer>>>();
            let mut frame = renderer.render(&mut framebuffer, size, Transform::Flipped180)?;
            frame.clear(Color32F::new(0.06, 0.08, 0.12, 1.0), &[damage])?;
            draw_render_elements(&mut frame, 1.0, &elements, &[damage])?;
            let _ = frame.finish()?;

            for surface in state.xdg_shell_state.toplevel_surfaces() {
                send_frames_surface_tree(
                    surface.wl_surface(),
                    start_time.elapsed().as_millis() as u32,
                );
            }

            if let Some(stream) = listener.accept()? {
                let client = display
                    .handle()
                    .insert_client(stream, Arc::new(ClientState::default()))?;
                clients.push(client);
            }
            display.dispatch_clients(&mut state)?;
            display.flush_clients()?;
        }
        backend.submit(Some(&[damage]))?;
    }
}

struct ShellProcess(Child);

impl ShellProcess {
    fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>, std::io::Error> {
        self.0.try_wait()
    }
}

impl Drop for ShellProcess {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn send_frames_surface_tree(surface: &WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_surface, states, &()| {
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
}

#[derive(Default, Debug)]
struct ClientState {
    compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}

    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

delegate_xdg_shell!(App);
delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_seat!(App);
delegate_data_device!(App);

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use smithay::input::keyboard::keysyms;

    use super::{
        CompositorConfig, DEFAULT_SOCKET, WORKSPACE_COUNT, clamp_pointer_location, parse_arguments,
        window_location, workspace_from_keysym,
    };

    #[test]
    fn nested_socket_name_is_private_to_devcore() {
        assert_eq!(DEFAULT_SOCKET, "wayland-devcore-nested");
        assert!(!DEFAULT_SOCKET.contains('/'));
    }

    #[test]
    fn launch_contract_accepts_an_absolute_shell() {
        assert_eq!(
            parse_arguments([
                "--shell".to_owned(),
                "/usr/bin/devcore-shell".to_owned(),
                "--socket".to_owned(),
                "wayland-test".to_owned(),
            ]),
            Ok(CompositorConfig {
                backend: super::Backend::Nested,
                socket: "wayland-test".to_owned(),
                shell: Some(PathBuf::from("/usr/bin/devcore-shell")),
            })
        );
    }

    #[test]
    fn launch_contract_rejects_relative_shell_and_unsafe_socket() {
        assert!(parse_arguments(["--shell".to_owned(), "devcore-shell".to_owned()]).is_err());
        assert!(parse_arguments(["--socket".to_owned(), "../wayland".to_owned()]).is_err());
    }

    #[test]
    fn native_backend_is_explicitly_opt_in() {
        assert_eq!(
            parse_arguments(["--backend".to_owned(), "drm".to_owned()])
                .expect("valid backend")
                .backend,
            super::Backend::NativeDrm
        );
    }

    #[test]
    fn workspace_shortcuts_and_stack_locations_are_bounded() {
        assert_eq!(WORKSPACE_COUNT, 4);
        assert_eq!(workspace_from_keysym(keysyms::KEY_1), Some(1));
        assert_eq!(workspace_from_keysym(keysyms::KEY_4), Some(4));
        assert_eq!(workspace_from_keysym(keysyms::KEY_0), None);
        assert_eq!(window_location(0), (0, 0));
        assert_eq!(window_location(2), (64, 64));
    }

    #[test]
    fn pointer_location_is_clamped_inside_the_output() {
        let output_size = smithay::utils::Size::from((1920, 1080));
        assert_eq!(
            clamp_pointer_location((-24.5, 2048.0).into(), output_size),
            (0.0, 1079.0).into()
        );
        assert_eq!(
            clamp_pointer_location((960.5, 540.25).into(), output_size),
            (960.5, 540.25).into()
        );
    }
}
