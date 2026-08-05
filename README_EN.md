# kimi-ui

A desktop client for [Kimi Code](https://www.kimi.com/code/): **official daemon + official Web UI + native shell**. This repo owns only the window, status bar, system integrations, and cross-platform packaging. The UI is the official prebuilt web bundle committed in each Kimi Code release; no Web UI fork is maintained.

> Kimi Code 桌面端：官方 daemon + 官方 Web UI + 原生壳。[中文说明](README.md)

## Features

- **Real app form**: own window, Dock icon, Cmd-Tab, close-to-quit; hidden-inset title bar with drag regions and double-click zoom
- **Official Web UI**: consumes the prebuilt bundle from the pinned Kimi Code release, keeping UI features and protocol behavior aligned with upstream
- **Native status bar**: context usage, plan quota (5h/weekly), busy dot, live swarm roster, follow/freeze toggle, update-available badge
- **Native notifications + Dock badge**: completion/question/approval alerts as macOS notifications, unread count on the Dock icon
- **Downloads & external links**: exports land in `~/Downloads` de-duplicated; links open in the system browser
- **Self-healing + three-layer watchdog**: loud warnings (never silent breakage) when official updates drift the DOM, protocol, or scrape format
- **Low memory**: ~26MB main process, system WebView, no bundled Chromium
- **CI releases + update check**: download the .app from Releases; the app checks for new versions itself

## Install

**Recommended: download** `kimi-ui-macos-arm64.zip` from [Releases](https://github.com/liujunGH/kimi-ui/releases), unzip, and drag to `/Applications`.

The only prerequisite: [Kimi Code CLI](https://www.kimi.com/code/docs/en/) installed and logged in (it provides the daemon and credentials).

## Build from source

Requires this repo plus a `kimi-code` checkout pinned to the official release tag:

```bash
git clone https://github.com/liujunGH/kimi-ui.git
git clone --branch '@moonshot-ai/kimi-code@0.33.0' --depth 1 https://github.com/MoonshotAI/kimi-code.git

cd kimi-ui
KIMI_CODE_REPO=../kimi-code bash scripts/build-web.sh  # stage official dist-web
bash packaging/make-app.sh --install                  # build, package, and install
```

Requires a Rust toolchain; Node/pnpm is no longer needed. Set `KIMI_CODE_REPO` when the checkout is not at `~/project/kimi-code`.

## How it works

1. Requires Kimi Code CLI 0.33 or newer, attaching to a live server or launching `kimi web --no-open` on an explicitly selected free port
2. Reads the daemon's address and credential from kimi's local data directory
3. Serves the official web bundle on a stable shell-owned origin (127.0.0.1:51821, outside Kimi's 58627+ daemon range), handing over the daemon via the official `kimi_origin` parameter and the credential via the URL hash; the stable origin preserves UI preferences, and missing assets fall back to the daemon-hosted UI
4. The status bar is the shell's own page talking to the daemon over REST/WebSocket; the injected script only adds desktop behaviors (notifications, dragging, etc.)

## Relationship with upstream

- The Web UI comes directly from `apps/kimi-code/dist-web` at official tag `@moonshot-ai/kimi-code@0.33.0`
- The old `liujunGH/kimi-code` branch `kimi-ui` remains only as a historical backup; it is no longer rebased or released
- UI and protocol issues go upstream; this repo maintains only the desktop window, native bridge, status bar, and packaging

## Maintenance notes

A three-layer watchdog warns loudly when official updates drift the DOM selectors, the status-bar REST/WS protocol, or the `/usage` scrape format. Shell-side fixes are concentrated in `src/main.rs` (`INIT_SCRIPT`). Worst case after an official update: a feature degrades to stock behavior — never silent breakage.

## Layout

```
src/main.rs            # shell logic (window layout, static server, injected script, commands, quota scrape, update check)
src/static_server.rs   # dependency-free static server (serves web-dist)
public/index.html      # launch placeholder page
public/status.html     # native status bar
scripts/build-web.sh   # stage the official prebuilt web bundle
scripts/icon.swift     # icon generator
packaging/             # Info.plist, make-app.sh, LaunchAgent plist
.github/workflows/     # CI release
capabilities/          # Tauri window permissions (dragging + remote-origin IPC)
icons/                 # generated icons
```

## License

[MIT](LICENSE)
