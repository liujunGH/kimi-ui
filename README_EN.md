# kimi-ui

A desktop client for [Kimi Code](https://www.kimi.com/code/): **official daemon + customized web UI + native shell**. The shell (this repo) owns the window, status bar, and system integrations; the UI is a customized kimi-web built from the fork branch [liujunGH/kimi-code `kimi-ui`](https://github.com/liujunGH/kimi-code/tree/kimi-ui) — scroll-follow, long-session performance, and list styling are fixed at the source level instead of via injected patches.

> Kimi Code 桌面端：官方 daemon + 定制 web UI + 原生壳。[中文说明](README.md)

## Features

- **Real app form**: own window, Dock icon, Cmd-Tab, close-to-quit; hidden-inset title bar with drag regions and double-click zoom
- **Customized web UI** (source-level, in the fork): auto-follow resumes only when you deliberately reach the bottom; tool groups default to collapsed and unmount on close; long outputs render tail-biased; only changed turns re-render during streaming; loaded history capped at 600 messages
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

Requires **both repos** (shell + custom UI source):

```bash
git clone https://github.com/liujunGH/kimi-ui.git
git clone -b kimi-ui https://github.com/liujunGH/kimi-code.git

cd kimi-ui
bash scripts/build-web.sh     # build the custom web bundle into web-dist/ (pnpm + corepack)
bash packaging/make-app.sh    # build the shell, assemble, ad-hoc sign
cp -R "Kimi Code.app" /Applications/
```

Requires: Rust toolchain, Node ≥ 24.15 (pnpm via corepack).

## How it works

1. Runs `kimi server run` (idempotent: starts the daemon or reuses a running one)
2. Reads the daemon's address and credential from kimi's local data directory
3. Serves the customized web bundle on a tiny built-in static server (127.0.0.1:58628), and hands over credential + daemon origin via the URL hash (same shape as the official flow); falls back to the daemon-hosted UI if web-dist is missing
4. The status bar is the shell's own page talking to the daemon over REST/WebSocket; the injected script only adds desktop behaviors (notifications, dragging, etc.)

## Relationship with upstream

- The fork branch `kimi-ui` only touches `apps/kimi-web` and rebases on upstream main periodically
- Generic fixes go back upstream: [#1898](https://github.com/MoonshotAI/kimi-code/pull/1898) (list numbers), [#1899](https://github.com/MoonshotAI/kimi-code/pull/1899) (scroll follow), [#1900](https://github.com/MoonshotAI/kimi-code/pull/1900) (long-session performance)
- Shell-specific changes (daemon-origin handoff, relative asset base) stay in the fork

## Maintenance notes

A three-layer watchdog warns loudly when official updates drift the DOM selectors, the status-bar REST/WS protocol, or the `/usage` scrape format. Fixes live in `src/main.rs` (`INIT_SCRIPT`) and the corresponding fork components. Worst case after an official update: a feature degrades to stock behavior — never silent breakage.

## Layout

```
src/main.rs            # shell logic (window layout, static server, injected script, commands, quota scrape, update check)
src/static_server.rs   # dependency-free static server (serves web-dist)
public/index.html      # launch placeholder page
public/status.html     # native status bar
scripts/build-web.sh   # build the custom web bundle from the fork
scripts/icon.swift     # icon generator
packaging/             # Info.plist, make-app.sh, LaunchAgent plist
.github/workflows/     # CI release
capabilities/          # Tauri window permissions (dragging + remote-origin IPC)
icons/                 # generated icons
```

## License

[MIT](LICENSE)
