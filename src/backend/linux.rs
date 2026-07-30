use std::error::Error;
use std::fmt;

mod mime;

use crate::{
    DragOrigin, FileDragPayloadData, FileSet, LinuxOutcome, LinuxRejector, SessionReporter,
};
use mime::MimeTargets;
use raw_window_handle::RawWindowHandle;
use x11rb::CURRENT_TIME;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ButtonReleaseEvent, ClientMessageEvent, ConfigureWindowAux, ConnectionExt,
    CreateGCAux, CreateWindowAux, EventMask, Gcontext, GrabMode, GrabStatus, ImageFormat, PropMode,
    SelectionNotifyEvent, SelectionRequestEvent, StackMode, Window as XWindow, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;

const XDND_VERSION: u32 = 5;
const STATUS_ACCEPT: u32 = 1;

/// Authoritative pointer state from the X11 event that initiated a drag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct X11PointerEvent {
    /// Root window containing the pointer.
    pub root: XWindow,
    /// Pointer position along the root window's horizontal axis.
    pub root_x: i16,
    /// Pointer position along the root window's vertical axis.
    pub root_y: i16,
    /// Server timestamp from the initiating pointer event.
    pub time: u32,
}

/// Whether an event-scoped X11 drag still owns a live session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum X11SessionStatus {
    /// The session still owns an active native drag.
    Active,
    /// The session reached a terminal native event.
    Finished,
}

/// Failure while starting or driving an event-scoped X11 session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct X11SessionError {
    message: Box<str>,
}

impl X11SessionError {
    pub(crate) fn new(message: impl Into<Box<str>>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for X11SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for X11SessionError {}

/// Failure to start an event-scoped X11 session.
pub type X11StartError = X11SessionError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct XdndTarget {
    logical: XWindow,
    recipient: XWindow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TargetAcceptance {
    Unknown,
    Accepted(XWindow),
    Rejected(XWindow),
}

struct XdndAtoms {
    xdnd_selection: Atom,
    xdnd_enter: Atom,
    xdnd_leave: Atom,
    xdnd_position: Atom,
    xdnd_status: Atom,
    xdnd_drop: Atom,
    xdnd_finished: Atom,
    xdnd_aware: Atom,
    xdnd_proxy: Atom,
    xdnd_type_list: Atom,
    xdnd_action_copy: Atom,
    text_uri_list: Atom,
    text_uri_list_utf8: Atom,
    text_x_uri: Atom,
    application_x_kde4_urilist: Atom,
    x_special_gnome_copied_files: Atom,
    text_plain: Atom,
    text_plain_utf8: Atom,
    targets: Atom,
    utf8_string: Atom,
    string: Atom,
}

impl XdndAtoms {
    fn new<C: Connection>(conn: &C) -> Result<Self, X11SessionError> {
        Ok(Self {
            xdnd_selection: atom(conn, b"XdndSelection")?,
            xdnd_enter: atom(conn, b"XdndEnter")?,
            xdnd_leave: atom(conn, b"XdndLeave")?,
            xdnd_position: atom(conn, b"XdndPosition")?,
            xdnd_status: atom(conn, b"XdndStatus")?,
            xdnd_drop: atom(conn, b"XdndDrop")?,
            xdnd_finished: atom(conn, b"XdndFinished")?,
            xdnd_aware: atom(conn, b"XdndAware")?,
            xdnd_proxy: atom(conn, b"XdndProxy")?,
            xdnd_type_list: atom(conn, b"XdndTypeList")?,
            xdnd_action_copy: atom(conn, b"XdndActionCopy")?,
            text_uri_list: atom(conn, b"text/uri-list")?,
            text_uri_list_utf8: atom(conn, b"text/uri-list;charset=utf-8")?,
            text_x_uri: atom(conn, b"text/x-uri")?,
            application_x_kde4_urilist: atom(conn, b"application/x-kde4-urilist")?,
            x_special_gnome_copied_files: atom(conn, b"x-special/gnome-copied-files")?,
            text_plain: atom(conn, b"text/plain")?,
            text_plain_utf8: atom(conn, b"text/plain;charset=utf-8")?,
            targets: atom(conn, b"TARGETS")?,
            utf8_string: atom(conn, b"UTF8_STRING")?,
            string: AtomEnum::STRING.into(),
        })
    }

    fn mime_targets(&self) -> MimeTargets {
        MimeTargets {
            text_uri_list: self.text_uri_list,
            text_uri_list_utf8: self.text_uri_list_utf8,
            text_x_uri: self.text_x_uri,
            kde_uri_list: self.application_x_kde4_urilist,
            gnome_copied_files: self.x_special_gnome_copied_files,
            text_plain: self.text_plain,
            text_plain_utf8: self.text_plain_utf8,
            utf8_string: self.utf8_string,
            string: self.string,
        }
    }
}

struct PreviewWindow {
    window: XWindow,
    gc: Gcontext,
    depth: u8,
    pixels: Vec<u8>,
}

impl PreviewWindow {
    const OFFSET_X: i32 = 20;
    const OFFSET_Y: i32 = 22;

    fn new<C: Connection>(
        conn: &C,
        root: XWindow,
        root_x: i16,
        root_y: i16,
        preview: &crate::DragPreview,
    ) -> Result<Self, X11SessionError> {
        let screen = conn
            .setup()
            .roots
            .iter()
            .find(|screen| screen.root == root)
            .ok_or_else(|| X11SessionError::new("X11 drag root has no matching screen"))?;
        let window = conn.generate_id().map_err(x11_error)?;
        conn.create_window(
            screen.root_depth,
            window,
            root,
            -10_000,
            -10_000,
            crate::preview::WIDTH as u16,
            crate::preview::HEIGHT as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            screen.root_visual,
            &CreateWindowAux::new()
                .override_redirect(1)
                .background_pixel(0)
                .event_mask(EventMask::EXPOSURE),
        )
        .map_err(x11_error)?;
        let gc = conn.generate_id().map_err(x11_error)?;
        if let Err(error) = conn.create_gc(gc, window, &CreateGCAux::new()) {
            let _ = conn.destroy_window(window);
            return Err(x11_error(error));
        }
        let preview = Self {
            window,
            gc,
            depth: screen.root_depth,
            pixels: crate::preview::render(preview),
        };
        if let Err(error) = conn.map_window(window).map_err(x11_error) {
            preview.destroy(conn);
            return Err(error);
        }
        if let Err(error) = preview.move_to(conn, root_x, root_y) {
            preview.destroy(conn);
            return Err(error);
        }
        Ok(preview)
    }

    fn move_to<C: Connection>(
        &self,
        conn: &C,
        root_x: i16,
        root_y: i16,
    ) -> Result<(), X11SessionError> {
        conn.configure_window(
            self.window,
            &ConfigureWindowAux::new()
                .x(i32::from(root_x) + Self::OFFSET_X)
                .y(i32::from(root_y) + Self::OFFSET_Y)
                .stack_mode(StackMode::ABOVE),
        )
        .map_err(x11_error)?;
        self.draw(conn)
    }

    fn draw<C: Connection>(&self, conn: &C) -> Result<(), X11SessionError> {
        conn.put_image(
            ImageFormat::Z_PIXMAP,
            self.window,
            self.gc,
            crate::preview::WIDTH as u16,
            crate::preview::HEIGHT as u16,
            0,
            0,
            0,
            self.depth,
            &self.pixels,
        )
        .map_err(x11_error)?;
        conn.flush().map_err(x11_error)
    }

    fn destroy<C: Connection>(self, conn: &C) {
        let _ = conn.free_gc(self.gc);
        let _ = conn.destroy_window(self.window);
    }
}

/// XDND state owned by the toolkit event loop that owns the X11 connection.
#[must_use = "the toolkit must drive this session or explicitly cancel it before teardown"]
pub struct X11Session {
    atoms: XdndAtoms,
    root: XWindow,
    source_window: XWindow,
    file_payload: FileDragPayloadData,
    current_target: Option<XdndTarget>,
    target_acceptance: TargetAcceptance,
    released_target: Option<XdndTarget>,
    drop_target: Option<XdndTarget>,
    payload_target: Option<XWindow>,
    transfer_complete: bool,
    pointer_grabbed: bool,
    preview: Option<PreviewWindow>,
    last_event_time: u32,
    reporter: SessionReporter,
    finished: bool,
}

impl fmt::Debug for X11Session {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("X11Session")
            .field("source_window", &self.source_window)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl X11Session {
    pub(crate) fn start<C: Connection>(
        conn: &C,
        origin: DragOrigin<'_>,
        files: FileSet,
        reporter: SessionReporter,
        press: X11PointerEvent,
    ) -> Result<Self, X11StartError> {
        let source_window = match origin.window_handle().as_raw() {
            RawWindowHandle::Xlib(handle) if handle.window != 0 => handle.window as XWindow,
            RawWindowHandle::Xcb(handle) => handle.window.get(),
            _ => {
                return Err(X11SessionError::new(
                    "event-scoped XDND requires an X11 or XWayland origin",
                ));
            }
        };
        if press.root == x11rb::NONE || press.time == CURRENT_TIME {
            return Err(X11SessionError::new(
                "event-scoped XDND requires the initiating X11 event root and timestamp",
            ));
        }

        let (paths, preview) = files.into_parts();
        let atoms = XdndAtoms::new(conn)?;
        conn.set_selection_owner(source_window, atoms.xdnd_selection, press.time)
            .map_err(x11_error)?;
        let owner = match conn
            .get_selection_owner(atoms.xdnd_selection)
            .map_err(x11_error)
            .and_then(|cookie| cookie.reply().map_err(x11_error))
        {
            Ok(reply) => reply.owner,
            Err(error) => {
                let _ = conn.set_selection_owner(x11rb::NONE, atoms.xdnd_selection, press.time);
                let _ = conn.flush();
                return Err(error);
            }
        };
        if owner != source_window {
            return Err(X11SessionError::new("could not own XdndSelection"));
        }
        let grab_status = match conn
            .grab_pointer(
                true,
                source_window,
                EventMask::POINTER_MOTION | EventMask::BUTTON_RELEASE,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                x11rb::NONE,
                x11rb::NONE,
                press.time,
            )
            .map_err(x11_error)
            .and_then(|cookie| cookie.reply().map_err(x11_error))
        {
            Ok(reply) => reply.status,
            Err(error) => {
                let _ = conn.set_selection_owner(x11rb::NONE, atoms.xdnd_selection, press.time);
                let _ = conn.flush();
                return Err(error);
            }
        };
        if grab_status != GrabStatus::SUCCESS {
            let _ = conn.set_selection_owner(x11rb::NONE, atoms.xdnd_selection, press.time);
            let _ = conn.flush();
            return Err(X11SessionError::new(format!(
                "could not capture the drag pointer: {grab_status:?}"
            )));
        }

        let preview = preview.and_then(|preview| {
            PreviewWindow::new(conn, press.root, press.root_x, press.root_y, &preview).ok()
        });
        let mut session = Self {
            atoms,
            root: press.root,
            source_window,
            file_payload: FileDragPayloadData::from_validated(paths),
            current_target: None,
            target_acceptance: TargetAcceptance::Unknown,
            released_target: None,
            drop_target: None,
            payload_target: None,
            transfer_complete: false,
            pointer_grabbed: true,
            preview,
            last_event_time: press.time,
            reporter,
            finished: false,
        };
        if let Err(error) = session.update_target(conn, press.root_x, press.root_y) {
            session.release_pointer(conn);
            if let Some(preview) = session.preview.take() {
                preview.destroy(conn);
            }
            let _ = conn.set_selection_owner(x11rb::NONE, session.atoms.xdnd_selection, press.time);
            let _ = conn.flush();
            return Err(error);
        }
        if let Err(error) = conn.flush().map_err(x11_error) {
            session.release_pointer(conn);
            if let Some(preview) = session.preview.take() {
                preview.destroy(conn);
            }
            let _ = conn.set_selection_owner(x11rb::NONE, session.atoms.xdnd_selection, press.time);
            let _ = conn.flush();
            return Err(error);
        }
        Ok(session)
    }

    /// Drive the session from one event taken from the owning X11 queue.
    pub fn handle_event<C: Connection>(
        &mut self,
        conn: &C,
        event: &Event,
    ) -> Result<X11SessionStatus, X11SessionError> {
        if self.finished {
            return Ok(X11SessionStatus::Finished);
        }

        if let Err(error) = self.drive_event(conn, event) {
            self.release_pointer(conn);
            if let Some(preview) = self.preview.take() {
                preview.destroy(conn);
            }
            let _ = self.leave_current_target(conn);
            let _ = conn.set_selection_owner(
                x11rb::NONE,
                self.atoms.xdnd_selection,
                self.last_event_time,
            );
            let _ = conn.flush();
            self.finished = true;
            self.reporter
                .finish_linux(LinuxOutcome::Failed(crate::LinuxFailure::Setup));
            return Err(error);
        }

        Ok(if self.finished {
            X11SessionStatus::Finished
        } else {
            X11SessionStatus::Active
        })
    }

    /// Whether protocol evidence already completed the user-visible transfer.
    ///
    /// The session remains alive after this point so targets may request
    /// additional representations before sending `XdndFinished`.
    #[must_use]
    pub const fn transfer_complete(&self) -> bool {
        self.transfer_complete
    }

    fn drive_event<C: Connection>(
        &mut self,
        conn: &C,
        event: &Event,
    ) -> Result<(), X11SessionError> {
        match event {
            Event::MotionNotify(event)
                if event.root == self.root && self.released_target.is_none() =>
            {
                self.note_event_time(event.time);
                self.update_target(conn, event.root_x, event.root_y)?;
                self.move_preview(conn, event.root_x, event.root_y);
            }
            Event::ButtonRelease(event)
                if event.detail == 1
                    && event.root == self.root
                    && self.released_target.is_none() =>
            {
                self.handle_release(conn, event)?;
            }
            Event::ClientMessage(event) if event.type_ == self.atoms.xdnd_status => {
                self.handle_status(conn, event)?;
            }
            Event::ClientMessage(event) if event.type_ == self.atoms.xdnd_finished => {
                self.handle_finished(conn, event)?;
            }
            Event::SelectionRequest(event) => {
                self.handle_selection_request(conn, event)?;
            }
            Event::Expose(event)
                if self
                    .preview
                    .as_ref()
                    .is_some_and(|preview| preview.window == event.window) =>
            {
                self.redraw_preview(conn);
            }
            Event::SelectionClear(event) if event.selection == self.atoms.xdnd_selection => {
                self.finish(conn, LinuxOutcome::Cancelled)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Cancel from an authoritative toolkit teardown or gesture-cancel event.
    pub fn cancel<C: Connection>(mut self, conn: &C) -> Result<(), X11SessionError> {
        if !self.finished {
            self.release_pointer(conn);
            if let Some(preview) = self.preview.take() {
                preview.destroy(conn);
            }
            self.leave_current_target(conn)?;
            self.finish(conn, LinuxOutcome::Cancelled)?;
        }
        Ok(())
    }

    /// Retire a transfer-ready session when a new drag gesture supersedes it.
    ///
    /// A drop was already sent, so this deliberately does not send
    /// `XdndLeave`. The target action remains unconfirmed.
    pub fn supersede<C: Connection>(mut self, conn: &C) -> Result<(), X11SessionError> {
        if !self.finished {
            self.release_pointer(conn);
            if let Some(preview) = self.preview.take() {
                preview.destroy(conn);
            }
            conn.set_selection_owner(x11rb::NONE, self.atoms.xdnd_selection, self.last_event_time)
                .map_err(x11_error)?;
            conn.flush().map_err(x11_error)?;
            self.finished = true;
            self.reporter.finish_linux(LinuxOutcome::Indeterminate);
        }
        Ok(())
    }

    fn note_event_time(&mut self, time: u32) {
        if time != CURRENT_TIME {
            self.last_event_time = time;
        }
    }

    fn release_pointer<C: Connection>(&mut self, conn: &C) {
        if self.pointer_grabbed {
            let _ = conn.ungrab_pointer(self.last_event_time);
            self.pointer_grabbed = false;
        }
    }

    fn move_preview<C: Connection>(&mut self, conn: &C, root_x: i16, root_y: i16) {
        let failed = self
            .preview
            .as_ref()
            .is_some_and(|preview| preview.move_to(conn, root_x, root_y).is_err());
        if failed && let Some(preview) = self.preview.take() {
            preview.destroy(conn);
        }
    }

    fn redraw_preview<C: Connection>(&mut self, conn: &C) {
        let failed = self
            .preview
            .as_ref()
            .is_some_and(|preview| preview.draw(conn).is_err());
        if failed && let Some(preview) = self.preview.take() {
            preview.destroy(conn);
        }
    }

    fn update_target<C: Connection>(
        &mut self,
        conn: &C,
        root_x: i16,
        root_y: i16,
    ) -> Result<(), X11SessionError> {
        let target = self.find_xdnd_target(conn, root_x, root_y)?;
        if target != self.current_target {
            self.leave_current_target(conn)?;
            self.current_target = target;
            if let Some(target) = target {
                self.send_enter(conn, target)?;
            }
        }
        if let Some(target) = self.current_target {
            self.send_position(conn, target, root_x, root_y)?;
        }
        Ok(())
    }

    fn handle_release<C: Connection>(
        &mut self,
        conn: &C,
        event: &ButtonReleaseEvent,
    ) -> Result<(), X11SessionError> {
        self.note_event_time(event.time);
        self.release_pointer(conn);
        if let Some(preview) = self.preview.take() {
            preview.destroy(conn);
        }
        let target = self.find_xdnd_target(conn, event.root_x, event.root_y)?;
        if target != self.current_target {
            self.leave_current_target(conn)?;
            self.current_target = target;
            if let Some(target) = target {
                self.send_enter(conn, target)?;
                self.send_position(conn, target, event.root_x, event.root_y)?;
            }
        }
        let Some(target) = self.current_target else {
            return self.finish(conn, LinuxOutcome::Cancelled);
        };
        self.released_target = Some(target);
        self.maybe_complete_release(conn)
    }

    fn send_drop<C: Connection>(
        &mut self,
        conn: &C,
        target: XdndTarget,
    ) -> Result<(), X11SessionError> {
        self.drop_target = Some(target);
        self.send_client_message(
            conn,
            target,
            self.atoms.xdnd_drop,
            [self.source_window, 0, self.last_event_time, 0, 0],
        )?;
        self.reporter.drop_performed();
        if self.payload_target == Some(target.logical) {
            self.mark_transfer_ready();
        }
        Ok(())
    }

    fn handle_status<C: Connection>(
        &mut self,
        conn: &C,
        event: &ClientMessageEvent,
    ) -> Result<(), X11SessionError> {
        let data = event.data.as_data32();
        let target = data[0];
        if self
            .current_target
            .is_none_or(|current| current.logical != target)
        {
            return Ok(());
        }
        let accepted = data[1] & STATUS_ACCEPT == STATUS_ACCEPT;
        self.target_acceptance = if accepted {
            TargetAcceptance::Accepted(target)
        } else {
            TargetAcceptance::Rejected(target)
        };
        self.maybe_complete_release(conn)
    }

    fn handle_finished<C: Connection>(
        &mut self,
        conn: &C,
        event: &ClientMessageEvent,
    ) -> Result<(), X11SessionError> {
        let data = event.data.as_data32();
        if self
            .drop_target
            .is_none_or(|target| target.logical != data[0])
        {
            return Ok(());
        }
        let outcome = if data[1] & 1 == 1 && data[2] == self.atoms.xdnd_action_copy {
            LinuxOutcome::Exported
        } else {
            LinuxOutcome::Rejected(LinuxRejector::Target)
        };
        self.finish(conn, outcome)
    }

    fn handle_selection_request<C: Connection>(
        &mut self,
        conn: &C,
        event: &SelectionRequestEvent,
    ) -> Result<(), X11SessionError> {
        if event.selection != self.atoms.xdnd_selection || event.owner != self.source_window {
            return Ok(());
        }
        let property = if event.property == AtomEnum::NONE.into() {
            event.target
        } else {
            event.property
        };

        if event.target == self.atoms.targets {
            let mut targets = vec![self.atoms.targets];
            targets.extend(self.offered_targets());
            conn.change_property32(
                PropMode::REPLACE,
                event.requestor,
                property,
                AtomEnum::ATOM,
                &targets,
            )
            .map_err(x11_error)?;
            self.send_selection_notify(conn, event, property)?;
        } else if self.is_payload_target(event.target) {
            self.reporter.data_requested();
            self.payload_target = self.current_target.map(|target| target.logical);
            let payload = if event.target == self.atoms.x_special_gnome_copied_files {
                self.file_payload.gnome_copied_files()
            } else if event.target == self.atoms.text_plain_utf8
                || event.target == self.atoms.text_plain
                || event.target == self.atoms.utf8_string
                || event.target == self.atoms.string
            {
                self.file_payload.plain_file_list()
            } else {
                self.file_payload.uri_list()
            };
            conn.change_property8(
                PropMode::REPLACE,
                event.requestor,
                property,
                event.target,
                payload,
            )
            .map_err(x11_error)?;
            self.send_selection_notify(conn, event, property)?;
        } else {
            self.send_selection_notify(conn, event, AtomEnum::NONE.into())?;
        }
        conn.flush().map_err(x11_error)?;
        if self
            .drop_target
            .is_some_and(|target| self.payload_target == Some(target.logical))
        {
            self.mark_transfer_ready();
        }
        Ok(())
    }

    fn maybe_complete_release<C: Connection>(&mut self, conn: &C) -> Result<(), X11SessionError> {
        let Some(target) = self.released_target else {
            return Ok(());
        };
        if self.drop_target.is_some() {
            return Ok(());
        }
        match self.target_acceptance {
            TargetAcceptance::Rejected(logical) if logical == target.logical => {
                self.leave_current_target(conn)?;
                self.finish(conn, LinuxOutcome::Rejected(LinuxRejector::Target))
            }
            TargetAcceptance::Accepted(logical) if logical == target.logical => {
                self.send_drop(conn, target)
            }
            TargetAcceptance::Unknown
            | TargetAcceptance::Accepted(_)
            | TargetAcceptance::Rejected(_) => Ok(()),
        }
    }

    fn send_selection_notify<C: Connection>(
        &self,
        conn: &C,
        request: &SelectionRequestEvent,
        property: Atom,
    ) -> Result<(), X11SessionError> {
        let event = SelectionNotifyEvent {
            response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property,
        };
        conn.send_event(false, request.requestor, EventMask::NO_EVENT, event)
            .map(|_| ())
            .map_err(x11_error)
    }

    fn find_xdnd_target<C: Connection>(
        &self,
        conn: &C,
        root_x: i16,
        root_y: i16,
    ) -> Result<Option<XdndTarget>, X11SessionError> {
        let mut window = self.window_at(conn, self.root, root_x, root_y)?;
        while let Some(candidate) = window {
            if candidate == self.source_window {
                return Ok(None);
            }
            if self.is_xdnd_aware(conn, candidate)? {
                return self.target(conn, candidate).map(Some);
            }
            window = self.parent_of(conn, candidate)?;
        }
        Ok(None)
    }

    fn window_at<C: Connection>(
        &self,
        conn: &C,
        window: XWindow,
        root_x: i16,
        root_y: i16,
    ) -> Result<Option<XWindow>, X11SessionError> {
        let mut current = window;
        loop {
            let tree = conn
                .query_tree(current)
                .map_err(x11_error)?
                .reply()
                .map_err(x11_error)?;
            let child = tree.children.iter().rev().copied().find(|&candidate| {
                self.preview
                    .as_ref()
                    .is_none_or(|preview| candidate != preview.window)
                    && self.window_contains_root_point(conn, candidate, root_x, root_y)
            });
            let Some(child) = child else {
                return Ok((current != self.source_window).then_some(current));
            };
            current = child;
        }
    }

    fn window_contains_root_point<C: Connection>(
        &self,
        conn: &C,
        window: XWindow,
        root_x: i16,
        root_y: i16,
    ) -> bool {
        conn.get_geometry(window)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .and_then(|geometry| {
                conn.translate_coordinates(self.root, window, root_x, root_y)
                    .ok()
                    .and_then(|cookie| cookie.reply().ok())
                    .map(|point| {
                        point.dst_x >= 0
                            && point.dst_y >= 0
                            && i32::from(point.dst_x) < i32::from(geometry.width)
                            && i32::from(point.dst_y) < i32::from(geometry.height)
                    })
            })
            .unwrap_or(false)
    }

    fn parent_of<C: Connection>(
        &self,
        conn: &C,
        window: XWindow,
    ) -> Result<Option<XWindow>, X11SessionError> {
        let tree = conn
            .query_tree(window)
            .map_err(x11_error)?
            .reply()
            .map_err(x11_error)?;
        Ok((tree.parent != x11rb::NONE).then_some(tree.parent))
    }

    fn is_xdnd_aware<C: Connection>(
        &self,
        conn: &C,
        window: XWindow,
    ) -> Result<bool, X11SessionError> {
        let property = conn
            .get_property(false, window, self.atoms.xdnd_aware, AtomEnum::ANY, 0, 1)
            .map_err(x11_error)?
            .reply()
            .map_err(x11_error)?;
        Ok(property.value_len > 0)
    }

    fn target<C: Connection>(
        &self,
        conn: &C,
        logical: XWindow,
    ) -> Result<XdndTarget, X11SessionError> {
        let recipient = match self.xdnd_proxy(conn, logical)? {
            Some(proxy) if self.xdnd_proxy(conn, proxy)? == Some(proxy) => proxy,
            _ => logical,
        };
        Ok(XdndTarget { logical, recipient })
    }

    fn xdnd_proxy<C: Connection>(
        &self,
        conn: &C,
        window: XWindow,
    ) -> Result<Option<XWindow>, X11SessionError> {
        let property = conn
            .get_property(false, window, self.atoms.xdnd_proxy, AtomEnum::WINDOW, 0, 1)
            .map_err(x11_error)?
            .reply()
            .map_err(x11_error)?;
        Ok(property.value32().and_then(|mut values| values.next()))
    }

    fn send_enter<C: Connection>(
        &self,
        conn: &C,
        target: XdndTarget,
    ) -> Result<(), X11SessionError> {
        let targets = self.offered_targets();
        let enter = self.atoms.mime_targets().enter_targets(&targets);
        conn.change_property32(
            PropMode::REPLACE,
            self.source_window,
            self.atoms.xdnd_type_list,
            AtomEnum::ATOM,
            &targets,
        )
        .map_err(x11_error)?;
        self.send_client_message(
            conn,
            target,
            self.atoms.xdnd_enter,
            [
                self.source_window,
                (XDND_VERSION << 24) | 1,
                enter[0],
                enter[1],
                enter[2],
            ],
        )
    }

    fn send_position<C: Connection>(
        &mut self,
        conn: &C,
        target: XdndTarget,
        root_x: i16,
        root_y: i16,
    ) -> Result<(), X11SessionError> {
        let xy = ((root_x as u32) << 16) | u32::from(root_y as u16);
        self.send_client_message(
            conn,
            target,
            self.atoms.xdnd_position,
            [
                self.source_window,
                0,
                xy,
                self.last_event_time,
                self.atoms.xdnd_action_copy,
            ],
        )
    }

    fn leave_current_target<C: Connection>(&mut self, conn: &C) -> Result<(), X11SessionError> {
        if let Some(target) = self.current_target.take() {
            self.send_client_message(
                conn,
                target,
                self.atoms.xdnd_leave,
                [self.source_window, 0, 0, 0, 0],
            )?;
        }
        self.target_acceptance = TargetAcceptance::Unknown;
        self.payload_target = None;
        Ok(())
    }

    fn send_client_message<C: Connection>(
        &self,
        conn: &C,
        target: XdndTarget,
        message_type: Atom,
        data: [u32; 5],
    ) -> Result<(), X11SessionError> {
        let event = ClientMessageEvent::new(32, target.logical, message_type, data);
        conn.send_event(false, target.recipient, EventMask::NO_EVENT, event)
            .map_err(x11_error)?;
        conn.flush().map_err(x11_error)
    }

    fn offered_targets(&self) -> Vec<Atom> {
        self.atoms.mime_targets().offered_targets()
    }

    fn is_payload_target(&self, target: Atom) -> bool {
        self.offered_targets().contains(&target)
    }

    fn mark_transfer_ready(&mut self) {
        if self.transfer_complete {
            return;
        }
        self.transfer_complete = true;
        self.reporter.transfer_ready();
    }

    fn finish<C: Connection>(
        &mut self,
        conn: &C,
        outcome: LinuxOutcome,
    ) -> Result<(), X11SessionError> {
        self.release_pointer(conn);
        if let Some(preview) = self.preview.take() {
            preview.destroy(conn);
        }
        conn.set_selection_owner(x11rb::NONE, self.atoms.xdnd_selection, self.last_event_time)
            .map_err(x11_error)?;
        conn.flush().map_err(x11_error)?;
        self.finished = true;
        self.reporter.finish_linux(outcome);
        Ok(())
    }
}

impl Drop for X11Session {
    fn drop(&mut self) {
        if !self.finished {
            self.reporter.finish_linux(LinuxOutcome::Cancelled);
        }
    }
}

fn atom<C: Connection>(conn: &C, name: &[u8]) -> Result<Atom, X11SessionError> {
    conn.intern_atom(false, name)
        .map_err(x11_error)?
        .reply()
        .map(|reply| reply.atom)
        .map_err(x11_error)
}

fn x11_error(error: impl fmt::Display) -> X11SessionError {
    X11SessionError::new(error.to_string())
}

#[cfg(test)]
mod bridge_handoff_tests {
    use super::{
        BRIDGE_HANDOFF_STREAK, BridgeHandoffDecision, DragCoordinateMapper, HyprMonitor,
        PreviewMonitor, bridge_hover_streak_after_sample, evaluate_bridge_handoff,
        is_drag_origin_window,
    };

    #[test]
    fn bridge_streak_increments_after_leaving_origin() {
        let first = bridge_hover_streak_after_sample(0, true, true, true, false, false, true);
        assert_eq!(first, 1);
        let second = bridge_hover_streak_after_sample(first, true, true, true, false, false, true);
        assert_eq!(second, BRIDGE_HANDOFF_STREAK);
        assert_eq!(
            evaluate_bridge_handoff(second, true, true, true, false, true, false),
            BridgeHandoffDecision::Handoff
        );
    }

    #[test]
    fn bridge_streak_resets_off_bridge_or_release() {
        assert_eq!(
            bridge_hover_streak_after_sample(1, true, false, true, false, false, true),
            0
        );
        assert_eq!(
            bridge_hover_streak_after_sample(1, true, true, false, false, false, true),
            0
        );
        assert_eq!(
            bridge_hover_streak_after_sample(1, false, true, true, false, false, true),
            0
        );
        assert_eq!(
            evaluate_bridge_handoff(1, true, true, true, false, true, false),
            BridgeHandoffDecision::SuppressedStreakInsufficient
        );
    }

    #[test]
    fn bridge_handoff_suppressed_over_origin() {
        assert_eq!(
            bridge_hover_streak_after_sample(0, true, true, true, true, false, false),
            0
        );
        assert_eq!(
            evaluate_bridge_handoff(BRIDGE_HANDOFF_STREAK, true, true, true, true, false, false),
            BridgeHandoffDecision::SuppressedStillOverOrigin
        );
    }

    #[test]
    fn bridge_handoff_suppressed_before_leaving_origin() {
        assert_eq!(
            bridge_hover_streak_after_sample(0, true, true, true, false, false, false),
            0
        );
        assert_eq!(
            evaluate_bridge_handoff(BRIDGE_HANDOFF_STREAK, true, true, true, false, false, false),
            BridgeHandoffDecision::SuppressedNotLeftOrigin
        );
    }

    #[test]
    fn bridge_handoff_suppressed_on_real_x11_target() {
        assert_eq!(
            bridge_hover_streak_after_sample(1, true, false, true, false, true, true),
            0
        );
        assert_eq!(
            evaluate_bridge_handoff(BRIDGE_HANDOFF_STREAK, true, false, true, false, true, true),
            BridgeHandoffDecision::SuppressedRealXdndTarget
        );
    }

    #[test]
    fn sibling_plugin_window_is_not_the_origin_window() {
        assert!(is_drag_origin_window(Some(0x100), 0x100));
        assert!(!is_drag_origin_window(Some(0x100), 0x200));
        assert!(!is_drag_origin_window(None, 0x200));
    }

    #[test]
    fn coordinate_mapper_keeps_single_scale_monitor_identity() {
        let mapper = DragCoordinateMapper::new(
            vec![PreviewMonitor {
                name: "DP-1".to_string(),
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            }],
            vec![HyprMonitor {
                name: "DP-1".to_string(),
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
                scale: 1.0,
                x11_x: 0,
                x11_y: 0,
            }],
            1.0,
        );

        assert_eq!(mapper.map_point(640, 480), (640, 480));
        assert_eq!(mapper.target_point(640, 480), (640, 480));
    }

    #[test]
    fn coordinate_mapper_scales_primary_logical_point_to_x11() {
        let mapper = DragCoordinateMapper::new(
            vec![PreviewMonitor {
                name: "DP-3".to_string(),
                x: 0,
                y: 0,
                width: 3840,
                height: 2160,
            }],
            vec![HyprMonitor {
                name: "DP-3".to_string(),
                x: 0,
                y: 0,
                width: 3840,
                height: 2160,
                scale: 1.5,
                x11_x: 0,
                x11_y: 0,
            }],
            1.0,
        );

        assert_eq!(mapper.map_point(1280, 720), (1920, 1080));
        // XDND / preview consumers must keep QueryPointer coords as-is.
        assert_eq!(mapper.target_point(1280, 720), (1280, 720));
    }

    #[test]
    fn coordinate_mapper_uses_secondary_monitor_origin_and_scale() {
        let mapper = DragCoordinateMapper::new(
            vec![
                PreviewMonitor {
                    name: "DP-3".to_string(),
                    x: 0,
                    y: 0,
                    width: 3840,
                    height: 2160,
                },
                PreviewMonitor {
                    name: "HDMI-A-1".to_string(),
                    x: 3840,
                    y: 0,
                    width: 2560,
                    height: 1440,
                },
            ],
            vec![
                HyprMonitor {
                    name: "DP-3".to_string(),
                    x: 0,
                    y: 0,
                    width: 3840,
                    height: 2160,
                    scale: 1.5,
                    x11_x: 0,
                    x11_y: 0,
                },
                HyprMonitor {
                    name: "HDMI-A-1".to_string(),
                    x: 2560,
                    y: 0,
                    width: 2560,
                    height: 1440,
                    scale: 1.0,
                    x11_x: 3840,
                    x11_y: 0,
                },
            ],
            1.0,
        );

        assert_eq!(mapper.map_point(3000, 500), (4280, 500));
        assert_eq!(mapper.target_point(3000, 500), (3000, 500));
    }

    #[test]
    fn coordinate_mapper_keeps_unknown_compositor_identity() {
        let mapper = DragCoordinateMapper::new(
            vec![PreviewMonitor {
                name: "screen".to_string(),
                x: 0,
                y: 0,
                width: 2560,
                height: 1440,
            }],
            Vec::new(),
            1.0,
        );

        assert_eq!(mapper.map_point(2400, 700), (2400, 700));
        assert_eq!(mapper.target_point(2400, 700), (2400, 700));
    }
}
