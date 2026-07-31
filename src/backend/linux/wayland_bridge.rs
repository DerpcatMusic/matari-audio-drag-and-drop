//! Serial-less native Wayland drag source for XWayland plugin editors.
//!
//! Hyprland accepts `wl_data_device.start_drag` with serial zero and a
//! role-less origin surface. This lets an XWayland editor use a private
//! Wayland connection while the compositor owns delivery to both native
//! Wayland and X11 targets.

use std::error::Error;
use std::fmt;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use smithay_client_toolkit::{
    data_device_manager::{
        DataDeviceManagerState, WritePipe,
        data_device::{DataDevice, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
        data_source::{DataSourceHandler, DragSource},
    },
    reexports::{
        calloop::{EventLoop, LoopSignal, channel},
        calloop_wayland_source::WaylandSource,
    },
    seat::{Capability, SeatHandler, SeatState},
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_compositor::WlCompositor, wl_data_device::WlDataDevice,
        wl_data_device_manager::DndAction, wl_data_source::WlDataSource, wl_registry,
        wl_seat::WlSeat, wl_shm, wl_surface::WlSurface,
    },
};

use crate::{
    FailureKind, FailureStage, FileDragOffer, FileSet, LinuxOutcome, Outcome, SessionFailure,
    SessionReporter, WaylandSourceReporter,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WaylandBridgeError {
    message: Box<str>,
}

impl WaylandBridgeError {
    fn new(message: impl Into<Box<str>>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for WaylandBridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for WaylandBridgeError {}

pub(crate) fn available() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        && (std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
            || std::env::var("XDG_CURRENT_DESKTOP")
                .is_ok_and(|desktop| desktop.to_ascii_lowercase().contains("hyprland")))
}

struct PreparedWaylandBridge {
    connection: Connection,
    event_queue: wayland_client::EventQueue<BridgeState>,
    seat_state: SeatState,
    shm: Shm,
    data_device: DataDevice,
    source: DragSource,
    origin: WlSurface,
    icon: Option<DragIcon>,
    offers: Vec<FileDragOffer>,
}

impl PreparedWaylandBridge {
    fn prepare(files: &FileSet) -> Result<Self, WaylandBridgeError> {
        let offers = files.offers();
        let preview = files.preview().cloned();
        let connection = Connection::connect_to_env()
            .map_err(|error| WaylandBridgeError::new(format!("Wayland connect failed: {error}")))?;
        let (globals, event_queue) =
            registry_queue_init::<BridgeState>(&connection).map_err(|error| {
                WaylandBridgeError::new(format!("Wayland registry failed: {error}"))
            })?;
        let queue = event_queue.handle();
        let compositor: WlCompositor = globals
            .bind(&queue, 1..=6, ())
            .map_err(|error| WaylandBridgeError::new(format!("wl_compositor failed: {error}")))?;
        let manager = DataDeviceManagerState::bind(&globals, &queue)
            .map_err(|error| WaylandBridgeError::new(format!("data device failed: {error}")))?;
        let shm = Shm::bind(&globals, &queue)
            .map_err(|error| WaylandBridgeError::new(format!("wl_shm failed: {error}")))?;
        let seat_state = SeatState::new(&globals, &queue);
        let seat = seat_state
            .seats()
            .next()
            .ok_or_else(|| WaylandBridgeError::new("Wayland session has no wl_seat"))?;
        let data_device = manager.get_data_device(&queue, &seat);
        let origin = compositor.create_surface(&queue, ());
        let icon = preview
            .as_ref()
            .map(|preview| DragIcon::new(&compositor, &shm, &queue, preview))
            .transpose()?;
        let mime_types: Vec<String> = offers
            .iter()
            .map(|offer| offer.mime_type().to_owned())
            .collect();
        let source = manager.create_drag_and_drop_source(&queue, mime_types, DndAction::Copy);
        source.start_drag(
            &data_device,
            &origin,
            icon.as_ref().map(|icon| &icon.surface),
            0,
        );
        connection
            .flush()
            .map_err(|error| WaylandBridgeError::new(format!("Wayland flush failed: {error}")))?;
        Ok(Self {
            connection,
            event_queue,
            seat_state,
            shm,
            data_device,
            source,
            origin,
            icon,
            offers,
        })
    }
}

pub(super) struct WaylandBridgeSession {
    commands: Option<channel::Sender<Command>>,
    transfer_ready: Arc<AtomicBool>,
    terminal: Arc<AtomicBool>,
}

impl WaylandBridgeSession {
    pub(super) fn start(
        files: FileSet,
        reporter: SessionReporter,
    ) -> Result<Self, WaylandBridgeError> {
        let prepared = PreparedWaylandBridge::prepare(&files)?;
        let transfer_ready = Arc::new(AtomicBool::new(false));
        let terminal = Arc::new(AtomicBool::new(false));
        let (commands, command_source) = channel::channel();
        let worker_ready = Arc::clone(&transfer_ready);
        let worker_terminal = Arc::clone(&terminal);
        thread::Builder::new()
            .name("matari-wayland-dnd".to_owned())
            .spawn(move || {
                let mut state = BridgeState {
                    seat_state: prepared.seat_state,
                    shm: prepared.shm,
                    _data_device: prepared.data_device,
                    source: Some(prepared.source),
                    _origin: prepared.origin,
                    _icon: prepared.icon,
                    offers: prepared.offers,
                    reporter: WaylandSourceReporter::new(reporter),
                    transfer_ready: worker_ready,
                    terminal: worker_terminal,
                    signal: None,
                };
                if run_queue(
                    prepared.connection,
                    prepared.event_queue,
                    command_source,
                    &mut state,
                )
                .is_err()
                    && !state.is_terminal()
                {
                    state.fail();
                }
            })
            .map_err(|error| {
                WaylandBridgeError::new(format!("Wayland drag thread failed: {error}"))
            })?;
        Ok(WaylandBridgeSession {
            commands: Some(commands),
            transfer_ready,
            terminal,
        })
    }

    pub(super) fn transfer_complete(&self) -> bool {
        self.transfer_ready.load(Ordering::Acquire)
    }

    pub(super) fn is_terminal(&self) -> bool {
        self.terminal.load(Ordering::Acquire)
    }

    pub(super) fn cancel(&self) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::Cancel);
        }
    }

    pub(super) fn detach(mut self) {
        self.commands.take();
    }
}

fn run_queue(
    connection: Connection,
    event_queue: wayland_client::EventQueue<BridgeState>,
    command_source: channel::Channel<Command>,
    state: &mut BridgeState,
) -> Result<(), WaylandBridgeError> {
    let mut event_loop = EventLoop::try_new()
        .map_err(|error| WaylandBridgeError::new(format!("event queue failed: {error}")))?;
    WaylandSource::new(connection, event_queue)
        .insert(event_loop.handle())
        .map_err(|error| WaylandBridgeError::new(format!("Wayland source failed: {error}")))?;
    state.signal = Some(event_loop.get_signal());
    event_loop
        .handle()
        .insert_source(command_source, |event, _, state| {
            if matches!(event, channel::Event::Msg(Command::Cancel)) {
                state.finish(LinuxOutcome::Cancelled);
            }
        })
        .map_err(|error| WaylandBridgeError::new(format!("cancel source failed: {error}")))?;
    event_loop
        .run(None, state, |_| {})
        .map_err(|error| WaylandBridgeError::new(format!("Wayland dispatch failed: {error}")))
}

impl fmt::Debug for WaylandBridgeSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WaylandBridgeSession")
            .field("transfer_ready", &self.transfer_complete())
            .field("terminal", &self.is_terminal())
            .finish()
    }
}

impl Drop for WaylandBridgeSession {
    fn drop(&mut self) {
        if !self.is_terminal() {
            self.cancel();
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Command {
    Cancel,
}

struct BridgeState {
    seat_state: SeatState,
    shm: Shm,
    _data_device: DataDevice,
    source: Option<DragSource>,
    _origin: WlSurface,
    _icon: Option<DragIcon>,
    offers: Vec<FileDragOffer>,
    reporter: WaylandSourceReporter,
    transfer_ready: Arc<AtomicBool>,
    terminal: Arc<AtomicBool>,
    signal: Option<LoopSignal>,
}

impl BridgeState {
    fn owns(&self, source: &WlDataSource) -> bool {
        self.source
            .as_ref()
            .is_some_and(|active| active.inner() == source)
    }

    fn is_terminal(&self) -> bool {
        self.terminal.load(Ordering::Acquire)
    }

    fn sync_transfer_ready(&self) {
        if self.reporter.is_transfer_ready() {
            self.transfer_ready.store(true, Ordering::Release);
        }
    }

    fn finish(&mut self, outcome: LinuxOutcome) {
        if self.terminal.swap(true, Ordering::AcqRel) {
            return;
        }
        drop(self.source.take());
        self.reporter.finish_linux(outcome);
        if let Some(signal) = &self.signal {
            signal.stop();
        }
    }

    fn fail(&mut self) {
        if self.terminal.swap(true, Ordering::AcqRel) {
            return;
        }
        drop(self.source.take());
        self.reporter.finish(Outcome::Failed(SessionFailure {
            stage: FailureStage::Transfer,
            kind: FailureKind::NativeFailure,
        }));
        if let Some(signal) = &self.signal {
            signal.stop();
        }
    }

    fn send_payload(&mut self, mime: &str, mut pipe: WritePipe) {
        let Some(offer) = self.offers.iter().find(|offer| offer.mime_type() == mime) else {
            return;
        };
        if pipe
            .write_all(offer.data())
            .and_then(|()| pipe.flush())
            .is_ok()
        {
            self.reporter.data_requested();
            self.sync_transfer_ready();
        }
    }
}

struct DragIcon {
    surface: WlSurface,
    _pool: SlotPool,
    _buffer: Buffer,
}

impl DragIcon {
    fn new(
        compositor: &WlCompositor,
        shm: &Shm,
        queue: &QueueHandle<BridgeState>,
        preview: &crate::DragPreview,
    ) -> Result<Self, WaylandBridgeError> {
        const STRIDE: i32 = crate::preview::WIDTH as i32 * 4;

        let surface = compositor.create_surface(queue, ());
        let mut pool = SlotPool::new(STRIDE as usize * crate::preview::HEIGHT, shm)
            .map_err(|error| WaylandBridgeError::new(format!("drag icon pool failed: {error}")))?;
        let (buffer, canvas) = pool
            .create_buffer(
                crate::preview::WIDTH as i32,
                crate::preview::HEIGHT as i32,
                STRIDE,
                wl_shm::Format::Argb8888,
            )
            .map_err(|error| WaylandBridgeError::new(format!("drag icon failed: {error}")))?;
        canvas.copy_from_slice(&crate::preview::render(preview));
        surface.attach(Some(buffer.wl_buffer()), 0, 0);
        surface.damage_buffer(
            0,
            0,
            crate::preview::WIDTH as i32,
            crate::preview::HEIGHT as i32,
        );
        surface.commit();
        Ok(Self {
            surface,
            _pool: pool,
            _buffer: buffer,
        })
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for BridgeState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: <wl_registry::WlRegistry as Proxy>::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlCompositor, ()> for BridgeState {
    fn event(
        _state: &mut Self,
        _proxy: &WlCompositor,
        _event: <WlCompositor as Proxy>::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSurface, ()> for BridgeState {
    fn event(
        _state: &mut Self,
        _proxy: &WlSurface,
        _event: <WlSurface as Proxy>::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }
}

impl SeatHandler for BridgeState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _connection: &Connection, _queue: &QueueHandle<Self>, _seat: WlSeat) {}

    fn new_capability(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _seat: WlSeat,
        _capability: Capability,
    ) {
    }

    fn remove_capability(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _seat: WlSeat,
        _capability: Capability,
    ) {
    }

    fn remove_seat(&mut self, _connection: &Connection, _queue: &QueueHandle<Self>, _seat: WlSeat) {
    }
}

impl ShmHandler for BridgeState {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl DataDeviceHandler for BridgeState {
    fn enter(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _device: &WlDataDevice,
        _x: f64,
        _y: f64,
        _surface: &WlSurface,
    ) {
    }

    fn leave(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _device: &WlDataDevice,
    ) {
    }

    fn motion(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _device: &WlDataDevice,
        _x: f64,
        _y: f64,
    ) {
    }

    fn selection(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _device: &WlDataDevice,
    ) {
    }

    fn drop_performed(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _device: &WlDataDevice,
    ) {
    }
}

impl DataOfferHandler for BridgeState {
    fn source_actions(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        offer: &mut DragOffer,
        actions: DndAction,
    ) {
        offer.set_actions(actions, DndAction::Copy);
    }

    fn selected_action(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _offer: &mut DragOffer,
        _actions: DndAction,
    ) {
    }
}

impl DataSourceHandler for BridgeState {
    fn accept_mime(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _source: &WlDataSource,
        _mime: Option<String>,
    ) {
    }

    fn send_request(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        source: &WlDataSource,
        mime: String,
        pipe: WritePipe,
    ) {
        if self.owns(source) {
            self.send_payload(&mime, pipe);
        }
    }

    fn cancelled(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        source: &WlDataSource,
    ) {
        if self.owns(source) {
            self.finish(LinuxOutcome::Cancelled);
        }
    }

    fn dnd_dropped(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        source: &WlDataSource,
    ) {
        if self.owns(source) {
            self.reporter.drop_performed();
            self.sync_transfer_ready();
        }
    }

    fn dnd_finished(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        source: &WlDataSource,
    ) {
        if self.owns(source) {
            self.finish(LinuxOutcome::Exported);
        }
    }

    fn action(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _source: &WlDataSource,
        _action: DndAction,
    ) {
    }
}

smithay_client_toolkit::delegate_dispatch2!(BridgeState);
