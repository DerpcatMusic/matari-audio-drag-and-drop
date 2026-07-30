# Changelog

## 0.1.1 - Unreleased

- Make XWayland-to-Wayland bridge transfers non-blocking after accepted protocol and payload-transfer events while retaining selection ownership and final authority for the target.
- Keep event-driven X11 pointer authority and accepted-target evidence through button release so crossing onto a native Wayland surface cannot strand the drag.
- Restore waveform, spectrogram, and MIDI source previews for event-driven X11/XWayland drags.
- Add a reusable native-Wayland lifecycle reporter that becomes replaceable only after both payload delivery and a compositor drop event.
- Coalesce repeated native progress notifications into one data-request and one drop-performed event per drag.
- Free Windows transfer allocations when native memory locking fails.
- Document native unsafe invariants and the public X11 event lifecycle.
- Enforce the declared Rust 1.95 MSRV, dependency policy, strict native unsafe
  lints, complete public documentation, and registry publication dry runs.
- Mark every native backend experimental until dated host evidence qualifies it.

## 0.1.0 - 2026-07-30

- Introduced Matari's editor-owned, typed drag-and-drop API with event-driven native lifecycle reporting.
- Fixed proxied XDND delivery and wait for the target's exact accept or reject event before sending a drop.
- X11/XWayland drag sessions now run on the editor's native event queue with no side connection or pointer polling.
- Native drag completion now wakes the editor immediately and preserves exact start-to-finish event order without polling or delay timers.
- Improved macOS drag-session cleanup after completed or cancelled drops.
