#![allow(missing_docs)]

use std::{
    error::Error,
    io,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode,
            compositor::FrameFlags,
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        egl::context::ContextPriority,
        input::{AbsolutePositionEvent, Event, InputEvent, KeyboardKeyEvent, PointerMotionEvent},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            element::{
                Kind,
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            },
            gles::GlesRenderer,
            multigpu::{GpuManager, MultiRenderer, gbm::GbmGlesBackend},
        },
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{UdevBackend, UdevEvent},
    },
    input::keyboard::FilterResult,
    output::{Mode as WaylandMode, Output, PhysicalProperties, Scale},
    reexports::{
        calloop::EventLoop,
        drm::control::{ModeTypeFlags, crtc},
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::{Client, Display, DisplayHandle, ListeningSocket, backend::GlobalId},
    },
    utils::{DeviceFd, Transform},
};
use tracing::{error, info, warn};

use super::{App, ClientState, CompositorConfig, ShellProcess, send_frames_surface_tree};

type GpuBackend = GbmGlesBackend<GlesRenderer, DrmDeviceFd>;
type NativeRenderer<'a> = MultiRenderer<'a, 'a, GpuBackend, GpuBackend>;
type OutputManager = DrmOutputManager<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    (),
    DrmDeviceFd,
>;
type OutputHandle =
    DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

struct NativeState {
    display: Display<App>,
    app: App,
    listener: ListeningSocket,
    clients: Vec<Client>,
    output_manager: OutputManager,
    output: OutputHandle,
    _output_global: GlobalId,
    session: LibSeatSession,
    shell: Option<ShellProcess>,
    gpu_node: DrmNode,
    crtc: crtc::Handle,
    frame_queued: bool,
    started_at: Instant,
}

pub(crate) fn run(config: &CompositorConfig) -> Result<(), Box<dyn Error>> {
    let mut event_loop = EventLoop::<NativeState>::try_new()?;
    let display: Display<App> = Display::new()?;
    let display_handle = display.handle();
    let app = create_app(&display_handle);

    let (mut session, notifier) = LibSeatSession::new()?;
    let seat_name = session.seat();
    let udev_backend = UdevBackend::new(&seat_name)?;
    let (device_id, device_path) = udev_backend
        .device_list()
        .next()
        .map(|(device_id, path)| (device_id, path.to_owned()))
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no DRM device for the active seat")
        })?;
    let gpu_node = DrmNode::from_dev_id(device_id)?;

    let opened_device = session.open(
        &device_path,
        OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
    )?;
    let drm_fd = DrmDeviceFd::new(DeviceFd::from(opened_device));
    let (drm_device, drm_notifier) = DrmDevice::new(drm_fd.clone(), true)?;
    let gbm = GbmDevice::new(drm_fd.clone())?;

    let mut gpus = GpuManager::new(GpuBackend::with_context_priority(ContextPriority::High))?;
    gpus.as_mut().add_node(gpu_node, gbm.clone())?;
    let mut renderer = gpus.single_renderer(&gpu_node)?;
    let render_formats = renderer
        .as_ref()
        .egl_context()
        .dmabuf_render_formats()
        .iter()
        .copied()
        .collect::<Vec<_>>();
    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let framebuffer_exporter = GbmFramebufferExporter::new(gbm.clone(), Some(gpu_node));
    let mut output_manager = OutputManager::new(
        drm_device,
        allocator,
        framebuffer_exporter,
        Some(gbm),
        [Fourcc::Argb8888, Fourcc::Abgr8888],
        render_formats,
    );

    let mut scanner = smithay_drm_extras::drm_scanner::DrmScanner::<
        smithay_drm_extras::drm_scanner::SimpleCrtcMapper,
    >::new();
    let (connector, crtc) = scanner
        .scan_connectors(output_manager.device())?
        .connected
        .into_iter()
        .find_map(|(connector, crtc)| crtc.map(|crtc| (connector, crtc)))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no connected DRM output for the active seat",
            )
        })?;
    let drm_mode = connector
        .modes()
        .iter()
        .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
        .copied()
        .or_else(|| connector.modes().first().copied())
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "connected DRM output has no mode")
        })?;
    let planes = output_manager.device().planes(&crtc)?;
    let wl_mode = WaylandMode::from(drm_mode);
    let output_size = wl_mode.size.to_logical(1);
    let (physical_width, physical_height) = connector.size().unwrap_or((0, 0));
    let output = Output::new(
        "DevCore-0".to_owned(),
        PhysicalProperties {
            size: (physical_width as i32, physical_height as i32).into(),
            subpixel: connector.subpixel().into(),
            make: "DevCore".to_owned(),
            model: "DRM output".to_owned(),
        },
    );
    output.set_preferred(wl_mode);
    output.change_current_state(
        Some(wl_mode),
        Some(Transform::Normal),
        Some(Scale::Integer(1)),
        Some((0, 0).into()),
    );
    let output_global = output.create_global::<App>(&display_handle);
    let native_render_elements: DrmOutputRenderElements<
        NativeRenderer<'_>,
        WaylandSurfaceRenderElement<NativeRenderer<'_>>,
    > = DrmOutputRenderElements::default();
    let output_handle = output_manager.initialize_output(
        crtc,
        drm_mode,
        &[connector.handle()],
        &output,
        Some(planes),
        &mut renderer,
        &native_render_elements,
    )?;
    drop(renderer);

    let shell = config
        .shell
        .as_ref()
        .map(|path| spawn_shell(path, config))
        .transpose()?;
    let listener = ListeningSocket::bind(&config.socket)?;
    let mut runtime = NativeState {
        display,
        app,
        listener,
        clients: Vec::new(),
        output_manager,
        output: output_handle,
        _output_global: output_global,
        session,
        shell,
        gpu_node,
        crtc,
        frame_queued: false,
        started_at: Instant::now(),
    };

    event_loop
        .handle()
        .insert_source(drm_notifier, |event, _, data| match event {
            DrmEvent::VBlank(vblank_crtc) if vblank_crtc == data.crtc => {
                match data.output.frame_submitted() {
                    Ok(_) => data.frame_queued = false,
                    Err(error) => warn!(?error, "failed to retire DRM frame"),
                }
            }
            DrmEvent::Error(error) => error!(?error, "DRM device reported an error"),
            _ => {}
        })?;

    let mut libinput_context = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(
        runtime.session.clone().into(),
    );
    libinput_context
        .udev_assign_seat(&runtime.session.seat())
        .map_err(|()| io::Error::other("libinput could not attach to the active seat"))?;
    let libinput_backend = LibinputInputBackend::new(libinput_context.clone());
    event_loop
        .handle()
        .insert_source(libinput_backend, |event, _, data| {
            match event {
                InputEvent::Keyboard { event } => {
                    if let Some(keyboard) = data.app.seat.get_keyboard() {
                        let key_state = event.state();
                        let switched = keyboard.input::<(), _>(
                            &mut data.app,
                            event.key_code(),
                            key_state,
                            0.into(),
                            0,
                            |app, modifiers, key| {
                                if key_state == smithay::backend::input::KeyState::Pressed
                                    && modifiers.logo
                                {
                                    let workspace =
                                        super::workspace_from_keysym(key.modified_sym().into());
                                    if let Some(workspace) = workspace {
                                        app.switch_workspace(workspace);
                                        return FilterResult::Intercept(());
                                    }
                                }
                                FilterResult::Forward
                            },
                        );
                        if switched.is_some()
                            && let Some(surface) = data.app.focused_surface()
                        {
                            keyboard.set_focus(&mut data.app, Some(surface), 0.into());
                        }
                    }
                }
                InputEvent::PointerMotion { event } => {
                    let location = super::clamp_pointer_location(
                        data.app
                            .seat
                            .get_pointer()
                            .map(|pointer| pointer.current_location())
                            .unwrap_or_default()
                            + event.delta(),
                        output_size,
                    );
                    data.app
                        .dispatch_pointer_motion(location, event.time_msec());
                }
                InputEvent::PointerMotionAbsolute { event } => {
                    let location = super::clamp_pointer_location(
                        event.position_transformed(output_size),
                        output_size,
                    );
                    data.app
                        .dispatch_pointer_motion(location, event.time_msec());
                }
                InputEvent::PointerButton { event } => {
                    data.app
                        .dispatch_pointer_button::<LibinputInputBackend>(event);
                }
                InputEvent::PointerAxis { event } => {
                    data.app
                        .dispatch_pointer_axis::<LibinputInputBackend>(event);
                }
                _ => {}
            }
            data.app.render_requested = true;
        })?;

    event_loop
        .handle()
        .insert_source(notifier, move |event, _, data| match event {
            SessionEvent::PauseSession => {
                libinput_context.suspend();
                data.output_manager.pause();
                data.frame_queued = false;
                info!("pausing native graphics session");
            }
            SessionEvent::ActivateSession => {
                if let Err(error) = libinput_context.resume() {
                    error!(?error, "failed to resume native input session");
                }
                if let Err(error) = data.output_manager.activate(false) {
                    error!(?error, "failed to reactivate native DRM output");
                }
                data.app.render_requested = true;
                info!("resuming native graphics session");
            }
        })?;

    event_loop
        .handle()
        .insert_source(udev_backend, |event, _, _| match event {
            UdevEvent::Added { device_id, path } => {
                info!(
                    ?device_id,
                    ?path,
                    "DRM device added; hotplug re-probe is pending"
                );
            }
            UdevEvent::Changed { device_id } => {
                info!(
                    ?device_id,
                    "DRM device changed; connector re-scan is pending"
                );
            }
            UdevEvent::Removed { device_id } => {
                warn!(
                    ?device_id,
                    "DRM device removed; compositor will stop on output loss"
                );
            }
        })?;

    runtime.app.render_requested = true;
    render_next_frame(&mut runtime, &mut gpus)?;

    loop {
        event_loop.dispatch(Some(Duration::from_millis(16)), &mut runtime)?;
        while let Some(stream) = runtime.listener.accept()? {
            let client = runtime
                .display
                .handle()
                .insert_client(stream, std::sync::Arc::new(ClientState::default()))?;
            runtime.clients.push(client);
        }
        runtime.display.dispatch_clients(&mut runtime.app)?;
        runtime.display.flush_clients()?;

        if runtime
            .shell
            .as_mut()
            .map(ShellProcess::try_wait)
            .transpose()?
            .flatten()
            .is_some()
        {
            return Ok(());
        }

        if !runtime.frame_queued && runtime.app.render_requested {
            render_next_frame(&mut runtime, &mut gpus)?;
        }
    }
}

fn create_app(display_handle: &DisplayHandle) -> App {
    let compositor_state =
        smithay::wayland::compositor::CompositorState::new::<App>(display_handle);
    let shm_state = smithay::wayland::shm::ShmState::new::<App>(display_handle, vec![]);
    let mut seat_state = smithay::input::SeatState::new();
    let mut seat = seat_state.new_wl_seat(display_handle, "devcore-seat");
    seat.add_pointer();
    let mut app = App {
        compositor_state,
        xdg_shell_state: smithay::wayland::shell::xdg::XdgShellState::new::<App>(display_handle),
        shm_state,
        seat_state,
        data_device_state: smithay::wayland::selection::data_device::DataDeviceState::new::<App>(
            display_handle,
        ),
        seat,
        render_requested: false,
        windows: Vec::new(),
        next_window_id: 1,
        active_workspace: 1,
        focused_window: None,
    };
    if let Err(error) = app.seat.add_keyboard(Default::default(), 200, 200) {
        warn!(
            ?error,
            "native keyboard initialization failed; continuing without keyboard"
        );
    }
    app
}

fn spawn_shell(path: &PathBuf, config: &CompositorConfig) -> Result<ShellProcess, io::Error> {
    let mut command = Command::new(path);
    command
        .env("WAYLAND_DISPLAY", &config.socket)
        .env("XDG_SESSION_TYPE", "wayland")
        .env("XDG_CURRENT_DESKTOP", "DevCore")
        .spawn()
        .map(ShellProcess)
}

fn render_next_frame(
    runtime: &mut NativeState,
    gpus: &mut GpuManager<GpuBackend>,
) -> Result<(), Box<dyn Error>> {
    let mut renderer = gpus.single_renderer(&runtime.gpu_node)?;
    let windows = runtime.app.visible_windows();
    let elements: Vec<WaylandSurfaceRenderElement<NativeRenderer<'_>>> = windows
        .iter()
        .enumerate()
        .flat_map(|(index, surface)| {
            render_elements_from_surface_tree(
                &mut renderer,
                surface.wl_surface(),
                super::window_location(index),
                1.0,
                1.0,
                Kind::Unspecified,
            )
        })
        .collect::<Vec<_>>();
    let frame = runtime.output.render_frame(
        &mut renderer,
        &elements,
        smithay::backend::renderer::Color32F::new(0.06, 0.08, 0.12, 1.0),
        FrameFlags::empty(),
    )?;
    if !frame.is_empty {
        runtime.output.queue_frame(())?;
        runtime.frame_queued = true;
        for surface in runtime.app.xdg_shell_state.toplevel_surfaces() {
            send_frames_surface_tree(
                surface.wl_surface(),
                runtime.started_at.elapsed().as_millis() as u32,
            );
        }
    }
    runtime.app.render_requested = false;
    Ok(())
}
