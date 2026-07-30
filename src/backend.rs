//! Native backend adapter contract.
//!
//! This module is the public surface GUI/toolkit adapters should implement.
//! It intentionally depends on `raw-window-handle` instead of a specific UI
//! toolkit, so baseview, winit, Vizia, Slint, or custom plugin wrappers can all
//! feed the same drag protocol.

use raw_window_handle::RawWindowHandle;
#[cfg(not(all(target_family = "unix", not(target_os = "macos"))))]
use raw_window_handle::WindowHandle;

#[cfg(not(all(target_family = "unix", not(target_os = "macos"))))]
pub(crate) struct ExternalDragPayload {
    pub(crate) paths: Vec<std::path::PathBuf>,
}
#[cfg(not(all(target_family = "unix", not(target_os = "macos"))))]
use crate::platform::DragBackendKind;
use crate::{DragOrigin, FileSet, NativeStartError, SessionReporter, SessionRoute};
#[cfg(not(all(target_family = "unix", not(target_os = "macos"))))]
use crate::{NativeProtocol, SourceContext};

#[cfg(all(target_family = "unix", not(target_os = "macos")))]
mod linux;
#[cfg(all(target_family = "unix", not(target_os = "macos")))]
pub use linux::{X11PointerEvent, X11Session, X11SessionError, X11SessionStatus, X11StartError};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// Native window context required by platform drag launchers.
#[derive(Clone, Copy, Debug)]
#[cfg(not(all(target_family = "unix", not(target_os = "macos"))))]
pub struct DragWindow<'a> {
    window: WindowHandle<'a>,
}

#[cfg(not(all(target_family = "unix", not(target_os = "macos"))))]
impl<'a> DragWindow<'a> {
    /// Build a drag window context from borrowed handles.
    #[must_use]
    pub const fn new(window: WindowHandle<'a>) -> Self {
        Self { window }
    }

    /// Borrowed window handle.
    #[must_use]
    pub const fn window(&self) -> WindowHandle<'a> {
        self.window
    }

    /// Backend kind represented by the window handle.
    #[must_use]
    pub fn backend_kind(&self) -> DragBackendKind {
        match self.window.as_raw() {
            RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_) => DragBackendKind::X11Xdnd,
            RawWindowHandle::Wayland(_) => DragBackendKind::WaylandDataDevice,
            RawWindowHandle::AppKit(_) => DragBackendKind::MacosAppKit,
            RawWindowHandle::Win32(_) | RawWindowHandle::WinRt(_) => DragBackendKind::WindowsOle,
            _ => DragBackendKind::Unsupported,
        }
    }
}

/// Error returned by a native drag backend.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(not(all(target_family = "unix", not(target_os = "macos"))))]
pub enum ExternalDragError {
    EmptyPayload,
    UnsupportedBackend {
        backend: DragBackendKind,
        window: String,
    },
    #[cfg(all(target_family = "unix", not(target_os = "macos")))]
    MissingWindowHandle(&'static str),
    #[cfg(all(target_family = "unix", not(target_os = "macos")))]
    BackendUnavailable(String),
    StartFailed(String),
}

#[cfg(not(all(target_family = "unix", not(target_os = "macos"))))]
impl std::fmt::Display for ExternalDragError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPayload => formatter.write_str("no files to drag"),
            Self::UnsupportedBackend { backend, window } => {
                write!(
                    formatter,
                    "external file drag is not implemented for {} from {window}",
                    backend.summary()
                )
            }
            #[cfg(all(target_family = "unix", not(target_os = "macos")))]
            Self::MissingWindowHandle(message) => formatter.write_str(message),
            #[cfg(all(target_family = "unix", not(target_os = "macos")))]
            Self::BackendUnavailable(message) => formatter.write_str(message),
            Self::StartFailed(message) => formatter.write_str(message),
        }
    }
}

#[cfg(not(all(target_family = "unix", not(target_os = "macos"))))]
impl std::error::Error for ExternalDragError {}

#[cfg(not(all(target_family = "unix", not(target_os = "macos"))))]
impl From<String> for ExternalDragError {
    fn from(message: String) -> Self {
        Self::StartFailed(message)
    }
}

#[cfg(not(all(target_family = "unix", not(target_os = "macos"))))]
pub(crate) fn start_reported_file_drag(
    origin: DragOrigin<'_>,
    files: FileSet,
    reporter: SessionReporter,
) -> Result<SessionRoute, NativeStartError> {
    let window = DragWindow::new(origin.window_handle());
    let route = match window.window().as_raw() {
        #[cfg(target_os = "windows")]
        RawWindowHandle::Win32(_) => SessionRoute {
            protocol: NativeProtocol::Ole,
            source: SourceContext::EmbeddedWin32,
        },
        #[cfg(target_os = "macos")]
        RawWindowHandle::AppKit(_) => SessionRoute {
            protocol: NativeProtocol::AppKit,
            source: SourceContext::EmbeddedAppKit,
        },
        other => {
            return Err(NativeStartError::new(format!(
                "the native runtime does not support {other:?}"
            )));
        }
    };
    let payload = ExternalDragPayload {
        paths: files.into_paths(),
    };
    platform_start_file_drag(window, payload, origin.appkit_event(), Some(reporter))
        .map_err(|error| NativeStartError::new(error.to_string()))?;
    Ok(route)
}

#[cfg(all(target_family = "unix", not(target_os = "macos")))]
pub(crate) fn start_reported_file_drag(
    origin: DragOrigin<'_>,
    _files: FileSet,
    _reporter: SessionReporter,
) -> Result<SessionRoute, NativeStartError> {
    let message = match origin.window_handle().as_raw() {
        RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_) => {
            "X11/XWayland drag requires StartTicket::start_x11 from the owning event queue"
        }
        RawWindowHandle::Wayland(_) => {
            "native Wayland drag requires StartTicket::start_wayland from the owning event queue"
        }
        _ => "the native window backend is unsupported",
    };
    Err(NativeStartError::new(message))
}

#[cfg(target_os = "windows")]
fn platform_start_file_drag(
    window: DragWindow<'_>,
    payload: ExternalDragPayload,
    _appkit_event: Option<std::ptr::NonNull<std::ffi::c_void>>,
    reporter: Option<SessionReporter>,
) -> Result<(), ExternalDragError> {
    windows::start_external_file_drag(window, payload, reporter)
}

#[cfg(target_os = "macos")]
fn platform_start_file_drag(
    window: DragWindow<'_>,
    payload: ExternalDragPayload,
    appkit_event: Option<std::ptr::NonNull<std::ffi::c_void>>,
    reporter: Option<SessionReporter>,
) -> Result<(), ExternalDragError> {
    macos::start_external_file_drag(window, payload, appkit_event, reporter)
}

#[cfg(not(any(
    all(target_family = "unix", not(target_os = "macos")),
    target_os = "windows",
    target_os = "macos"
)))]
fn platform_start_file_drag(
    window: DragWindow<'_>,
    _payload: ExternalDragPayload,
    _appkit_event: Option<std::ptr::NonNull<std::ffi::c_void>>,
    _reporter: Option<SessionReporter>,
) -> Result<(), ExternalDragError> {
    Err(ExternalDragError::UnsupportedBackend {
        backend: window.backend_kind(),
        window: format!("{:?}", window.window().as_raw()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use raw_window_handle::{RawDisplayHandle, XcbWindowHandle, XlibDisplayHandle};

    #[test]
    fn infers_xwayland_backend_from_xcb_window() {
        let mut handle = XcbWindowHandle::empty();
        handle.window = 42;
        let window = DragWindow::new(
            RawDisplayHandle::Xlib(XlibDisplayHandle::empty()),
            RawWindowHandle::Xcb(handle),
        );

        assert_eq!(window.backend_kind(), DragBackendKind::X11Xdnd);
        assert_eq!(window.source_route(), DragRoute::XwaylandToXwayland);
    }

    #[test]
    fn rejects_empty_payload_before_backend_dispatch() {
        let mut handle = XcbWindowHandle::empty();
        handle.window = 42;
        let window = DragWindow::new(
            RawDisplayHandle::Xlib(XlibDisplayHandle::empty()),
            RawWindowHandle::Xcb(handle),
        );
        let payload = ExternalDragPayload {
            id: 1,
            paths: Vec::new(),
            preview: None,
        };

        let err = start_file_drag(window, payload).expect_err("empty payload should fail");

        assert_eq!(err, ExternalDragError::EmptyPayload);
    }

    #[test]
    fn inbound_drop_event_describes_accepted_files() {
        let event = InboundDropEvent::Accepted { file_count: 2 };
        assert_eq!(event.summary(), "Inbound drop accepted: 2 files");
    }

    #[test]
    fn routed_terminal_survives_competing_bus_drain() {
        emit_backend_lifecycle_event(ExternalDragLifecycleEvent::new(
            11,
            ExternalDragLifecyclePhase::Finished,
        ));
        emit_backend_lifecycle_event(ExternalDragLifecycleEvent::new(
            22,
            ExternalDragLifecyclePhase::Cancelled,
        ));

        assert_eq!(
            drain_backend_lifecycle_events(),
            vec![
                ExternalDragLifecycleEvent::new(11, ExternalDragLifecyclePhase::Finished),
                ExternalDragLifecycleEvent::new(22, ExternalDragLifecyclePhase::Cancelled),
            ]
        );

        assert_eq!(
            take_drag_terminal(11),
            Some(ExternalDragLifecyclePhase::Finished)
        );
        assert_eq!(
            take_drag_terminal(22),
            Some(ExternalDragLifecyclePhase::Cancelled)
        );
        assert_eq!(take_drag_terminal(11), None);
    }

    #[test]
    fn take_drag_terminal_is_consume_once() {
        emit_backend_lifecycle_event(ExternalDragLifecycleEvent::new(
            5,
            ExternalDragLifecyclePhase::Failed,
        ));

        assert_eq!(
            take_drag_terminal(5),
            Some(ExternalDragLifecyclePhase::Failed)
        );
        assert_eq!(take_drag_terminal(5), None);
        assert!(!has_routed_drag_lifecycle(5));
    }

    #[test]
    fn lifecycle_bus_drains_typed_events_in_order() {
        emit_backend_lifecycle_event(ExternalDragLifecycleEvent::new(
            7,
            ExternalDragLifecyclePhase::Started,
        ));
        emit_backend_lifecycle_event(ExternalDragLifecycleEvent::new(
            7,
            ExternalDragLifecyclePhase::Finished,
        ));

        assert_eq!(
            drain_backend_lifecycle_events(),
            vec![
                ExternalDragLifecycleEvent::new(7, ExternalDragLifecyclePhase::Started),
                ExternalDragLifecycleEvent::new(7, ExternalDragLifecyclePhase::Finished),
            ]
        );
        assert!(drain_backend_lifecycle_events().is_empty());
    }
}
