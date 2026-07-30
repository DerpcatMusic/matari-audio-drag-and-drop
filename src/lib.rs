#![doc = include_str!("../README.md")]

mod backend;
mod facade;
mod file_payload;
#[cfg(not(all(target_family = "unix", not(target_os = "macos"))))]
mod platform;

pub use file_payload::FileDragOffer;
pub(crate) use file_payload::FileDragPayloadData;

#[cfg(all(target_family = "unix", not(target_os = "macos")))]
pub use backend::{X11PointerEvent, X11Session, X11SessionError, X11SessionStatus, X11StartError};

pub use facade::{
    Controller, DragOrigin, FailureKind, FailureStage, FileSet, FileSetError, Inbound,
    InboundDecision, InboundDisposition, InboundError, InboundHandler, InboundOffer, LinuxFailure,
    LinuxOutcome, LinuxRejector, NativeProtocol, NativeReporter, NativeRuntime, NativeRuntimePort,
    NativeStartError, OriginError, Outbound, Outcome, ProtocolError, RejectReason, RejectedStart,
    SessionEvent, SessionFailure, SessionId, SessionReporter, SessionRoute, SourceContext,
    StartError, StartTicket, ToolkitAdapter, Update, WaylandReporter, WaylandRuntimePort,
    WaylandStartError,
};
