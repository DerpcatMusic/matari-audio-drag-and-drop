# Changelog

## 0.1.0 - 2026-07-30

- Introduced Matari's editor-owned, typed drag-and-drop API with event-driven native lifecycle reporting.
- Fixed proxied XDND delivery and wait for the target's exact accept or reject event before sending a drop.
- X11/XWayland drag sessions now run on the editor's native event queue with no side connection or pointer polling.
- Native drag completion now wakes the editor immediately and preserves exact start-to-finish event order without polling or delay timers.
- Improved macOS drag-session cleanup after completed or cancelled drops.
