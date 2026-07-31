# matari-audio-drag-and-drop

Typed, toolkit-neutral drag-and-drop for Rust audio plug-in editors.

Matari separates product decisions from native protocol work:

- `Controller` owns one editor's sessions and events.
- `ToolkitAdapter` connects the controller to a live GUI callback.
- `StartTicket` is non-cloneable authority to start exactly one drag.
- `NativeRuntimePort` covers OLE and AppKit runtimes.
- `X11Session` runs XDND on the toolkit-owned X11 connection, or a native
  Wayland data-device source for embedded XWayland editors on Hyprland.
- `WaylandRuntimePort` consumes an event-scoped runtime that owns the
  toolkit's one-use press token and existing Wayland queue.
- `SessionReporter` delivers native lifecycle events without polling, timers,
  or process-global buses.

The package validates file-backed payloads before they reach a native runtime.
Session IDs are direction-typed, outcomes and failures are explicit, and each
toolkit adapter reports only the routes available to its live editor.

## Integration shape

An editor owns a `Controller`, constructs a `FileSet` after its background
export completes, and passes it to `Controller::start_outbound`. Its
`ToolkitAdapter` schedules the owned `StartTicket` from the current GUI gesture.
The native runtime retains the supplied reporter and finishes the session from
real platform callbacks.

Native Wayland starts consume an event-scoped runtime that owns the toolkit's
one-use press token and existing event queue.

X11/XWayland starts consume the initiating pointer event and return an
`X11Session`. The adapter selects the route before consuming the start ticket.
XDND sessions use events forwarded from the owning X11 queue. Matari detects
the compositor bridge explicitly: KWin and Mutter use canonical XDND,
Hyprland uses its serial-less native Wayland compatibility route only when both
the live X window manager and session identify Hyprland, and unknown or
unsupported bridges remain XDND-only. A live xwayland-satellite server
overrides stale desktop labels because it does not bridge DND between X11 and
native Wayland. Neither route queries pointer state nor infers completion from
elapsed time.

Compositors without an X11-source to native-Wayland bridge cannot make an
embedded XWayland editor drop into native Wayland targets. Those sessions need
a native Wayland editor with a real toolkit press serial; XDND remains
available for X11/XWayland targets.

Linux reporters distinguish successful export, target/compositor rejection,
unsupported routes, and setup failure.

## Maturity

Matari 0.1 is experimental. Windows OLE, macOS AppKit, X11/XWayland XDND, and
native Wayland data-device integrations compile on their native targets, but
compilation is not host qualification. Treat a route as supported only after a
dated run records the operating system, host and format, window presentation,
source and target backends, payload, and exact terminal outcome.

## Realtime safety

Matari is a GUI/background facility. Do not validate files, start a native
drag, access the OS, or drain controller events from an audio callback.

## Development

```sh
cargo fmt --check
cargo check --locked
cargo clippy --locked --lib
cargo publish --dry-run --locked
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for protocol invariants and
[SECURITY.md](SECURITY.md) for private vulnerability reporting.

## License

[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE)
