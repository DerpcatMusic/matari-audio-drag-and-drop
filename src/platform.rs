//! Platform backend names and diagnostics.

/// Native backend used to start a drag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DragBackendKind {
    X11Xdnd,
    WaylandDataDevice,
    WindowsOle,
    MacosAppKit,
    Unsupported,
}

impl DragBackendKind {
    /// Human-readable backend summary.
    #[must_use]
    pub const fn summary(self) -> &'static str {
        match self {
            Self::X11Xdnd => "X11/XWayland XDND",
            Self::WaylandDataDevice => "native Wayland data-device",
            Self::WindowsOle => "Windows OLE",
            Self::MacosAppKit => "macOS AppKit",
            Self::Unsupported => "unsupported backend",
        }
    }
}
