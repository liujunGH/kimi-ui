# kimi-ui

A lightweight desktop shell for the official [Kimi Code](https://www.kimi.com/code/) Web UI — Tauri 2 + the system WebView (WKWebView on macOS). It turns `kimi web` into a standalone desktop app without bundling Chromium; the shell itself costs about **21MB** of memory.

> [中文说明](README.md)

## Features

- **Real app form**: own window, Dock icon, Cmd-Tab — and everything exits when the window closes
- **Native window chrome** (macOS): hidden-inset title bar with overlaid traffic lights, matching the web UI's official desktop layout; drag the window from the sidebar/chat header (double-click to zoom/restore)
- **Native notifications**: polyfills the missing WKWebView `Notification` API — completion/question/approval alerts become macOS notifications; clicking one re-focuses the window
- **Dock badge**: unread-notification count on the Dock icon, cleared when the window regains focus
- **Native status bar**: a shell-owned strip at the bottom of the window (the SPA never overlaps it) showing live context usage and model; it talks to the daemon directly over REST/WebSocket with zero DOM coupling to the official page — and it's the shell's extension surface
- **Plan quota**: the strip also shows membership quota (5-hour / weekly) with reset countdown; there is no public endpoint for it, so the shell periodically runs a headless TUI `/usage` inside an embedded pseudo-terminal (via `portable-pty` + `vt100`) and parses the rendered screen — zero external dependencies, degrades gracefully
- **Swarm panel**: the status bar's "蜂群" button expands a live subagent roster — status dots, swarm index, task description, result summary, token usage (fed by the daemon's `subagent.*` events)
- **No more chat climbing**: streaming thinking blocks are height-capped and scroll internally, so long reasoning streams stop pushing the page upward
- **Downloads**: session exports etc. land in `~/Downloads`, de-duplicated as `name (n).ext`
- **External links**: always handed to the system browser
- **Self-healing + watchdog**: fixes clipped double-digit list numbers, hides the "internal testing" badge, and warns once if an official UI update breaks the shell's DOM hooks
- **Zero babysitting**: the daemon exits by itself 60s after the last client disconnects; the shell just starts or reuses it
- **Low memory**: ~26MB main process; ~650MB total including the WebKit helpers, the SPA itself, and the tiny status-bar page (an equivalent Electron build is typically 900MB–1.2GB)

## Requirements

- [Kimi Code CLI](https://www.kimi.com/code/docs/en/) ≥ 0.26, installed and logged in (`kimi` on PATH; it provides the web UI and the token)
- macOS 13+ (Windows/Linux code paths exist but are untested)
- Rust toolchain for building

## Usage

Build and run from source:

```bash
cargo build --release
./target/release/kimi-ui
```

Package as a `.app` and install (recommended, no tauri-cli needed):

```bash
bash packaging/make-app.sh                 # build + assemble + ad-hoc sign
cp -R "Kimi Code.app" /Applications/
```

Launch at login (optional):

```bash
cp packaging/dev.kimiui.desktop.plist ~/Library/LaunchAgents/
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.kimiui.desktop.plist
```

Remove launch-at-login:

```bash
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/dev.kimiui.desktop.plist
rm ~/Library/LaunchAgents/dev.kimiui.desktop.plist
```

## How it works

The shell implements no Kimi functionality itself; it is just a launcher + a browser:

1. Runs `kimi server run` (idempotent: starts the daemon or reuses a running one)
2. Reads the daemon's address and access credential from kimi's local data directory (same location and format the official client uses)
3. Serves a **customized kimi-web bundle** (from the fork [liujunGH/kimi-code](https://github.com/liujunGH/kimi-code), branch `kimi-ui`; one-command build via `scripts/build-web.sh`) on a tiny built-in static server (127.0.0.1:58628), and hands over both the credential and the daemon origin via the URL hash — the same shape the official flow uses. Falls back to the daemon-hosted UI if web-dist is missing
4. The main webview gets an injected script only for desktop behaviors (`Notification` polyfill, `window.focus()`, drag-region mirroring, etc.); the bottom status bar is the shell's own page, talking to the daemon directly over REST/WebSocket

## Maintenance notes

Several integration points are coupled to the official web UI's DOM or protocol. An official update may degrade a feature (it degrades gracefully to stock behavior, never breaks the app):

- Drag regions: `.side.macos-desktop .ch`, `.chat-header.macos-desktop`
- Badge hiding: `.internal-build-tag`
- Thinking-block cap: `.tc-wrap:not(.is-collapsed) pre.tc`
- List-number fix: `.md ol`
- Status bar: `/api/v1/sessions/*` REST endpoints and `subagent.*` event field names (protocol-level, more stable than DOM)

DOM fixes live in `INIT_SCRIPT` in `src/main.rs` (the watchdog warns when it detects drift); the status bar code lives in `public/status.html`.

## Layout

```
src/main.rs            # all shell logic (window layout, injected script, downloads, commands)
public/index.html      # launch placeholder page
public/status.html     # native status bar (usage, model, swarm roster)
capabilities/          # Tauri window permissions (dragging + remote-origin IPC)
scripts/icon.swift     # icon generator
packaging/             # Info.plist, make-app.sh, LaunchAgent plist
icons/                 # generated icons
```

## License

[MIT](LICENSE)
