// Prevents the black console window on Windows in release builds: without
// this the linker marks the exe as console-subsystem and Windows attaches a
// console at launch. Debug builds keep the console for log output.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! kimi-ui: minimal desktop shell for the Kimi Code web UI.
//!
//! Launch flow:
//!   1. open a window with a tiny local placeholder page immediately;
//!   2. on a background thread, attach to an already-running server if one
//!      exists and is not older than the installed CLI (a stale daemon whose
//!      `host_version` lags the CLI is skipped and gracefully shut down via
//!      its own `/api/v1/shutdown`, so a `kimi upgrade` actually takes effect
//!      on the next launch instead of silently attaching to the old server);
//!      otherwise spawn `kimi web --no-open` and kill that child
//!      on app exit. Discover the address from `server/instances/*.json`
//!      first,
//!      falling back to the legacy `server/lock`, and read the bearer token
//!      from `server.token` under $KIMI_CODE_HOME (default ~/.kimi-code).
//!      kimi is version-gated (≥ 0.39); missing or outdated installs get a
//!      guided error page;
//!   3. navigate the main webview to the official UI on the loopback static
//!      server (127.0.0.1:51821) — the official prebuilt web bundle,
//!      embedded into the exe in release builds and served from disk in
//!      dev — with token and daemon origin handed over via the URL.
//!
//! Window layout (nothing shifts the SPA):
//!   - a bare window holds two child webviews;
//!   - the main webview (the official web UI) always stops `STRIP` px short
//!     of the window's bottom edge — it never resizes afterwards;
//!   - a transparent, shell-owned "status" webview sits in that strip. On
//!     demand it grows upward *over* the main webview (the main webview
//!     does NOT move) to float a card: context-usage detail, the update
//!     notice, or the Remote Control panel. It talks to the daemon directly
//!     over REST, so it has ZERO DOM coupling to the SPA and is the shell's
//!     extensible UI surface.
//!
//! Desktop integrations in the main webview (injected script):
//!   - `window.Notification` polyfill -> native notifications, bumping the
//!     Dock badge until the window is focused again;
//!   - `window.focus()` -> raise the native window;
//!   - downloads land in ~/Downloads with de-duplicated filenames;
//!   - external links open in the system browser;
//!   - hidden-inset title bar: SPA drag areas mirrored to Tauri's
//!     `data-tauri-drag-region`; double-click toggles zoom; the "internal
//!     testing" badge is hidden;
//!   - the SPA's route is reported to the shell, which titles the window
//!     ("<session> — Kimi Code", visible in Cmd-Tab) and feeds the 会话
//!     menu's recent-session list;
//!   - streaming thinking blocks height-capped (no more chat climbing);
//!   - double-digit ordered-list numbers unclipped;
//!   - watchdog warns once if the SPA's desktop classes vanish.
//!
//! Native chrome: a Chinese-labeled menu bar (app/file/edit/sessions/window/
//! help) installed via `app.set_menu`; Remote Control (official experimental
//! `kimi rc`) is spawned/terminated by the `remote_control` command and its
//! access URL + QR are shown in the status bar's remote card.
//!
//! NOTE: the main page is a *remote* origin to Tauri, so `capabilities/`
//! must list the daemon URL under `remote.urls` — otherwise every IPC
//! invoke from the injected script is silently denied.
//!
//! The daemon exits by itself 60s after the last client disconnects, so the
//! shell does not manage its lifecycle.

use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Mutex,
    },
    thread,
    time::Duration,
};

use serde_json::Value;
use tauri::{LogicalPosition, LogicalSize, Manager, Url, WebviewBuilder, WebviewUrl, WindowBuilder};
use tauri_plugin_notification::NotificationExt;

mod static_server;

/// Status strip height (collapsed), tab bar height, and overlay height (open).
const STRIP: f64 = 28.0;
/// Tab bar height when only the tab strip is shown, and the taller height it
/// grows to while the "add connection" dialog is open. The dialog lives in the
/// tab-bar webview, so that webview must actually be tall enough to contain it
/// — a fixed 36px strip clips everything below the tabs.
const TABBAR_H: f64 = 36.0;
const TABBAR_EXPANDED_H: f64 = 420.0;
/// Whether the tab bar is expanded to show the add-connection dialog.
static TABBAR_EXPANDED: AtomicBool = AtomicBool::new(false);
const OVERLAY_H: f64 = 340.0;
/// Stable, shell-owned origin so Web Storage survives app restarts. Keep this
/// outside Kimi's 58627+ daemon port range.
const CUSTOM_UI_PORT: u16 = 51821;

/// Unread-notification count shown on the Dock icon (macOS).
static BADGE_COUNT: AtomicU32 = AtomicU32::new(0);
/// Whether any overlay card is open (usage / update / remote) — the status
/// webview then grows to OVERLAY_H over the main webview.
static OVERLAY_OPEN: AtomicBool = AtomicBool::new(false);

/// Daemon connection details shared with the status webview.
#[derive(Clone)]
struct DaemonState {
    base: String,
    token: String,
}

type SharedDaemon = Mutex<Option<DaemonState>>;

// ---------------------------------------------------------------------------
// Connections (the tab bar's model).
//
// The shell can view several daemons at once: the local one it manages, plus
// any remote daemon the user points it at. Each connection is one child
// webview; only the active one is visible.
//
// How each kind is loaded matters:
//   - Local  -> the shell's own loopback static server (stable origin: keeps
//               UI preferences and long-lived hash asset caching) with the
//               daemon origin handed over via `kimi_origin`.
//   - Remote -> the daemon's OWN served UI, same-origin. The official SPA
//               resolves its API origin from `window.location.origin` when no
//               `kimi_origin` is given (verified in the 0.42.0 bundle), which
//               is exactly the flow `kimi web` uses when it opens a browser.
//               Same-origin also means no CORS allow-list on the far side.
// ---------------------------------------------------------------------------

/// Where a connection's UI comes from.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConnKind {
    /// The daemon this machine's CLI runs; managed by the shell.
    Local,
    /// Someone else's daemon, reached by address.
    Remote,
}

/// One viewable daemon.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Connection {
    /// Stable key for the webview label and menu ids.
    id: String,
    /// Shown on the tab.
    name: String,
    kind: ConnKind,
    /// `http://host:port` (no query).
    base: String,
    /// Bearer token (empty until known for the local daemon).
    token: String,
}

/// `(id, name)` of every connection, for the status page's tab bar.
type SharedConnections = Mutex<Vec<Connection>>;
/// Id of the connection currently shown.
type SharedActive = Mutex<Option<String>>;

/// Webview label for a connection's view. Tauri restricts labels to
/// alphanumerics plus `- / : _`, and a connection id is derived from a host
/// (dots!) so it must be sanitized.
fn view_label(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '/' | ':' | '_') { c } else { '_' })
        .collect();
    format!("view-{safe}")
}

/// URL to load for a connection.
///
/// Local daemons go through the shell's loopback server (stable origin);
/// remote ones load the daemon's own UI, which is same-origin and therefore
/// needs no `kimi_origin` and no CORS entry on the far side.
fn connection_url(conn: &Connection) -> Url {
    match conn.kind {
        ConnKind::Local => custom_ui_url(&conn.base, &conn.token)
            .unwrap_or_else(|| conn.base.parse().expect("valid local url")),
        ConnKind::Remote => {
            let base = conn.base.trim_end_matches('/');
            format!("{base}/?kimi_desktop&platform={}#token={}", platform_name(), conn.token)
                .parse()
                .expect("valid remote url")
        }
    }
}

/// Node's `process.platform` value — the official bundle reads this flag.
fn platform_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else {
        "linux"
    }
}

fn connections_path() -> PathBuf {
    app_config_dir().join("connections.json")
}

fn app_config_dir() -> PathBuf {
    // ~/Library/Application Support/<bundle id> on macOS.
    home_dir()
        .join("Library/Application Support")
        .join("dev.kimiui.desktop")
}

/// Load persisted connections. Remote entries carry their token; the local
/// entry is stored as a *preferences only* stub (no token/base — those are
/// re-derived from the live daemon each launch) so a renamed local tab keeps
/// its name.
fn load_connections() -> Vec<Connection> {
    let Ok(raw) = fs::read_to_string(connections_path()) else {
        return Vec::new();
    };
    let mut conns: Vec<Connection> = serde_json::from_str(&raw).unwrap_or_default();
    // Drop any persisted local entry: the caller re-derives it from the daemon.
    conns.retain(|c| c.kind == ConnKind::Remote);
    conns
}

/// The persisted local tab name, if the user renamed it.
fn load_local_name() -> Option<String> {
    let raw = fs::read_to_string(connections_path()).ok()?;
    let conns: Vec<Connection> = serde_json::from_str(&raw).ok()?;
    conns
        .iter()
        .find(|c| c.kind == ConnKind::Local)
        .map(|c| c.name.clone())
}

/// Persist remote connections (token included) plus the local tab's name.
/// Owner-only permissions: the file holds bearer tokens, matching how the
/// official CLI stores its own `server.token`.
fn save_connections(conns: &[Connection]) {
    // Remote connections keep everything; the local one is stored name-only so
    // the file never carries a local token it does not need.
    let persisted: Vec<Connection> = conns
        .iter()
        .map(|c| match c.kind {
            ConnKind::Remote => c.clone(),
            ConnKind::Local => Connection {
                id: c.id.clone(),
                name: c.name.clone(),
                kind: ConnKind::Local,
                base: String::new(),
                token: String::new(),
            },
        })
        .collect();
    let Ok(json) = serde_json::to_string_pretty(&persisted) else { return };
    let dir = app_config_dir();
    let _ = fs::create_dir_all(&dir);
    let path = connections_path();
    if fs::write(&path, json).is_err() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
}

/// Validate a user-entered remote address into a connection. Accepts
/// `host:port`, `http://host:port`, or a bare host (default port 58627).
fn parse_remote_connection(name: &str, address: &str, token: &str) -> Result<Connection, String> {
    let trimmed = address.trim();
    if trimmed.is_empty() {
        return Err("请填写地址".to_string());
    }
    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let url: Url = with_scheme
        .parse()
        .map_err(|_| format!("地址无法解析：{address}"))?;
    let Some(host) = url.host_str() else {
        return Err("地址缺少主机名".to_string());
    };
    if host.is_empty() {
        return Err("地址缺少主机名".to_string());
    }
    let port = url.port().unwrap_or(58627);
    let base = format!("http://{host}:{port}");
    let name = if name.trim().is_empty() {
        format!("{host}:{port}")
    } else {
        name.trim().to_string()
    };
    Ok(Connection {
        id: format!("{host}-{port}"),
        name,
        kind: ConnKind::Remote,
        base,
        token: token.trim().to_string(),
    })
}

/// Script injected at document start on the MAIN webview's pages only
/// (WKUserScript is not affected by the SPA's CSP).
const INIT_SCRIPT: &str = r#"
(function () {
  'use strict';
  function invoke(cmd, args) {
    try {
      if (window.__TAURI_INTERNALS__) window.__TAURI_INTERNALS__.invoke(cmd, args || {});
    } catch (e) { /* ignore */ }
  }

  // 1. Notification polyfill.
  if (typeof window.Notification === 'undefined') {
    class KimiNotification extends EventTarget {
      static get permission() { return 'granted'; }
      static requestPermission(cb) {
        if (typeof cb === 'function') cb('granted');
        return Promise.resolve('granted');
      }
      constructor(title, options) {
        super();
        options = options || {};
        this.title = String(title);
        this.body = options.body ? String(options.body) : '';
        this.tag = options.tag || '';
        this.onclick = null; this.onshow = null; this.onclose = null; this.onerror = null;
        invoke('notify', { title: this.title, body: this.body });
      }
      close() {}
    }
    window.Notification = KimiNotification;
  }

  // 2. window.focus() -> raise the native window.
  window.focus = function () { invoke('focus_window', {}); };

  var isDesktop = new URLSearchParams(location.search).has('kimi_desktop')
    || (function () { try { return sessionStorage.getItem('kimi-desktop') === '1'; } catch (e) { return false; } })();

  // 3. Static shell fixes live in one stylesheet. This avoids repeatedly
  // walking the entire chat DOM while the official UI is streaming.
  var shellStyleId = 'kimi-desktop-shell-style';
  function ensureShellStyle() {
    if (document.getElementById(shellStyleId)) return;
    var root = document.head || document.documentElement;
    if (!root) return;
    var style = document.createElement('style');
    style.id = shellStyleId;
    style.textContent = [
      '.internal-build-tag{display:none!important}',
      '.tc-wrap:not(.is-collapsed) pre.tc{max-height:9em!important;overflow-y:auto!important}',
      '.md ol{padding-left:2.2em!important}',
      '.u-turn{content-visibility:auto;contain-intrinsic-block-size:auto 96px}',
      '.a-msg{content-visibility:auto;contain-intrinsic-block-size:auto 480px}'
    ].join('');
    root.appendChild(style);
  }

  // Drag-region attributes cannot be expressed in CSS. Patch only newly
  // inserted subtrees instead of running a full-document query on every
  // MutationObserver batch.
  var dragSelector = '.side.macos-desktop .ch, .chat-header.macos-desktop';
  function patchDragScope(root) {
    if (!root || (root.nodeType !== 1 && root.nodeType !== 9)) return;
    if (root.matches && root.matches(dragSelector)) {
      root.setAttribute('data-tauri-drag-region', 'deep');
    }
    var els = root.querySelectorAll ? root.querySelectorAll(dragSelector) : [];
    for (var i = 0; i < els.length; i++) {
      if (els[i].getAttribute('data-tauri-drag-region') !== 'deep') {
        els[i].setAttribute('data-tauri-drag-region', 'deep');
      }
    }
  }

  var pendingRoots = [];
  var patchScheduled = false;
  function queuePatch(root) {
    if (!root || root.nodeType !== 1) return;
    if (pendingRoots.indexOf(root) === -1) pendingRoots.push(root);
    schedulePatch();
  }
  function schedulePatch() {
    if (patchScheduled) return;
    patchScheduled = true;
    requestAnimationFrame(function () {
      patchScheduled = false;
      ensureShellStyle();
      var roots = pendingRoots.splice(0, pendingRoots.length);
      for (var i = 0; i < roots.length; i++) patchDragScope(roots[i]);
    });
  }
  new MutationObserver(function (records) {
    for (var i = 0; i < records.length; i++) {
      for (var j = 0; j < records[i].addedNodes.length; j++) {
        queuePatch(records[i].addedNodes[j]);
      }
    }
    // The SPA can replace <head>; restore the shell stylesheet if needed.
    if (!document.getElementById(shellStyleId)) schedulePatch();
  }).observe(document.documentElement, { childList: true, subtree: true });
  ensureShellStyle();
  patchDragScope(document);

  // 4. Double-click on a drag region toggles maximize (zoom).
  document.addEventListener('dblclick', function (e) {
    var t = e.target;
    if (!t || !t.closest) return;
    if (!t.closest('[data-tauri-drag-region]')) return;
    if (t.closest('button, a, input, textarea, select, label, [role="button"], [contenteditable]')) return;
    invoke('toggle_maximize', {});
  }, true);

  // 5. Report the SPA's active session so the shell can title the window.
  //    The official SPA is a pathname router: "/" = new session,
  //    "/sessions/<id>" = that session. Pure URL coupling, no DOM walk.
  var activeSession;
  function reportRoute() {
    var m = location.pathname.match(/^\/sessions\/([^\/]+)/);
    var id = m ? decodeURIComponent(m[1]) : null;
    if (id !== activeSession) {
      activeSession = id;
      invoke('set_active_session', { id: id });
    }
  }
  ['pushState', 'replaceState'].forEach(function (fn) {
    var orig = history[fn].bind(history);
    history[fn] = function () {
      var r = orig.apply(null, arguments);
      reportRoute();
      return r;
    };
  });
  window.addEventListener('popstate', reportRoute);
  reportRoute();

  // 6. Watchdog: verify each selector group this shell depends on. If an
  //    official UI update breaks one, warn once with the broken features.
  if (isDesktop) {
    var domWarned = false;
    setInterval(function () {
      if (domWarned) return;
      var broken = [];
      var header = document.querySelector('.chat-header');
      var side = document.querySelector('.side');
      if ((header && !header.classList.contains('macos-desktop'))
        || (side && !side.classList.contains('macos-desktop'))) {
        broken.push('窗口拖拽/桌面布局');
      }
      var pill = document.querySelector('.internal-build-tag');
      if (pill && getComputedStyle(pill).display !== 'none' && pill.offsetParent !== null) {
        broken.push('角标隐藏');
      }
      var tc = document.querySelector('.tc-wrap:not(.is-collapsed) pre.tc');
      if (tc && !document.getElementById(shellStyleId)) {
        broken.push('思考限高');
      }
      if (broken.length) {
        domWarned = true;
        invoke('notify', {
          title: 'Kimi Code',
          body: '检测到官方界面结构更新，以下功能可能失效：' + broken.join('、') + '。请更新桌面壳'
        });
      }
    }, 20000);
  }
})();
"#;

fn home_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    #[cfg(not(target_os = "windows"))]
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home)
}

fn kimi_home() -> PathBuf {
    std::env::var("KIMI_CODE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".kimi-code"))
}

/// Locate the `kimi` binary. GUI apps launched from Finder get a minimal PATH,
/// so fall back to well-known install locations.
fn find_kimi() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    const EXE: &str = "kimi.exe";
    #[cfg(not(target_os = "windows"))]
    const EXE: &str = "kimi";

    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(EXE);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    let candidates = [
        home_dir().join(".kimi-code/bin/kimi"),
        PathBuf::from("/opt/homebrew/bin/kimi"),
        PathBuf::from("/usr/local/bin/kimi"),
    ];
    #[cfg(target_os = "windows")]
    let candidates = [home_dir().join(".kimi-code/bin").join(EXE)];
    candidates.into_iter().find(|p| p.is_file())
}

/// Open external links in the system browser instead of a bare webview window.
fn open_in_system_browser(url: &Url) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    #[cfg(target_os = "windows")]
    let program = "rundll32";

    #[cfg(target_os = "windows")]
    let args = ["url.dll,FileProtocolHandler", url.as_str()];
    #[cfg(not(target_os = "windows"))]
    let args = [url.as_str()];

    let _ = Command::new(program).args(args).spawn();
}

/// Set the Dock icon badge label (0 clears it).
#[cfg(target_os = "macos")]
fn set_dock_badge(app: &tauri::AppHandle, count: u32) {
    let _ = app.run_on_main_thread(move || {
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSApplication;
        use objc2_foundation::NSString;

        let Some(mtm) = MainThreadMarker::new() else { return };
        let tile = NSApplication::sharedApplication(mtm).dockTile();
        if count == 0 {
            tile.setBadgeLabel(None);
        } else {
            tile.setBadgeLabel(Some(&NSString::from_str(&count.to_string())));
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn set_dock_badge(_app: &tauri::AppHandle, _count: u32) {}

struct Launch {
    base: String,
    token: String,
    url: Url,
}

/// Windows: spawn console-subsystem children (kimi.exe, gh.exe) without
/// allocating a black console window. No-op on other platforms.
fn no_console(cmd: Command) -> Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut cmd = cmd;
        cmd.creation_flags(CREATE_NO_WINDOW);
        return cmd;
    }
    #[cfg(not(target_os = "windows"))]
    cmd
}

/// Oldest kimi CLI paired with the official web bundle shipped by this build.
const MIN_KIMI_VERSION: &str = "0.39.1";

/// Structured boot failure; the placeholder page renders it as guided steps.
#[derive(Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum BootError {
    /// kimi CLI not found on PATH or in well-known install spots.
    KimiMissing,
    /// kimi found but older than `min`.
    KimiTooOld { version: String, min: String },
    /// No reachable server came up; `detail` says what was tried.
    DaemonUnreachable {
        detail: String,
        version: Option<String>,
    },
}

/// The `kimi web` child we spawned ourselves (step 3 below); killed on app
/// exit so it does not outlive us — mirrors the old daemon's idle exit.
static SPAWNED_SERVER: Mutex<Option<Child>> = Mutex::new(None);

/// Ensure a local server is running and build its address/credentials.
///
///   1. attach to an already-running server (the user's own `kimi web`, or a
///      daemon from an older CLI) — no subprocess at all, unless the daemon
///      is positively older than the installed CLI: stale daemons are skipped
///      and then gracefully shut down via their own `/api/v1/shutdown` route,
///      so upgrading the CLI actually moves the app onto a fresh server and
///      the old one stops pinning its port;
///   2. spawn `kimi web --no-open` ourselves and wait (≤15s, bailing early
///      if the child dies) for it to register under `server/instances/`.
fn connect_daemon() -> Result<Launch, BootError> {
    let kimi = find_kimi().ok_or(BootError::KimiMissing)?;
    let version = kimi_version(&kimi);
    if let Some(v) = &version {
        if version_older(v, MIN_KIMI_VERSION) {
            return Err(BootError::KimiTooOld {
                version: v.clone(),
                min: MIN_KIMI_VERSION.to_string(),
            });
        }
    }

    let home = kimi_home();
    if let Ok(launch) = attach(&home, version.as_deref()) {
        shutdown_stale_daemons(&home, version.as_deref(), &launch.token);
        return Ok(launch);
    }

    let (mut child, port) = spawn_web_server(&kimi).map_err(|e| BootError::DaemonUnreachable {
        detail: format!("拉起 `kimi web --no-open` 失败：{e}"),
        version: version.clone(),
    })?;
    match wait_attach(
        &home,
        &mut child,
        port,
        50,
        Duration::from_millis(300),
    ) {
        Ok(launch) => {
            if let Ok(mut guard) = SPAWNED_SERVER.lock() {
                *guard = Some(child);
            }
            shutdown_stale_daemons(&home, version.as_deref(), &launch.token);
            Ok(launch)
        }
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(BootError::DaemonUnreachable {
                detail: e,
                version,
            })
        }
    }
}

/// Attach to a reachable daemon (single attempt) and build the launch params.
/// Candidates are verified with an authenticated `/api/v1/meta` probe rather
/// than a bare TCP connect: a daemon that is shutting down still accepts
/// sockets, and latching onto one would leave the app talking to a dying
/// server. Falls through every candidate until one actually answers.
fn attach(home: &Path, cli_version: Option<&str>) -> Result<Launch, String> {
    let token = read_server_token(home).ok_or("读取 server.token 失败")?;
    let candidates = daemon_candidates(home, cli_version);
    let mut last = String::from("没有发现可达的 kimi daemon（server/instances 与 server/lock 均无效）");
    for addr in candidates {
        if !tcp_alive(&addr.host, addr.port) {
            continue;
        }
        match daemon_meta(&addr, &token) {
            Ok(_) => return launch_at_addr(&token, addr),
            Err(e) => last = format!("daemon {}:{} 无响应：{e}", addr.host, addr.port),
        }
    }
    Err(last)
}

fn read_server_token(home: &Path) -> Option<String> {
    fs::read_to_string(home.join("server.token"))
        .ok()
        .map(|t| t.trim().to_string())
}

/// Build launch details for a specific reachable server.
fn launch_at(home: &Path, addr: DaemonAddr) -> Result<Launch, String> {
    let token = read_server_token(home).ok_or("读取 server.token 失败")?;
    launch_at_addr(&token, addr)
}

fn launch_at_addr(token: &str, addr: DaemonAddr) -> Result<Launch, String> {
    let desktop_query = desktop_query();
    let base = format!("http://{}:{}", addr.host, addr.port);
    let url = format!("{base}/{desktop_query}#token={token}")
        .parse()
        .map_err(|e| format!("构造 web UI 地址失败：{e}"))?;
    Ok(Launch {
        base,
        token: token.to_string(),
        url,
    })
}

/// Official bundle flags for desktop-only behavior. Keep the platform values
/// aligned with Node's `process.platform`, which the web bundle also uses.
fn desktop_query() -> &'static str {
    if cfg!(target_os = "macos") {
        "?kimi_desktop&platform=darwin"
    } else if cfg!(target_os = "windows") {
        "?kimi_desktop&platform=win32"
    } else {
        "?kimi_desktop&platform=linux"
    }
}

/// Poll `attach` until the spawned server registers (bounded by
/// `attempts × interval`); bails early with the child's captured stderr when
/// the child exits before becoming reachable.
fn wait_attach(
    home: &Path,
    child: &mut Child,
    port: u16,
    attempts: u32,
    interval: Duration,
) -> Result<Launch, String> {
    let mut last_err = String::new();
    for attempt in 0..attempts {
        let host = "127.0.0.1";
        if tcp_alive(host, port) {
            match launch_at(
                home,
                DaemonAddr {
                    host: host.to_string(),
                    port,
                },
            ) {
                Ok(launch) => return Ok(launch),
                Err(e) => last_err = e,
            }
        } else {
            last_err = format!("等待 {host}:{port} 接受连接");
        }
        if let Ok(Some(status)) = child.try_wait() {
            let log = fs::read_to_string(web_stderr_log()).unwrap_or_default();
            let tail: String = log
                .chars()
                .skip(log.chars().count().saturating_sub(600))
                .collect();
            return Err(format!("`kimi web` 提前退出（{status}）：{}", tail.trim()));
        }
        if attempt + 1 < attempts {
            thread::sleep(interval);
        }
    }
    Err(format!("等待服务就绪超时：{last_err}"))
}

/// Temp file capturing the spawned server's stderr (read back on failure).
fn web_stderr_log() -> PathBuf {
    std::env::temp_dir().join("kimi-ui-web-server.log")
}

/// Spawn `kimi web --no-open` on an explicitly selected free port. Selecting
/// the port here keeps the shell attached to its own child when the default
/// 58627 is already occupied by another Kimi instance.
fn spawn_web_server(kimi: &Path) -> Result<(Child, u16), String> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    drop(listener);

    let log = fs::File::create(web_stderr_log()).map_err(|e| e.to_string())?;
    let port_arg = port.to_string();
    let child = no_console(Command::new(kimi))
        .args(["web", "--port", port_arg.as_str(), "--no-open"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok((child, port))
}

/// `kimi --version` → e.g. "0.27.0". Best-effort: None when unparseable.
fn kimi_version(kimi: &Path) -> Option<String> {
    let out = no_console(Command::new(kimi)).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_kimi_version(&String::from_utf8_lossy(&out.stdout))
}

/// Parse a bare semver from `--version` stdout; rejects anything fancier.
fn parse_kimi_version(stdout: &str) -> Option<String> {
    let v = stdout.trim();
    let valid = !v.is_empty()
        && v
            .split('.')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    valid.then(|| v.to_string())
}

/// Strict semver-ish less-than; equal is not older.
fn version_older(v: &str, min: &str) -> bool {
    version_newer(min, v)
}

/// A reachable daemon address.
struct DaemonAddr {
    host: String,
    port: u16,
}

/// Wildcard binds (`0.0.0.0` / `::`) are reached via loopback — mirrors
/// kap-server's `normalizeHost` (apps/kimi-inspect/vite/serverDiscovery.ts).
fn normalize_host(host: Option<&str>) -> String {
    match host {
        Some(h) if !h.is_empty() && h != "0.0.0.0" && h != "::" && h != "[::]" => h.to_string(),
        _ => "127.0.0.1".to_string(),
    }
}

/// TCP connect probe — answers "can we actually talk to this daemon", which
/// is what the shell needs; unlike a pid-liveness check it behaves the same
/// on every OS and is immune to pid reuse.
fn tcp_alive(host: &str, port: u16) -> bool {
    let Ok(mut addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    addrs.any(|a| TcpStream::connect_timeout(&a, Duration::from_millis(300)).is_ok())
}

/// A daemon is stale when we positively know both versions and the daemon's
/// `host_version` is older than the installed CLI. Unknown/missing versions
/// keep the legacy attach behavior — only a proven downgrade skips.
fn daemon_stale(host_version: Option<&str>, cli_version: Option<&str>) -> bool {
    match (host_version, cli_version) {
        (Some(d), Some(c)) => version_older(d, c),
        _ => false,
    }
}

/// Ordered candidate addresses from the multi-instance registry (longest
/// running first) plus the legacy lock. Pure parsing — no liveness checks, so
/// callers can apply their own (TCP-only vs authenticated).
fn daemon_candidates(home: &Path, cli_version: Option<&str>) -> Vec<DaemonAddr> {
    let mut candidates: Vec<(u64, String, u16)> = Vec::new();

    if let Ok(rd) = fs::read_dir(home.join("server/instances")) {
        for entry in rd.flatten() {
            if !entry.file_name().to_string_lossy().ends_with(".json") {
                continue;
            }
            let Ok(raw) = fs::read_to_string(entry.path()) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            if daemon_stale(v["host_version"].as_str(), cli_version) {
                continue;
            }
            let Some(port) = v["port"].as_u64().and_then(|p| u16::try_from(p).ok()) else {
                continue;
            };
            let started_at = v["started_at"].as_u64().unwrap_or(0);
            candidates.push((started_at, normalize_host(v["host"].as_str()), port));
        }
    }
    // Longest-running instance first (upstream sorts started_at ascending).
    candidates.sort_by_key(|(started_at, _, _)| *started_at);

    // Legacy lock: no comparable started_at (old builds wrote an ISO string),
    // so it always sorts after every registry instance.
    if let Ok(raw) = fs::read_to_string(home.join("server/lock")) {
        if let Ok(v) = serde_json::from_str::<Value>(&raw) {
            if !daemon_stale(v["host_version"].as_str(), cli_version) {
                if let Some(port) = v["port"].as_u64().and_then(|p| u16::try_from(p).ok()) {
                    candidates.push((u64::MAX, normalize_host(v["host"].as_str()), port));
                }
            }
        }
    }

    candidates
        .into_iter()
        .map(|(_, host, port)| DaemonAddr { host, port })
        .collect()
}

/// Discover a live daemon address: scan the multi-instance registry
/// (`server/instances/*.json`, longest-running first), then fall back to the
/// legacy single-instance `server/lock`. Mirrors kap-server's own discovery
/// order (`packages/kap-server/src/instanceRegistry.ts`). Every candidate is
/// verified with a TCP connect, so stale files from crashed daemons are
/// skipped instead of fatal. Daemons older than the installed CLI are skipped
/// too, so an upgraded CLI is not shadowed by a long-running old server.
///
/// TCP-only discovery: kept for tests and diagnostics. Production attach uses
/// `daemon_candidates` plus an authenticated probe (a shutting-down daemon
/// still accepts sockets), so this variant is test-only.
#[cfg(test)]
fn discover_daemon(home: &Path, cli_version: Option<&str>) -> Result<DaemonAddr, String> {
    for addr in daemon_candidates(home, cli_version) {
        if tcp_alive(&addr.host, addr.port) {
            return Ok(addr);
        }
    }
    Err("没有发现可达的 kimi daemon（server/instances 与 server/lock 均无效）".to_string())
}

/// Reachable daemons proven older than the installed CLI — the ones
/// discover_daemon skipped. Scans the same registry + legacy lock.
fn stale_daemons(home: &Path, cli_version: Option<&str>) -> Vec<DaemonAddr> {
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = fs::read_dir(home.join("server/instances")) {
        for entry in rd.flatten() {
            if entry.file_name().to_string_lossy().ends_with(".json") {
                paths.push(entry.path());
            }
        }
    }
    paths.push(home.join("server/lock"));

    let mut stale = Vec::new();
    for path in paths {
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if !daemon_stale(v["host_version"].as_str(), cli_version) {
            continue;
        }
        let Some(port) = v["port"].as_u64().and_then(|p| u16::try_from(p).ok()) else {
            continue;
        };
        let host = normalize_host(v["host"].as_str());
        if tcp_alive(&host, port) {
            stale.push(DaemonAddr { host, port });
        }
    }
    stale
}

/// Ask one daemon to exit via its own graceful route (`POST /api/v1/shutdown`
/// — loopback-only and bearer-authenticated, so no pid-reuse risk, and the
/// same mechanism the official `kimi server kill` fallback uses). The server
/// closes the connection as it exits, so every IO/response error is normal
/// and ignored.
fn shutdown_daemon(addr: &DaemonAddr, token: &str) {
    let request = format!(
        "POST /api/v1/shutdown HTTP/1.1\r\nHost: {}:{}\r\nAuthorization: Bearer {token}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        addr.host, addr.port
    );
    let Ok(mut stream) = TcpStream::connect((addr.host.as_str(), addr.port)) else {
        return;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    if stream.write_all(request.as_bytes()).is_err() {
        return;
    }
    let mut buf = [0u8; 512];
    let _ = stream.read(&mut buf);
}

/// Shut down stale daemons once a good server is secured. They were skipped
/// during attach, so leaving them running only pins their ports and memory.
/// Best-effort: failures just mean the stale daemon stays up as before.
fn shutdown_stale_daemons(home: &Path, cli_version: Option<&str>, token: &str) {
    for addr in stale_daemons(home, cli_version) {
        shutdown_daemon(&addr, token);
    }
}

// ---------------------------------------------------------------------------
// Daemon restart.
//
// Attaching to a port that answers TCP is not enough: a daemon in the middle
// of shutting down still accepts connections, so a new instance can latch onto
// a dying server. The restart below follows the sequence the sibling kimi-gui
// shell uses, which is the difference between "usually works" and reliable:
//   1. graceful shutdown via POST /api/v1/shutdown (bearer-authenticated,
//      loopback-only, so there is no pid-reuse risk);
//   2. wait for the port to actually stop accepting connections;
//   3. relaunch the installed CLI on the same host/port;
//   4. wait until the registry reports a DIFFERENT pid — proof the old server
//      is gone and the new one is serving, not merely that a socket exists.
// ---------------------------------------------------------------------------

/// How long to wait for the old daemon to release its port.
const STOP_WAIT: Duration = Duration::from_secs(8);
/// How long to wait for the replacement daemon to register.
const START_WAIT: Duration = Duration::from_secs(12);

/// True when the daemon behind `addr` answers an authenticated request — a
/// stronger liveness check than a bare TCP connect, because a shutting-down
/// server may still accept sockets while refusing to serve.
fn daemon_healthy(addr: &DaemonAddr, token: &str) -> bool {
    daemon_meta(addr, token).is_ok()
}

/// `GET /api/v1/meta` against a specific address; Ok when the daemon answers
/// 200 with a JSON envelope. Used both for liveness and for version reads.
fn daemon_meta(addr: &DaemonAddr, token: &str) -> Result<Value, String> {
    let base = format!("http://{}:{}", addr.host, addr.port);
    http_get_json(&base, "/api/v1/meta", token)
}

/// The registry entry (pid + port) for a specific loopback endpoint.
fn instance_pid_for(home: &Path, addr: &DaemonAddr) -> Option<u32> {
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = fs::read_dir(home.join("server/instances")) {
        for entry in rd.flatten() {
            if entry.file_name().to_string_lossy().ends_with(".json") {
                paths.push(entry.path());
            }
        }
    }
    paths.push(home.join("server/lock"));
    for path in paths {
        let Ok(raw) = fs::read_to_string(&path) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&raw) else { continue };
        let Some(port) = v["port"].as_u64() else { continue };
        if port != u64::from(addr.port) || normalize_host(v["host"].as_str()) != addr.host {
            continue;
        }
        if let Some(pid) = v["pid"].as_u64().and_then(|p| u32::try_from(p).ok()) {
            return Some(pid);
        }
    }
    None
}

/// Terminate a spawned child politely: SIGTERM first so the CLI runs its own
/// shutdown (relay/lock cleanup), then SIGKILL if it overstays. SIGKILL alone
/// can leave a stale `server/instances/*.json` behind, which then misleads the
/// next launch's discovery.
fn terminate_child(child: &mut Child, grace: Duration) {
    let pid = child.id();
    #[cfg(unix)]
    {
        let _ = no_console(Command::new("kill")).arg(pid.to_string()).spawn();
        let deadline = std::time::Instant::now() + grace;
        while std::time::Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(150));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Stop the exact daemon the shell is attached to, then start the installed
/// CLI again on the same address. Other instances are deliberately untouched.
fn restart_daemon_at(base: &str, token: &str, addr: &DaemonAddr) -> Result<Launch, String> {
    let home = kimi_home();
    let old_pid = instance_pid_for(&home, addr);

    let probe = DaemonAddr {
        host: addr.host.clone(),
        port: addr.port,
    };
    let launch = launch_at_addr(token, probe)?;
    shutdown_daemon(addr, token);
    let deadline = std::time::Instant::now() + STOP_WAIT;
    while std::time::Instant::now() < deadline && tcp_alive(&addr.host, addr.port) {
        thread::sleep(Duration::from_millis(150));
    }
    if tcp_alive(&addr.host, addr.port) {
        return Err("daemon 未在 8 秒内退出；未启动第二个实例".to_string());
    }

    let kimi = find_kimi().ok_or("找不到 kimi CLI，请先安装或更新 Kimi Code")?;
    let port_arg = addr.port.to_string();
    let log = fs::File::create(web_stderr_log()).map_err(|e| e.to_string())?;
    let mut child = no_console(Command::new(&kimi))
        .args([
            "web",
            "--host",
            addr.host.as_str(),
            "--port",
            port_arg.as_str(),
            "--no-open",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
        .map_err(|e| format!("启动新版 daemon 失败：{e}"))?;

    let start_deadline = std::time::Instant::now() + START_WAIT;
    while std::time::Instant::now() < start_deadline {
        if let Ok(Some(status)) = child.try_wait() {
            let log = fs::read_to_string(web_stderr_log()).unwrap_or_default();
            let tail = truncate_chars(log.trim(), 300);
            return Err(format!("新版 daemon 启动失败（{status}）：{tail}"));
        }
        // Registration with a NEW pid is the proof we want.
        if let Some(pid) = instance_pid_for(&home, addr) {
            if Some(pid) != old_pid && daemon_healthy(addr, token) {
                if let Ok(mut guard) = SPAWNED_SERVER.lock() {
                    *guard = Some(child);
                }
                return Ok(Launch {
                    base: base.to_string(),
                    token: token.to_string(),
                    url: launch.url,
                });
            }
        }
        thread::sleep(Duration::from_millis(250));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err("新版 daemon 启动超时，已停止未就绪的进程".to_string())
}

/// Re-attach to the daemon address the shell already knows about.
fn current_addr(base: &str) -> Option<DaemonAddr> {
    let host_part = base.trim_start_matches("http://");
    let (host, port) = host_part.rsplit_once(':')?;
    Some(DaemonAddr {
        host: host.to_string(),
        port: port.parse().ok()?,
    })
}

/// Dev-only: the official Kimi Code web bundle on disk —
/// `web/` next to the exe, `<exe>/../Resources/web`, or `<project>/web-dist`.
/// Release builds embed the bundle into the exe instead (see asset_source()).
#[cfg(debug_assertions)]
fn web_root() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let sibling = exe_dir.join("web");
            if sibling.is_dir() {
                return Some(sibling);
            }
            if let Some(res) = exe_dir.parent().map(|p| p.join("Resources/web")) {
                if res.is_dir() {
                    return Some(res);
                }
            }
        }
    }
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web-dist");
    dev.is_dir().then_some(dev)
}

/// web-dist/ embedded into the release exe — single-file distribution, no
/// sibling folder to locate at runtime.
#[cfg(not(debug_assertions))]
static EMBEDDED_WEB: include_dir::Dir<'static> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/web-dist");

/// Recursively collect files from an embedded dir — `Dir::files()` is NOT
/// recursive in include_dir 0.7 (subdirs live in `dirs()`), and without them
/// every nested asset fell back to index.html and white-screened the app.
#[cfg(not(debug_assertions))]
fn collect_embedded(dir: &'static include_dir::Dir<'static>, out: &mut Vec<(String, &'static [u8])>) {
    for f in dir.files() {
        out.push((f.path().to_string_lossy().into_owned(), f.contents()));
    }
    for d in dir.dirs() {
        collect_embedded(d, out);
    }
}

/// Asset source for the official UI: embedded into the exe in release.
#[cfg(not(debug_assertions))]
fn asset_source() -> Option<static_server::AssetSource> {
    let mut files = Vec::new();
    collect_embedded(&EMBEDDED_WEB, &mut files);
    Some(static_server::AssetSource::from_memory(files))
}

/// Asset source for the official UI: dev builds serve web-dist/ from disk,
/// so web rebuilds don't need a cargo rebuild.
#[cfg(debug_assertions)]
fn asset_source() -> Option<static_server::AssetSource> {
    web_root().map(static_server::AssetSource::Dir)
}

/// URL of the official UI served by our loopback static server, with the token
/// in the hash and the live daemon origin in `kimi_origin`. The latter is the
/// desktop handoff supported by the official bundle.
fn custom_ui_url(base: &str, token: &str) -> Option<Url> {
    let port = static_server::serve(asset_source()?, CUSTOM_UI_PORT).ok()?;
    let enc_base = base.replace(':', "%3A").replace('/', "%2F");
    let desktop_query = desktop_query();
    format!("http://127.0.0.1:{port}/{desktop_query}&kimi_origin={enc_base}#token={token}")
        .parse()
        .ok()
}

/// Pick a download destination under ~/Downloads without overwriting
/// existing files ("name (n).ext").
fn download_destination(url: &Url) -> PathBuf {
    let filename = url
        .path_segments()
        .and_then(|mut segs| segs.next_back())
        .filter(|s| !s.is_empty() && !s.contains(':'))
        .unwrap_or("download.bin");
    let dir = home_dir().join("Downloads");
    let mut path = dir.join(filename);
    for n in 1..100 {
        if !path.exists() {
            break;
        }
        let candidate = match filename.rsplit_once('.') {
            Some((stem, ext)) => format!("{stem} ({n}).{ext}"),
            None => format!("{filename} ({n})"),
        };
        path = dir.join(candidate);
    }
    path
}

/// Wire the standard behaviors onto a connection-webview builder. Each
/// connection gets its own view, so the label is per-connection.
fn main_webview_builder(label: &str) -> WebviewBuilder<tauri::Wry> {
    WebviewBuilder::new(label, WebviewUrl::App("index.html".into()))
        .initialization_script(INIT_SCRIPT)
        // External links (PRs, docs) go to the system browser.
        .on_new_window(|url, _features| {
            open_in_system_browser(&url);
            tauri::webview::NewWindowResponse::Deny
        })
        .on_download(|_webview, event| {
            match event {
                tauri::webview::DownloadEvent::Requested { url, destination } => {
                    *destination = download_destination(&url);
                }
                tauri::webview::DownloadEvent::Finished { success, .. } => {
                    if !success {
                        eprintln!("kimi-ui: 一次下载失败");
                    }
                }
                _ => {}
            }
            true
        })
}

#[tauri::command]
fn notify(app: tauri::AppHandle, title: String, body: String) {
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        eprintln!("kimi-ui: 通知发送失败：{e}");
    }
    let count = BADGE_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    set_dock_badge(&app, count);
}

#[tauri::command]
fn focus_window(window: tauri::Window) {
    let _ = window.unminimize();
    if let Err(e) = window.set_focus() {
        eprintln!("kimi-ui: 激活窗口失败：{e}");
    }
}

#[tauri::command]
fn toggle_maximize(window: tauri::Window) {
    let result = (|| -> tauri::Result<()> {
        // macOS: maximize() zooms to fill the screen, unmaximize() restores
        // the frame from before the zoom.
        if window.is_maximized()? {
            window.unmaximize()
        } else {
            window.maximize()
        }
    })();
    if let Err(e) = result {
        eprintln!("kimi-ui: 缩放窗口失败：{e}");
    }
}

// ---------------------------------------------------------------------------
// Scroll freeze ("静止" mode).
//
// During streaming the SPA re-pins the message list to the bottom on every
// delta, so users can't read older content mid-turn. Freezing installs an
// own-property `scrollTop` setter (and a no-op `scrollIntoView`) on the
// chat scroller: programmatic scrolls become no-ops, while native wheel /
// touch scrolling bypasses the setter and keeps working. Zero jitter.
// ---------------------------------------------------------------------------

const FREEZE_JS: &str = r#"
(function () {
  if (window.__kimiScrollFreeze && window.__kimiScrollFreeze.on) return;
  window.__kimiScrollFreeze = { on: true, el: null, timer: null };
  function findScroller() {
    var best = null, bestArea = 0;
    var els = document.querySelectorAll('div,main,section');
    for (var i = 0; i < els.length; i++) {
      var e = els[i];
      if (e.scrollHeight - e.clientHeight <= 60) continue;
      var s = getComputedStyle(e);
      if (s.overflowY !== 'auto' && s.overflowY !== 'scroll') continue;
      var area = e.clientWidth * e.clientHeight;
      if (area > bestArea) { bestArea = area; best = e; }
    }
    return best;
  }
  var desc = Object.getOwnPropertyDescriptor(Element.prototype, 'scrollTop');
  function releaseEl(el) {
    try { delete el.scrollTop; } catch (e) {}
    if (el.__kimiFrozenSIV) { el.scrollIntoView = el.__kimiFrozenSIV; delete el.__kimiFrozenSIV; }
  }
  function apply() {
    var el = findScroller();
    if (!el || el === window.__kimiScrollFreeze.el) return;
    if (window.__kimiScrollFreeze.el) releaseEl(window.__kimiScrollFreeze.el);
    window.__kimiScrollFreeze.el = el;
    Object.defineProperty(el, 'scrollTop', {
      configurable: true,
      get: function () { return desc.get.call(this); },
      set: function () { /* frozen: programmatic scrolls are no-ops */ }
    });
    el.__kimiFrozenSIV = el.scrollIntoView;
    el.scrollIntoView = function () {};
  }
  window.__kimiScrollFreeze.release = function () {
    if (window.__kimiScrollFreeze.timer) { clearInterval(window.__kimiScrollFreeze.timer); window.__kimiScrollFreeze.timer = null; }
    if (window.__kimiScrollFreeze.el) releaseEl(window.__kimiScrollFreeze.el);
    window.__kimiScrollFreeze = { on: false, el: null, timer: null };
  };
  apply();
  window.__kimiScrollFreeze.timer = setInterval(apply, 3000);
})();
"#;

const UNFREEZE_JS: &str = r#"
if (window.__kimiScrollFreeze && window.__kimiScrollFreeze.release) {
  window.__kimiScrollFreeze.release();
}
"#;

/// Toggle the scroll freeze on the main webview ("静止" mode).
#[tauri::command]
fn set_scroll_freeze(app: tauri::AppHandle, frozen: bool) {
    if let Some(wv) = active_view(&app) {
        let _ = wv.eval(if frozen { FREEZE_JS } else { UNFREEZE_JS });
    }
}

/// Toggle Safari Web Inspector on the main webview (memory/DOM profiling).
#[tauri::command]
fn toggle_devtools(app: tauri::AppHandle) {
    if let Some(wv) = active_view(&app) {
        if wv.is_devtools_open() {
            wv.close_devtools();
        } else {
            wv.open_devtools();
        }
        // The docked inspector changes the webview's frame to make room for
        // itself, and on close the frame stays at FULL window height — the
        // bottom of the page slides under the status strip. Re-assert our
        // layout immediately and once more after the native frame change
        // settles (it lands asynchronously).
        layout_strip(&app);
        let app2 = app.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(250));
            let app3 = app2.clone();
            let _ = app2.run_on_main_thread(move || layout_strip(&app3));
        });
    }
}

/// Status-bar data for the ACTIVE connection, fetched server-side.
///
/// The status page cannot fetch the daemon itself: its own document origin is
/// `tauri://localhost`, which the daemon's origin check rejects for anything
/// not same-origin (a remote daemon is a different origin by definition).
/// Proxying through Rust also keeps the token out of the webview.
/// Returns the payloads the page needs: meta, the session list, and the chosen
/// session's status.
#[tauri::command]
fn daemon_snapshot(app: tauri::AppHandle) -> Value {
    let Some(state) = daemon_state(&app) else {
        return serde_json::json!({ "state": "down", "error": "daemon 尚未就绪" });
    };
    let (base, token) = (state.base.clone(), state.token.clone());
    let meta = match http_get_json(&base, "/api/v1/meta", &token) {
        Ok(m) => m,
        Err(e) => return serde_json::json!({ "state": "down", "error": e }),
    };
    let version = meta["data"]["server_version"].clone();
    let sessions = match http_get_json(&base, "/api/v1/sessions?page_size=50", &token) {
        Ok(v) => v,
        Err(e) => return serde_json::json!({ "state": "down", "error": e, "version": version }),
    };
    let Some(items) = sessions["data"]["items"].as_array() else {
        return serde_json::json!({ "state": "broken" });
    };
    // Same pick rule the page used: first busy session, else first live one.
    let pick = items
        .iter()
        .find(|s| s["busy"].as_bool().unwrap_or(false))
        .or_else(|| items.iter().find(|s| !s["archived"].as_bool().unwrap_or(false)));
    let Some(pick) = pick else {
        return serde_json::json!({
            "state": "ok",
            "version": version,
            "session": Value::Null,
            "status": Value::Null,
        });
    };
    let id = pick["id"].as_str().unwrap_or_default();
    let status = http_get_json(&base, &format!("/api/v1/sessions/{id}/status"), &token)
        .unwrap_or(Value::Null);
    serde_json::json!({
        "state": "ok",
        "version": version,
        "session": pick,
        "status": status["data"].clone(),
    })
}

/// The status webview asks for daemon connection details once it boots.
#[tauri::command]
fn daemon_info(state: tauri::State<'_, SharedDaemon>) -> Result<Value, String> {
    let guard = state.lock().map_err(|e| e.to_string())?;
    let s = guard.as_ref().ok_or_else(|| "daemon 尚未就绪".to_string())?;
    Ok(serde_json::json!({ "base": s.base, "token": s.token }))
}

/// Restart the local daemon on its current port with the installed CLI.
/// Only meaningful for the shell's own loopback daemon: remote daemons are
/// someone else's machine, and a non-loopback bind disables `/shutdown`.
#[tauri::command]
fn restart_daemon(app: tauri::AppHandle) -> Result<Value, String> {
    let launch = daemon_state(&app).ok_or("daemon 尚未连接")?;
    let addr = current_addr(&launch.base).ok_or("daemon 地址无法解析")?;
    if !is_loopback_host(&addr.host) {
        return Err("只能重启本地 daemon（当前是远程连接）".to_string());
    }
    let next = restart_daemon_at(&launch.base, &launch.token, &addr)?;
    if let Some(state) = app.try_state::<SharedDaemon>() {
        if let Ok(mut guard) = state.lock() {
            *guard = Some(DaemonState {
                base: next.base.clone(),
                token: next.token.clone(),
            });
        }
    }
    // Point the active view at the fresh daemon.
    let url = custom_ui_url(&next.base, &next.token).unwrap_or(next.url.clone());
    if let Some(wv) = active_view(&app) {
        let _ = wv.navigate(url);
    }
    Ok(serde_json::json!({ "ok": true, "base": next.base }))
}

// ---------------------------------------------------------------------------
// Connection commands (the tab bar's backend).
// ---------------------------------------------------------------------------

/// Every connection plus which one is active.
#[tauri::command]
fn list_connections(app: tauri::AppHandle) -> Value {
    let conns = app
        .try_state::<SharedConnections>()
        .and_then(|s| s.lock().ok().map(|g| {
            g.iter()
                .map(|c| {
                    serde_json::json!({
                        "id": c.id,
                        "name": c.name,
                        "kind": match c.kind { ConnKind::Local => "local", ConnKind::Remote => "remote" },
                        "base": c.base,
                    })
                })
                .collect::<Vec<_>>()
        }))
        .unwrap_or_default();
    let active = app
        .try_state::<SharedActive>()
        .and_then(|s| s.lock().ok().and_then(|g| g.clone()));
    serde_json::json!({ "connections": conns, "active": active })
}

/// Reachability of a connection's daemon, probed server-side so the page never
/// needs cross-origin fetch. Also returns the daemon's reported version.
#[tauri::command]
fn connection_status(app: tauri::AppHandle, id: String) -> Value {
    let Some(conn) = find_connection(&app, &id) else {
        return serde_json::json!({ "ok": false, "error": "连接不存在" });
    };
    let Some(addr) = current_addr(&conn.base) else {
        return serde_json::json!({ "ok": false, "error": "地址无效" });
    };
    match daemon_meta(&addr, &conn.token) {
        Ok(meta) => serde_json::json!({
            "ok": true,
            "version": meta["data"]["server_version"].as_str().unwrap_or(""),
        }),
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    }
}

/// Add a remote daemon and open a tab for it.
#[tauri::command]
fn add_remote_connection(
    app: tauri::AppHandle,
    name: String,
    address: String,
    token: String,
) -> Result<Value, String> {
    let conn = parse_remote_connection(&name, &address, &token)?;
    // Probe before accepting: a bad address or token should fail here with a
    // clear message rather than as a blank tab.
    let addr = current_addr(&conn.base).ok_or("地址无效")?;
    daemon_meta(&addr, &conn.token)
        .map_err(|e| format!("无法连接 {}({e})", conn.base))?;

    if let Some(state) = app.try_state::<SharedConnections>() {
        let mut guard = state.lock().map_err(|e| e.to_string())?;
        if guard.iter().any(|c| c.id == conn.id) {
            return Err("该地址已添加".to_string());
        }
        guard.push(conn.clone());
        save_connections(&guard);
    }
    open_connection(&app, &conn)?;
    activate_connection(&app, &conn.id);
    Ok(serde_json::json!({ "ok": true, "id": conn.id }))
}

/// Close a tab. The local connection cannot be removed.
#[tauri::command]
fn remove_connection(app: tauri::AppHandle, id: String) -> Result<Value, String> {
    if id == "local" {
        return Err("本地连接不能删除".to_string());
    }
    let was_active = app
        .try_state::<SharedActive>()
        .and_then(|s| s.lock().ok().and_then(|g| g.clone()))
        .as_deref()
        == Some(id.as_str());
    if let Some(wv) = app.get_webview(&view_label(&id)) {
        let _ = wv.close();
    }
    if let Some(state) = app.try_state::<SharedConnections>() {
        let mut guard = state.lock().map_err(|e| e.to_string())?;
        guard.retain(|c| c.id != id);
        save_connections(&guard);
    }
    if was_active {
        activate_connection(&app, "local");
    } else {
        refresh_tabs(&app);
    }
    Ok(serde_json::json!({ "ok": true }))
}

/// Rename a connection's tab. Handy when several addresses are configured.
#[tauri::command]
fn rename_connection(app: tauri::AppHandle, id: String, name: String) -> Result<Value, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("名称不能为空".to_string());
    }
    if let Some(state) = app.try_state::<SharedConnections>() {
        let mut guard = state.lock().map_err(|e| e.to_string())?;
        let Some(conn) = guard.iter_mut().find(|c| c.id == id) else {
            return Err("连接不存在".to_string());
        };
        conn.name = trimmed.to_string();
        save_connections(&guard);
    } else {
        return Err("连接不存在".to_string());
    }
    refresh_tabs(&app);
    Ok(serde_json::json!({ "ok": true }))
}

/// Switch the visible tab.
#[tauri::command]
fn switch_connection(app: tauri::AppHandle, id: String) -> Result<Value, String> {
    if find_connection(&app, &id).is_none() {
        return Err("连接不存在".to_string());
    }
    activate_connection(&app, &id);
    Ok(serde_json::json!({ "ok": true }))
}

/// True for hosts that mean "this machine".
fn is_loopback_host(host: &str) -> bool {
    host == "127.0.0.1" || host == "localhost" || host == "::1" || host == "[::1]"
}

/// Open/close an overlay card in the status webview ("none" collapses;
/// "usage" | "update" | "remote" all expand to OVERLAY_H — the card layout
/// itself lives entirely in the status page).
#[tauri::command]
fn set_overlay(app: tauri::AppHandle, mode: String) {
    let open = !matches!(mode.as_str(), "none" | "");
    OVERLAY_OPEN.store(open, Ordering::Relaxed);
    layout_strip(&app);
}

/// Grow/shrink the tab bar so its add-connection dialog is fully visible.
/// The dialog is HTML inside the tab-bar webview, so the webview itself has to
/// be tall enough — it cannot overflow its own bounds.
#[tauri::command]
fn set_tabbar_expanded(app: tauri::AppHandle, expanded: bool) {
    TABBAR_EXPANDED.store(expanded, Ordering::Relaxed);
    layout_strip(&app);
}

/// Recompute every view's bounds. The tab bar and status strip are pinned to
/// the window edges; connection views share the space between them and only
/// the active one is visible (hidden views keep their page alive).
fn layout_strip(app: &tauri::AppHandle) {
    let Some(window) = app.get_window("main") else { return };
    let (Ok(size), Ok(scale)) = (window.inner_size(), window.scale_factor()) else { return };
    let w = size.width as f64 / scale;
    let h = size.height as f64 / scale;

    let tabbar_expanded = TABBAR_EXPANDED.load(Ordering::Relaxed);
    let tabbar_h = if tabbar_expanded { TABBAR_EXPANDED_H } else { TABBAR_H };
    let content_y = tabbar_h;
    let content_h = (h - tabbar_h - STRIP).max(200.0);

    if let Some(tabbar) = app.get_webview("tabbar") {
        let _ = tabbar.set_position(LogicalPosition::new(0.0, 0.0));
        let _ = tabbar.set_size(LogicalSize::new(w, tabbar_h));
    }
    // Every connection view gets the full content rect; visibility decides
    // which one is on screen, so switching needs no resize.
    if let Ok(conns) = app.state::<SharedConnections>().lock() {
        for conn in conns.iter() {
            if let Some(wv) = app.get_webview(&view_label(&conn.id)) {
                let _ = wv.set_position(LogicalPosition::new(0.0, content_y));
                let _ = wv.set_size(LogicalSize::new(w, content_h));
            }
        }
    }
    let overlay = OVERLAY_OPEN.load(Ordering::Relaxed);
    let status_h = if overlay { OVERLAY_H } else { STRIP };
    if let Some(status_wv) = app.get_webview("status") {
        let _ = status_wv.set_position(LogicalPosition::new(0.0, h - status_h));
        let _ = status_wv.set_size(LogicalSize::new(w, status_h));
    }
}

/// The webview showing the active connection's official UI, if any.
fn active_view(app: &tauri::AppHandle) -> Option<tauri::Webview> {
    let id = app
        .try_state::<SharedActive>()?
        .lock()
        .ok()
        .and_then(|g| g.clone())?;
    app.get_webview(&view_label(&id))
}

/// The connection with `id`, cloned out of state.
fn find_connection(app: &tauri::AppHandle, id: &str) -> Option<Connection> {
    app.try_state::<SharedConnections>()?
        .lock()
        .ok()?
        .iter()
        .find(|c| c.id == id)
        .cloned()
}

/// Create the webview for a connection if it does not exist yet. The view is
/// created hidden; `activate_connection` decides what is on screen.
fn ensure_view(app: &tauri::AppHandle, conn: &Connection) -> Result<tauri::Webview, String> {
    let label = view_label(&conn.id);
    if let Some(wv) = app.get_webview(&label) {
        return Ok(wv);
    }
    let window = app.get_window("main").ok_or("主窗口不存在")?;
    let (Ok(size), Ok(scale)) = (window.inner_size(), window.scale_factor()) else {
        return Err("无法读取窗口尺寸".to_string());
    };
    let (w, h) = (size.width as f64 / scale, size.height as f64 / scale);
    let content_h = (h - TABBAR_H - STRIP).max(200.0);

    let mut builder = main_webview_builder(&label);
    // Remote daemons on plain http over a LAN need ATS to allow local
    // networking (Info.plist grants this) and their document origin must be
    // covered by a runtime capability for script IPC (see grant_remote_capability).
    if conn.kind == ConnKind::Remote {
        builder = builder.incognito(false);
    }
    window
        .add_child(
            builder,
            LogicalPosition::new(0.0, TABBAR_H),
            LogicalSize::new(w, content_h),
        )
        .map_err(|e| format!("创建视图失败：{e}"))
}

/// Point a connection's view at its URL and make sure the page knows which
/// daemon it is talking to.
fn open_connection(app: &tauri::AppHandle, conn: &Connection) -> Result<(), String> {
    if conn.kind == ConnKind::Remote {
        grant_remote_capability(app, &conn.base)?;
    }
    // Register (or refresh) the connection so the tab bar and every lookup by
    // id see it. The local entry is re-derived on each launch, so this
    // replaces rather than duplicates it.
    if let Some(state) = app.try_state::<SharedConnections>() {
        let mut guard = state.lock().map_err(|e| e.to_string())?;
        match guard.iter_mut().find(|c| c.id == conn.id) {
            Some(existing) => *existing = conn.clone(),
            None => guard.push(conn.clone()),
        }
    }
    let wv = ensure_view(app, conn)?;
    // Local daemons may not have a token yet (error page path).
    if conn.base.is_empty() {
        return Ok(());
    }
    let url = connection_url(conn);
    wv.navigate(url).map_err(|e| format!("导航失败：{e}"))
}

/// Activate a connection: show its view, update the active daemon, refresh the
/// window title, sessions menu and tab bar.
fn activate_connection(app: &tauri::AppHandle, id: &str) {
    if let Some(state) = app.try_state::<SharedActive>() {
        if let Ok(mut guard) = state.lock() {
            *guard = Some(id.to_string());
        }
    }
    if let Some(conn) = find_connection(app, id) {
        if let Some(state) = app.try_state::<SharedDaemon>() {
            if let Ok(mut guard) = state.lock() {
                *guard = Some(DaemonState {
                    base: conn.base.clone(),
                    token: conn.token.clone(),
                });
            }
        }
    }
    sync_view_visibility(app);
    layout_strip(app);
    let app2 = app.clone();
    thread::spawn(move || sync_sessions_and_title(&app2));
    refresh_tabs(app);
    // The status page follows the active connection too.
    if let Some(status) = app.get_webview("status") {
        let _ = status.eval("window.__kimiConnChanged && window.__kimiConnChanged()");
    }
}

/// Rebuild the tab bar's contents.
fn refresh_tabs(app: &tauri::AppHandle) {
    let Some(tabs) = app.get_webview("tabbar") else { return };
    let payload = serde_json::json!({
        "connections": app
            .try_state::<SharedConnections>()
            .and_then(|s| s.lock().ok().map(|g| {
                g.iter()
                    .map(|c| serde_json::json!({
                        "id": c.id,
                        "name": c.name,
                        "kind": match c.kind { ConnKind::Local => "local", ConnKind::Remote => "remote" },
                    }))
                    .collect::<Vec<_>>()
            }))
            .unwrap_or_default(),
        "active": app
            .try_state::<SharedActive>()
            .and_then(|s| s.lock().ok().and_then(|g| g.clone())),
    });
    let _ = tabs.eval(&format!(
        "window.__kimiTabs && window.__kimiTabs({payload})"
    ));
}

/// Register a runtime capability so a remote daemon's page may call the
/// shell's desktop-bridge commands. Capabilities are matched by document
/// origin, and the built-in one only covers loopback — without this, every
/// `invoke` from a LAN-hosted page is silently denied. (The `dynamic-acl`
/// tauri feature, enabled in Cargo.toml, provides `add_capability`.)
fn grant_remote_capability(app: &tauri::AppHandle, base: &str) -> Result<(), String> {
    use tauri::ipc::CapabilityBuilder;
    use tauri::Manager as _;
    let origin = base.trim_end_matches('/');
    let identifier = format!("remote-{}", origin.replace([':', '/', '.'], "-"));
    let capability = CapabilityBuilder::new(identifier)
        .remote(origin.to_string())
        .window("main")
        .permission("core:window:allow-start-dragging")
        .permission("core:window:allow-internal-toggle-maximize")
        .permission("notification:default")
        .permission("allow-notify")
        .permission("allow-focus-window")
        .permission("allow-toggle-maximize")
        .permission("allow-set-active-session");
    app.add_capability(capability)
        .map_err(|e| format!("授权远程页面失败：{e}"))
}


/// Show exactly one connection view and hide the rest.
fn sync_view_visibility(app: &tauri::AppHandle) {
    let active = app
        .try_state::<SharedActive>()
        .and_then(|s| s.lock().ok().and_then(|g| g.clone()));
    let conns: Vec<String> = app
        .try_state::<SharedConnections>()
        .and_then(|s| s.lock().ok().map(|g| g.iter().map(|c| c.id.clone()).collect()))
        .unwrap_or_default();
    for id in conns {
        if let Some(wv) = app.get_webview(&view_label(&id)) {
            if Some(&id) == active.as_ref() {
                let _ = wv.show();
                let _ = wv.set_focus();
            } else {
                let _ = wv.hide();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Update check + in-app auto-update.
//
// Two layers, deliberately:
//   - THIS module answers "is there a newer release, and what does it say?"
//     for the status card's badge, notes and "前往下载" link. It reads the
//     GitHub releases API (gh when available for auth/rate limits, anonymous
//     curl otherwise) purely for display.
//   - The actual download/verify/install is the official
//     `tauri-plugin-updater`'s job (see auto_update_pipeline): it reads the
//     signed `latest.json` manifest configured in tauri.conf.json, verifies
//     the minisign signature, installs the bundle and relaunches. That is
//     stricter than the shell-out this replaced, which only compared a
//     sha256 digest and then swapped the .app by hand.
//
// Test hooks: KIMI_UI_FORCE_UPDATE=1 always reports has_update;
// KIMI_UI_UPDATE_TAG=<tag> targets a specific release instead of latest.
// ---------------------------------------------------------------------------

/// Latest-release info exposed to the status page.
#[derive(Clone, serde::Serialize)]
struct UpdateInfo {
    latest: String,
    url: String,
    has_update: bool,
    /// Release notes (markdown), rendered by the status page's update card.
    notes: String,
    /// macOS zip asset, used only for the "前往下载" fallback link.
    asset_url: String,
    asset_size: u64,
    /// "sha256:<hex>" from the release API (informational; the updater plugin
    /// verifies minisign signatures instead).
    asset_digest: String,
    /// Auto-update only exists on macOS.
    can_auto_update: bool,
}

static UPDATE_INFO: Mutex<Option<UpdateInfo>> = Mutex::new(None);

/// Locate an executable: PATH first, then Homebrew spots (Finder/launchd
/// environments get a minimal PATH).
fn find_executable(name: &str) -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    [
        PathBuf::from(format!("/opt/homebrew/bin/{name}")),
        PathBuf::from(format!("/usr/local/bin/{name}")),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

fn version_newer(latest: &str, current: &str) -> bool {
    fn parts(v: &str) -> Vec<u32> {
        v.split('.').filter_map(|p| p.parse().ok()).collect()
    }
    parts(latest) > parts(current)
}

/// Session the SPA is currently viewing, from its URL route.
static ACTIVE_SESSION: Mutex<Option<String>> = Mutex::new(None);
/// Ordered (id, title) pairs of recent non-archived sessions (API order).
static SESSION_TITLES: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// How many recent sessions the 会话 menu shows.
const MENU_SESSIONS: usize = 8;

/// Last session list the menu was built from; skip rebuilds when unchanged.
static MENU_BUILT: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// The macOS release asset this updater downloads.
const MACOS_ASSET: &str = "kimi-ui-macos-arm64.zip";

/// `UpdateInfo` from a releases API JSON body; None when the shape drifted.
fn parse_release_json(raw: &str) -> Option<UpdateInfo> {
    let json: Value = serde_json::from_str(raw).ok()?;
    let latest = json["tag_name"].as_str()?.trim_start_matches('v').to_string();
    let url = json["html_url"].as_str()?.to_string();
    let notes = json["body"].as_str().unwrap_or("").trim().to_string();
    let asset = json["assets"]
        .as_array()?
        .iter()
        .find(|a| a["name"].as_str() == Some(MACOS_ASSET))?;
    let forced = std::env::var("KIMI_UI_FORCE_UPDATE").map_or(false, |v| v == "1");
    Some(UpdateInfo {
        has_update: forced || version_newer(&latest, env!("CARGO_PKG_VERSION")),
        latest,
        url,
        notes,
        asset_url: asset["browser_download_url"].as_str().unwrap_or("").to_string(),
        asset_size: asset["size"].as_u64().unwrap_or(0),
        asset_digest: asset["digest"].as_str().unwrap_or("").to_string(),
        can_auto_update: cfg!(target_os = "macos"),
    })
}

/// Fetch the latest-release JSON: gh first (auth), anonymous curl fallback.
fn fetch_latest_release() -> Option<UpdateInfo> {
    let endpoint = match std::env::var("KIMI_UI_UPDATE_TAG") {
        Ok(tag) if !tag.is_empty() => format!("releases/tags/{tag}"),
        _ => "releases/latest".to_string(),
    };
    let api = format!("repos/liujunGH/kimi-ui/{endpoint}");
    if let Some(gh) = find_executable("gh") {
        if let Ok(out) = no_console(Command::new(gh)).args(["api", &api]).output() {
            if out.status.success() {
                if let Some(info) = parse_release_json(&String::from_utf8_lossy(&out.stdout)) {
                    return Some(info);
                }
            }
        }
    }
    let curl = find_executable("curl")?;
    let url = format!("https://api.github.com/liujunGH/kimi-ui/{endpoint}");
    let out = no_console(Command::new(curl))
        .args(["-sL", "--fail", "--max-time", "20", &url])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_release_json(&String::from_utf8_lossy(&out.stdout))
}

fn start_update_check() {
    thread::spawn(|| {
        let info = fetch_latest_release();
        if let Ok(mut guard) = UPDATE_INFO.lock() {
            *guard = info;
        }
    });
}

#[tauri::command]
fn update_info() -> Value {
    let info = UPDATE_INFO.lock().ok().and_then(|g| g.clone());
    info.map(|i| serde_json::to_value(i).unwrap_or(Value::Null))
        .unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------
// Auto-update pipeline: download -> verify -> stage -> swap-and-relaunch.
// ---------------------------------------------------------------------------

/// Pipeline state polled by the status page.
#[derive(Clone, serde::Serialize)]
struct UpdateProgress {
    phase: &'static str, // idle | downloading | verifying | ready | error
    pct: u32,            // 0-100 while downloading
    message: String,
}

static UPDATE_PROGRESS: Mutex<UpdateProgress> = Mutex::new(UpdateProgress {
    phase: "idle",
    pct: 0,
    message: String::new(),
});
static AUTO_UPDATE_RUNNING: AtomicBool = AtomicBool::new(false);

fn set_progress(phase: &'static str, pct: u32, message: String) {
    if let Ok(mut guard) = UPDATE_PROGRESS.lock() {
        *guard = UpdateProgress { phase, pct, message };
    }
}

/// Plugin-driven update pipeline.
///
/// The heavy lifting (signed manifest fetch, download, minisign verification,
/// bundle install) is the official `tauri-plugin-updater`'s job — it is
/// stricter than a hand-rolled shell-out (signature check instead of only a
/// digest) and it resolves the platform artifact from `latest.json`. We keep
/// the status card's contract by translating its progress events into
/// `UPDATE_PROGRESS`.
///
/// Note there is no separate "install" step any more: the plugin verifies and
/// installs in one call, so `start` runs the whole thing and then relaunches.
#[cfg(desktop)]
fn auto_update_pipeline(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt as _;

    // Deliberately keeps the system HTTP proxy: with a VPN/Clash setup the
    // proxy is what makes github.com reachable. (A local http test endpoint
    // would be swallowed by it — smoke tests set NO_PROXY for that.)
    let updater = app
        .updater()
        .map_err(|e| format!("更新器不可用：{e}"))?;

    let update = tauri::async_runtime::block_on(updater.check())
        .map_err(|e| format!("检查更新失败：{e:?}"))?
        .ok_or("已是最新版本".to_string())?;

    let version = update.version.clone();
    set_progress("downloading", 0, String::new());
    tauri::async_runtime::block_on(update.download_and_install(
        |downloaded, total| {
            let pct = match total {
                Some(t) if t > 0 => ((downloaded as f64 / t as f64) * 100.0).min(99.0) as u32,
                _ => 0,
            };
            set_progress("downloading", pct, String::new());
        },
        || {
            // Emitted once the payload is verified and about to be installed.
            set_progress("verifying", 100, "校验中…".to_string());
        },
    ))
    .map_err(|e| format!("安装失败：{e}"))?;

    set_progress("ready", 100, format!("v{version} 已就绪"));
    Ok(())
}

#[cfg(not(desktop))]
fn auto_update_pipeline(_app: &tauri::AppHandle) -> Result<(), String> {
    Err("自动更新目前仅支持桌面平台".to_string())
}

/// Status-bar entry point: "start" | "install" | "status".
///
/// "start" performs the whole download+verify+install (the plugin does those
/// in one call); "install" simply relaunches, which is what the card's
/// "安装并重启" button means once the payload is staged.
#[tauri::command]
fn auto_update(app: tauri::AppHandle, action: String) -> Value {
    match action.as_str() {
        "start" => {
            if !AUTO_UPDATE_RUNNING.swap(true, Ordering::Relaxed) {
                let app2 = app.clone();
                thread::spawn(move || {
                    if let Err(e) = auto_update_pipeline(&app2) {
                        eprintln!("kimi-ui: 自动更新失败：{e}");
                        set_progress("error", 0, e);
                    }
                    AUTO_UPDATE_RUNNING.store(false, Ordering::Relaxed);
                });
            }
            serde_json::json!({})
        }
        "install" => {
            // The plugin already installed the verified payload; restart into
            // it (re-execs the replaced bundle).
            app.restart();
        }
        _ => {
            let snapshot = UPDATE_PROGRESS.lock().map(|g| g.clone()).unwrap_or(UpdateProgress {
                phase: "idle",
                pct: 0,
                message: String::new(),
            });
            serde_json::to_value(snapshot).unwrap_or(Value::Null)
        }
    }
}

fn build_app_menu(
    app: &tauri::AppHandle,
    sessions: &[(String, String)],
) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};

    let app_menu = Submenu::with_items(app, "Kimi Code", true, &[
        &PredefinedMenuItem::about(app, Some("关于 Kimi Code"), None)?,
        &PredefinedMenuItem::separator(app)?,
        &PredefinedMenuItem::services(app, Some("服务"))?,
        &PredefinedMenuItem::separator(app)?,
        &PredefinedMenuItem::hide(app, Some("隐藏 Kimi Code"))?,
        &PredefinedMenuItem::hide_others(app, Some("隐藏其他"))?,
        &PredefinedMenuItem::show_all(app, Some("显示全部"))?,
        &PredefinedMenuItem::separator(app)?,
        &PredefinedMenuItem::quit(app, Some("退出 Kimi Code"))?,
    ])?;

    let file_menu = Submenu::with_items(app, "文件", true, &[
        &MenuItem::with_id(app, "new-session", "新建会话", true, Some("CmdOrCtrl+N"))?,
        &PredefinedMenuItem::separator(app)?,
        &PredefinedMenuItem::close_window(app, Some("关闭窗口"))?,
    ])?;

    let edit_menu = Submenu::with_items(app, "编辑", true, &[
        &PredefinedMenuItem::undo(app, Some("撤销"))?,
        &PredefinedMenuItem::redo(app, Some("重做"))?,
        &PredefinedMenuItem::separator(app)?,
        &PredefinedMenuItem::cut(app, Some("剪切"))?,
        &PredefinedMenuItem::copy(app, Some("复制"))?,
        &PredefinedMenuItem::paste(app, Some("粘贴"))?,
        &PredefinedMenuItem::select_all(app, Some("全选"))?,
    ])?;

    let sessions_menu = Submenu::new(app, "会话", true)?;
    let recents: Vec<MenuItem<tauri::Wry>> = sessions
        .iter()
        .take(MENU_SESSIONS)
        .map(|(id, title)| {
            MenuItem::with_id(
                app,
                format!("open-session:{id}"),
                truncate_chars(title, 40),
                true,
                None::<&str>,
            )
        })
        .collect::<tauri::Result<_>>()?;
    if recents.is_empty() {
        let empty = MenuItem::with_id(app, "sessions-empty", "（暂无会话）", false, None::<&str>)?;
        sessions_menu.append(&empty)?;
    } else {
        let refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> =
            recents.iter().map(|i| i as &dyn tauri::menu::IsMenuItem<tauri::Wry>).collect();
        sessions_menu.append_items(&refs)?;
    }

    let window_menu = Submenu::with_items(app, "窗口", true, &[
        &PredefinedMenuItem::minimize(app, Some("最小化"))?,
        &MenuItem::with_id(app, "zoom", "缩放", true, None::<&str>)?,
        &PredefinedMenuItem::separator(app)?,
        &PredefinedMenuItem::bring_all_to_front(app, Some("全部置前"))?,
    ])?;

    let help_menu = Submenu::with_items(app, "帮助", true, &[
        &MenuItem::with_id(app, "docs", "官方文档", true, None::<&str>)?,
        &MenuItem::with_id(app, "check-update", "检查更新", true, None::<&str>)?,
    ])?;

    let menu = Menu::with_items(app, &[
        &app_menu,
        &file_menu,
        &edit_menu,
        &sessions_menu,
        &window_menu,
        &help_menu,
    ])?;
    app.set_menu(menu)?;
    Ok(())
}


fn daemon_state(app: &tauri::AppHandle) -> Option<DaemonState> {
    app.try_state::<SharedDaemon>()
        .and_then(|s| s.lock().ok().and_then(|g| g.clone()))
}

fn eval_main_navigation(app: &tauri::AppHandle, path: &str) {
    if let Some(wv) = app.get_webview("main") {
        let path_json = serde_json::json!(path).to_string();
        let _ = wv.eval(&format!(
            "try{{history.pushState({{}},'',{path_json})}}catch(e){{location.assign({path_json})}}"
        ));
    }
}

fn fetch_sessions(daemon: &DaemonState) -> Result<Vec<(String, String)>, String> {
    let v = http_get_json(&daemon.base, "/api/v1/sessions?page_size=50", &daemon.token)?;
    parse_sessions(&v).ok_or_else(|| "会话列表结构可能已变化".to_string())
}

fn handle_menu_event(app: &tauri::AppHandle, event: &tauri::menu::MenuEvent) {
    let id = event.id().0.as_str();
    match id {
        "new-session" => {
            let _ = app.get_window("main").map(|w| w.set_focus());
            eval_main_navigation(app, "/");
        }
        "zoom" => {
            if let Some(window) = app.get_window("main") {
                let _ = (|| -> tauri::Result<()> {
                    if window.is_maximized()? {
                        window.unmaximize()
                    } else {
                        window.maximize()
                    }
                })();
            }
        }
        "docs" => open_in_system_browser(&"https://www.kimi.com/code/docs/en/".parse().unwrap()),
        "check-update" => {
            let url = UPDATE_INFO
                .lock()
                .ok()
                .and_then(|g| g.as_ref().map(|i| i.url.clone()))
                .unwrap_or_else(|| "https://github.com/liujunGH/kimi-ui/releases/latest".into());
            if let Ok(u) = url.parse::<Url>() {
                open_in_system_browser(&u);
            }
        }
        _ if id.starts_with("open-session:") => {
            let sid = &id["open-session:".len()..];
            let _ = app.get_window("main").map(|w| w.set_focus());
            let path_json = serde_json::json!(format!("/sessions/{sid}")).to_string();
            if let Some(wv) = app.get_webview("main") {
                let _ = wv.eval(&format!(
                    "try{{history.pushState({{}},'',{path_json})}}catch(e){{location.assign({path_json})}}"
                ));
            }
        }
        _ => {}
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .invoke_handler(tauri::generate_handler![
            notify,
            focus_window,
            toggle_maximize,
            daemon_info,
            set_overlay,
            plan_usage,
            set_scroll_freeze,
            toggle_devtools,
            update_info,
            open_url,
            set_active_session,
            remote_control,
            auto_update,
            restart_daemon,
            list_connections,
            connection_status,
            add_remote_connection,
            remove_connection,
            switch_connection,
            daemon_snapshot,
            rename_connection,
            set_tabbar_expanded
        ])
        .manage(SharedDaemon::new(None))
        .manage(SharedConnections::new(Vec::new()))
        .manage(SharedActive::new(None))
        .setup(|app| {
            let window_builder = WindowBuilder::new(app, "main")
                .title("Kimi Code")
                .inner_size(1280.0, 840.0)
                .min_inner_size(860.0, 560.0);
            #[cfg(target_os = "macos")]
            let window_builder = window_builder
                .title_bar_style(tauri::TitleBarStyle::Overlay)
                .hidden_title(true);
            let window = window_builder.build()?;

            let size = window.inner_size()?;
            let scale = window.scale_factor()?;
            let (w, h) = (size.width as f64 / scale, size.height as f64 / scale);

            // Tab bar: the shell's connection switcher, flush with the top so
            // the macOS traffic lights sit on it (Overlay title bar).
            let _tabbar_wv = window.add_child(
                WebviewBuilder::new("tabbar", WebviewUrl::App("tabs.html".into()))
                    .transparent(true),
                LogicalPosition::new(0.0, 0.0),
                LogicalSize::new(w, TABBAR_H),
            )?;
            // Status webview: the shell's own UI surface (transparent so the
            // overlay cards float over the connection views).
            let _status_wv = window.add_child(
                WebviewBuilder::new("status", WebviewUrl::App("status.html".into()))
                    .transparent(true),
                LogicalPosition::new(0.0, h - STRIP),
                LogicalSize::new(w, STRIP),
            )?;

            {
                let app_handle = app.handle().clone();
                window.on_window_event(move |event| match event {
                    tauri::WindowEvent::Resized(_) => layout_strip(&app_handle),
                    tauri::WindowEvent::Focused(true) => {
                        BADGE_COUNT.store(0, Ordering::Relaxed);
                        set_dock_badge(&app_handle, 0);
                        // Titles move fast while chatting; refresh on focus.
                        let app2 = app_handle.clone();
                        thread::spawn(move || sync_sessions_and_title(&app2));
                    }
                    _ => {}
                });
            }

            // Native menu bar + its actions. Built once now (empty 会话
            // list), rebuilt on the main thread whenever recents change.
            if let Err(e) = build_app_menu(app.handle(), &[]) {
                eprintln!("kimi-ui: 构建菜单失败：{e}");
            }
            let app_handle = app.handle().clone();
            app.on_menu_event(move |handle, event| handle_menu_event(handle, &event));
            start_sessions_refresher(app_handle);

            // Restore the remote connections the user added last time.
            for conn in load_connections() {
                let app_handle = app.handle().clone();
                thread::spawn(move || {
                    if let Err(e) = open_connection(&app_handle, &conn) {
                        eprintln!("kimi-ui: 打开远程连接失败：{e}");
                    }
                });
            }

            let app_handle = app.handle().clone();
            thread::spawn(move || match connect_daemon() {
                Ok(launch) => {
                    let conn = Connection {
                        id: "local".to_string(),
                        // Honour a rename from a previous session.
                        name: load_local_name().unwrap_or_else(|| "本地".to_string()),
                        kind: ConnKind::Local,
                        base: launch.base.clone(),
                        token: launch.token.clone(),
                    };
                    if let Some(state) = app_handle.try_state::<SharedDaemon>() {
                        *state.lock().unwrap() = Some(DaemonState {
                            base: launch.base,
                            token: launch.token,
                        });
                    }
                    if let Err(e) = open_connection(&app_handle, &conn) {
                        eprintln!("kimi-ui: 打开本地连接失败：{e}");
                    }
                    activate_connection(&app_handle, "local");
                }
                Err(e) => {
                    // No daemon: still show a view so the boot guidance renders.
                    let conn = Connection {
                        id: "local".to_string(),
                        name: load_local_name().unwrap_or_else(|| "本地".to_string()),
                        kind: ConnKind::Local,
                        base: String::new(),
                        token: String::new(),
                    };
                    if let Ok(wv) = ensure_view(&app_handle, &conn) {
                        activate_connection(&app_handle, "local");
                        let msg = serde_json::to_string(&e)
                            .unwrap_or_else(|_| "{\"kind\":\"unknown\"}".to_string());
                        let _ = wv.eval(&format!("window.__kimiBootError({msg})"));
                    }
                }
            });
            start_update_check();
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building kimi-ui")
        .run(|_app, event| {
            // Reap the children we spawned (daemon, `kimi rc`) so neither
            // outlives the shell.
            if let tauri::RunEvent::Exit = event {
                if let Ok(mut guard) = SPAWNED_SERVER.lock() {
                    if let Some(mut child) = guard.take() {
                        // SIGTERM so the CLI runs its own shutdown (clearing
                        // its instance files); SIGKILL alone leaves stale
                        // registry entries that mislead the next launch.
                        terminate_child(&mut child, Duration::from_secs(3));
                    }
                }
                if let Some(mut child) = REMOTE_RC.lock().ok().and_then(|mut g| g.take()) {
                    if let Some(lock) = read_rc_lock() {
                        terminate_pid(lock.pid);
                    } else {
                        terminate_pid(child.id());
                    }
                    let _ = child.wait();
                }
            }
        });
}

#[tauri::command]
fn open_url(url: String) {
    if let Ok(u) = url.parse::<Url>() {
        open_in_system_browser(&u);
    }
}

fn parse_sessions(v: &Value) -> Option<Vec<(String, String)>> {
    let items = v["data"]["items"].as_array()?;
    Some(
        items
            .iter()
            .filter(|s| !s["archived"].as_bool().unwrap_or(false))
            .filter_map(|s| {
                let id = s["id"].as_str()?.to_string();
                let title = s["title"]
                    .as_str()
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or("未命名会话");
                Some((id, title.to_string()))
            })
            .collect(),
    )
}

fn refresh_sessions_menu(app: &tauri::AppHandle) {
    let sessions: Vec<(String, String)> = SESSION_TITLES
        .lock()
        .map(|g| {
            g.iter()
                .take(MENU_SESSIONS)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let unchanged = MENU_BUILT
        .lock()
        .map(|built| *built == sessions)
        .unwrap_or(false);
    if unchanged {
        return;
    }
    if let Ok(mut built) = MENU_BUILT.lock() {
        *built = sessions.clone();
    }
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Err(e) = build_app_menu(&app2, &sessions) {
            eprintln!("kimi-ui: 重建菜单失败：{e}");
        }
    });
}

fn refresh_window_title(app: &tauri::AppHandle) {
    if let Some(window) = app.get_window("main") {
        let title = window_title_text();
        let _ = window.set_title(&title);
    }
}

#[tauri::command]
fn set_active_session(app: tauri::AppHandle, id: Option<String>) {
    if let Ok(mut guard) = ACTIVE_SESSION.lock() {
        *guard = id;
    }
    let app = app.clone();
    thread::spawn(move || sync_sessions_and_title(&app));
}

fn start_sessions_refresher(app: tauri::AppHandle) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(30));
        sync_sessions_and_title(&app);
    });
}

fn sync_sessions_and_title(app: &tauri::AppHandle) {
    if let Some(daemon) = daemon_state(app) {
        if let Ok(sessions) = fetch_sessions(&daemon) {
            if let Ok(mut guard) = SESSION_TITLES.lock() {
                *guard = sessions;
            }
        }
    }
    refresh_window_title(app);
    refresh_sessions_menu(app);
}

fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn window_title_text() -> String {
    let Some(id) = ACTIVE_SESSION.lock().ok().and_then(|g| g.clone()) else {
        return "Kimi Code".to_string();
    };
    SESSION_TITLES
        .lock()
        .ok()
        .and_then(|g| {
            g.iter()
                .find(|(sid, _)| *sid == id)
                .map(|(_, title)| format!("{} — Kimi Code", truncate_chars(title, 60)))
        })
        .unwrap_or_else(|| "Kimi Code".to_string())
}

// ---------------------------------------------------------------------------
// Plan quota.
//
// The daemon exposes `GET /api/v1/oauth/usage` (kimi ≥ 0.38): the managed
// account's usage windows. We poll it on a 10-minute TTL and hand raw
// percentages plus reset timestamps to the status page, which formats the
// remaining time in JS (Date.parse handles the ISO timestamps natively).
// Replaces the old headless-TUI /usage screen scrape.
// ---------------------------------------------------------------------------

/// Plan quota from `/api/v1/oauth/usage`.
#[derive(Clone, Debug, serde::Serialize)]
struct PlanUsage {
    /// Daemon this reading came from (cache key).
    #[serde(skip)]
    base: String,
    weekly_pct: u32,
    weekly_reset_at: String,
    hourly_pct: u32,
    hourly_reset_at: String,
    fetched_at: u64,
}

static PLAN_USAGE: Mutex<Option<PlanUsage>> = Mutex::new(None);
static FETCH_RUNNING: AtomicBool = AtomicBool::new(false);

/// Quota TTL: the status page may ask often, we fetch at most this often.
const FETCH_TTL_SECS: u64 = 600;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Minimal blocking HTTP/1.1 GET returning the parsed JSON body. Only for
/// loopback daemon endpoints: no TLS, no redirects, no chunked bodies (the
/// daemon answers small JSON with Content-Length). Same std-only style as
/// static_server.
fn http_get_json(base: &str, path: &str, token: &str) -> Result<Value, String> {
    let host_part = base.trim_start_matches("http://");
    let (host, port) = host_part
        .rsplit_once(':')
        .ok_or_else(|| format!("daemon 地址缺少端口：{base}"))?;
    let port: u16 = port.parse().map_err(|_| format!("daemon 端口无效：{port}"))?;

    let mut stream = TcpStream::connect((host, port)).map_err(|e| format!("连接 daemon 失败：{e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("发送请求失败：{e}"))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("读取响应失败：{e}"))?;
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| "daemon 响应缺少头部分隔".to_string())?;
    if !head.starts_with("HTTP/1.1 200") {
        let status = head.lines().next().unwrap_or("");
        return Err(format!("daemon 响应非 200：{status}"));
    }
    serde_json::from_str(body).map_err(|e| format!("解析 JSON 失败：{e}"))
}

/// One usage window row: `summary` or an entry of `limits[]`.
fn row_pct(row: &Value) -> Option<u32> {
    let used = row["used"].as_f64()?;
    let limit = row["limit"].as_f64()?;
    (limit > 0.0).then(|| (used / limit * 100.0).round() as u32)
}

fn row_reset_at(row: &Value) -> String {
    row["reset_at"].as_str().unwrap_or("").to_string()
}

fn row_is_weekly(row: &Value) -> bool {
    (row["window"]["unit"].as_str() == Some("week"))
        || row["name"].as_str().map_or(false, |n| n.to_lowercase().contains("week"))
}

fn row_is_hourly(row: &Value) -> bool {
    let window = &row["window"];
    (window["duration"].as_u64() == Some(5) && window["unit"].as_str() == Some("hour"))
        || row["name"].as_str().map_or(false, |n| n.to_lowercase().contains("5h"))
}

/// Extract (weekly pct, weekly reset, hourly pct, hourly reset) from the
/// `/oauth/usage` data object. The weekly row may live in `summary` or in
/// `limits[]` depending on account shape — consider both.
fn extract_plan_usage(base: &str, data: &Value) -> Result<PlanUsage, String> {
    if data["kind"].as_str() == Some("error") {
        let msg = data["message"].as_str().unwrap_or("未知错误");
        return Err(format!("额度接口返回错误：{msg}"));
    }
    let mut rows: Vec<&Value> = Vec::new();
    if data["summary"].is_object() {
        rows.push(&data["summary"]);
    }
    if let Some(limits) = data["limits"].as_array() {
        rows.extend(limits);
    }
    let weekly = rows.iter().find(|r| row_is_weekly(r) && row_pct(r).is_some());
    let hourly = rows.iter().find(|r| row_is_hourly(r) && row_pct(r).is_some());
    match (weekly, hourly) {
        (Some(w), Some(h)) => Ok(PlanUsage {
            base: base.to_string(),
            weekly_pct: row_pct(w).unwrap_or(0),
            weekly_reset_at: row_reset_at(w),
            hourly_pct: row_pct(h).unwrap_or(0),
            hourly_reset_at: row_reset_at(h),
            fetched_at: now_secs(),
        }),
        _ => Err("额度响应缺少每周或 5 小时窗口行（接口结构可能已变化）".to_string()),
    }
}

/// The status page asks for plan quota; we return the cache and refresh it in
/// the background when stale.
#[tauri::command]
fn plan_usage(state: tauri::State<'_, SharedDaemon>) -> Value {
    let Some(daemon) = state.lock().ok().and_then(|g| g.clone()) else {
        return serde_json::json!({ "loading": true });
    };
    // Quota is per-account, so the cache is keyed by the daemon it came from:
    // otherwise switching tabs would show the previous connection's numbers.
    let stale = PLAN_USAGE
        .lock()
        .map(|u| {
            u.as_ref().map_or(true, |u| {
                u.base != daemon.base || u.fetched_at + FETCH_TTL_SECS < now_secs()
            })
        })
        .unwrap_or(true);
    if stale && !FETCH_RUNNING.swap(true, Ordering::Relaxed) {
        thread::spawn(move || {
            match http_get_json(&daemon.base, "/api/v1/oauth/usage", &daemon.token)
                .and_then(|v| extract_plan_usage(&daemon.base, &v["data"]))
            {
                Ok(u) => {
                    if let Ok(mut guard) = PLAN_USAGE.lock() {
                        *guard = Some(u);
                    }
                }
                Err(e) => eprintln!("kimi-ui: 额度获取失败：{e}"),
            }
            FETCH_RUNNING.store(false, Ordering::Relaxed);
        });
    }
    let guard = PLAN_USAGE.lock().ok();
    match guard.as_ref().and_then(|g| g.as_ref()) {
        Some(u) => serde_json::to_value(u).unwrap_or_else(|_| Value::Null),
        None => serde_json::json!({ "loading": true }),
    }
}

// ---------------------------------------------------------------------------
// Remote Control (official experimental `kimi rc`).
//
// `kimi rc` runs its own foreground server plus a reverse-tunnel client to
// code-rc.kimi.com; the access URL goes to stdout and the single-instance
// lock at <home>/server/rc.json carries {pid, local_origin, url}. The shell
// starts/stops that child, reads the lock for status (TCP-probing
// local_origin instead of trusting the pid — same liveness model as daemon
// discovery), and surfaces the QR code the CLI drops at <home>/rc-qrcode.png.
// ---------------------------------------------------------------------------

/// The `kimi rc` child we spawned (None when started outside the shell).
static REMOTE_RC: Mutex<Option<Child>> = Mutex::new(None);
static RC_STARTING: AtomicBool = AtomicBool::new(false);
/// Tail of the child's stderr, surfaced when it dies unexpectedly.
static RC_STDERR: Mutex<String> = Mutex::new(String::new());

/// Status payload for the status page's remote card.
#[derive(Clone, Default, serde::Serialize)]
struct RcState {
    running: bool,
    starting: bool,
    url: String,
    error: String,
    /// rc-qrcode.png as a data-URL body (empty when unavailable).
    qr_base64: String,
}

fn rc_lock_path() -> PathBuf {
    kimi_home().join("server/rc.json")
}

/// Parsed rc.json: the fields the shell cares about.
struct RcLock {
    pid: u32,
    local_origin: String,
    url: String,
}

fn read_rc_lock() -> Option<RcLock> {
    let raw = fs::read_to_string(rc_lock_path()).ok()?;
    parse_rc_lock(&raw)
}

/// Fields the shell cares about from rc.json.
fn parse_rc_lock(raw: &str) -> Option<RcLock> {
    let v: Value = serde_json::from_str(raw).ok()?;
    Some(RcLock {
        pid: v["pid"].as_u64().and_then(|p| u32::try_from(p).ok())?,
        local_origin: v["local_origin"].as_str().unwrap_or("").to_string(),
        url: v["url"].as_str().unwrap_or("").to_string(),
    })
}

/// The lock counts as live when its recorded local server still accepts
/// connections — immune to pid reuse, same model as daemon discovery.
fn rc_lock_alive(lock: &RcLock) -> bool {
    let origin = lock.local_origin.trim_start_matches("http://");
    match origin.split_once(':') {
        Some((host, port)) => port
            .parse::<u16>()
            .map(|p| tcp_alive(host, p))
            .unwrap_or(false),
        None => false,
    }
}

fn rc_qrcode_base64() -> String {
    use base64::Engine as _;
    fs::read(kimi_home().join("rc-qrcode.png"))
        .ok()
        .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes))
        .unwrap_or_default()
}

fn rc_state() -> RcState {
    let starting = RC_STARTING.load(Ordering::Relaxed);
    if let Some(lock) = read_rc_lock() {
        if rc_lock_alive(&lock) {
            return RcState {
                running: true,
                starting,
                url: lock.url,
                error: String::new(),
                qr_base64: rc_qrcode_base64(),
            };
        }
    }
    if let Ok(mut guard) = REMOTE_RC.lock() {
        if let Some(child) = guard.as_mut() {
            if let Ok(Some(_)) = child.try_wait() {
                // Our child died without leaving a live lock — surface stderr.
                let tail = RC_STDERR.lock().map(|s| truncate_chars(&s, 300)).unwrap_or_default();
                return RcState {
                    running: false,
                    starting: false,
                    url: String::new(),
                    error: format!("`kimi rc` 已退出：{}", tail.trim()),
                    qr_base64: String::new(),
                };
            }
        }
    }
    RcState {
        running: false,
        starting,
        ..RcState::default()
    }
}

/// SIGTERM the RC child so the CLI's own shutdown runs (relay disconnect,
/// lock release). `kill`/`taskkill` via Command keeps us off libc.
fn terminate_pid(pid: u32) {
    #[cfg(unix)]
    let _ = no_console(Command::new("kill")).arg(pid.to_string()).spawn();
    #[cfg(windows)]
    let _ = no_console(Command::new("taskkill"))
        .args(["/PID", &pid.to_string()])
        .spawn();
}

fn rc_start(app: &tauri::AppHandle) {
    if RC_STARTING.swap(true, Ordering::Relaxed) {
        return;
    }
    let app = app.clone();
    thread::spawn(move || {
        if let Err(e) = rc_start_blocking(&app) {
            eprintln!("kimi-ui: Remote Control 启动失败：{e}");
            if let Ok(mut guard) = RC_STDERR.lock() {
                *guard = e.clone();
            }
        }
        RC_STARTING.store(false, Ordering::Relaxed);
    });
}

fn rc_start_blocking(app: &tauri::AppHandle) -> Result<(), String> {
    if rc_state().running {
        return Ok(());
    }
    let kimi = find_kimi().ok_or("找不到 kimi CLI")?;
    let mut cmd = no_console(Command::new(&kimi));
    cmd.arg("rc")
        .env("KIMI_CODE_EXPERIMENTAL_REMOTE_CONTROL", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("拉起 `kimi rc` 失败：{e}"))?;

    // Drain both pipes so the child never blocks on a full buffer; keep a
    // stderr tail for failure reporting.
    if let Some(stderr) = child.stderr.take() {
        thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            let mut reader = BufReader::new(stderr);
            let mut buf = String::new();
            let mut total = String::new();
            loop {
                buf.clear();
                match reader.read_line(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        total.push_str(&buf);
                        if let Ok(mut guard) = RC_STDERR.lock() {
                            *guard = truncate_chars(&total, 2000);
                        }
                    }
                }
            }
        });
    }
    if let Some(mut stdout) = child.stdout.take() {
        thread::spawn(move || {
            let mut sink = Vec::new();
            let _ = std::io::Read::read_to_end(&mut stdout, &mut sink);
        });
    }
    if let Ok(mut guard) = REMOTE_RC.lock() {
        *guard = Some(child);
    }

    // Wait for the lock (and its URL) or an early exit — the CLI needs to
    // boot its server and register with the relay first.
    for _ in 0..40 {
        thread::sleep(Duration::from_millis(300));
        let state = rc_state();
        if state.running && !state.url.is_empty() {
            notify_rc_ready(app);
            return Ok(());
        }
        if let Ok(mut guard) = REMOTE_RC.lock() {
            if let Some(child) = guard.as_mut() {
                if let Ok(Some(_)) = child.try_wait() {
                    let tail = RC_STDERR
                        .lock()
                        .map(|s| truncate_chars(&s, 300))
                        .unwrap_or_default();
                    return Err(format!("`kimi rc` 提前退出：{}", tail.trim()));
                }
            }
        }
    }
    Err("等待 Remote Control 就绪超时（需要 Kimi 账号登录）".to_string())
}

fn rc_stop() {
    if let Some(mut child) = REMOTE_RC.lock().ok().and_then(|mut g| g.take()) {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        if let Some(lock) = read_rc_lock() {
            terminate_pid(lock.pid);
        } else {
            terminate_pid(child.id());
        }
        let _ = child.wait();
    } else if let Some(lock) = read_rc_lock() {
        terminate_pid(lock.pid);
    }
}

/// Native notification once the tunnel is ready (the card may be closed).
fn notify_rc_ready(app: &tauri::AppHandle) {
    let _ = app
        .notification()
        .builder()
        .title("Kimi Remote Control")
        .body("远程访问已就绪，点击状态栏“远程”查看链接")
        .show();
}

/// Status-bar entry point: "start" | "stop" | "status".
#[tauri::command]
fn remote_control(app: tauri::AppHandle, action: String) -> Value {
    match action.as_str() {
        "start" => {
            rc_start(&app);
            serde_json::json!({})
        }
        "stop" => {
            rc_stop();
            serde_json::json!({})
        }
        _ => serde_json::to_value(rc_state()).unwrap_or(Value::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kimi-ui-discovery-test-{}-{tag}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("server/instances")).unwrap();
        dir
    }

    /// A port with a live listener behind it.
    fn live_port() -> (TcpListener, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    /// A port that is closed on loopback: bound to reserve, released on drop.
    fn dead_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[test]
    fn prefers_longest_running_instance() {
        let home = temp_home("multi");
        let (_l1, p1) = live_port();
        let (_l2, p2) = live_port();
        fs::write(
            home.join("server/instances/newer.json"),
            format!(
                r#"{{"server_id":"newer","pid":1,"host":"127.0.0.1","port":{p2},"started_at":200,"heartbeat_at":200}}"#
            ),
        )
        .unwrap();
        fs::write(
            home.join("server/instances/older.json"),
            format!(
                r#"{{"server_id":"older","pid":1,"host":"127.0.0.1","port":{p1},"started_at":100,"heartbeat_at":100}}"#
            ),
        )
        .unwrap();
        let addr = discover_daemon(&home, None).unwrap();
        assert_eq!(addr.port, p1);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn skips_stale_daemon_older_than_cli() {
        let home = temp_home("stale");
        let (_l1, stale_port) = live_port();
        let (_l2, fresh_port) = live_port();
        fs::write(
            home.join("server/instances/stale.json"),
            format!(
                r#"{{"server_id":"stale","pid":1,"host":"127.0.0.1","port":{stale_port},"started_at":100,"host_version":"0.36.1"}}"#
            ),
        )
        .unwrap();
        fs::write(
            home.join("server/instances/fresh.json"),
            format!(
                r#"{{"server_id":"fresh","pid":1,"host":"127.0.0.1","port":{fresh_port},"started_at":200,"host_version":"0.39.0"}}"#
            ),
        )
        .unwrap();
        // Stale daemon is skipped even though it is the longest-running.
        let addr = discover_daemon(&home, Some("0.39.0")).unwrap();
        assert_eq!(addr.port, fresh_port);
        // Without a known CLI version the legacy attach behavior is kept.
        let addr = discover_daemon(&home, None).unwrap();
        assert_eq!(addr.port, stale_port);
        // An equal or newer daemon is still attached.
        let addr = discover_daemon(&home, Some("0.36.1")).unwrap();
        assert_eq!(addr.port, stale_port);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn stale_lock_only_is_rejected() {
        let home = temp_home("stale-lock");
        fs::remove_dir_all(home.join("server/instances")).unwrap();
        let (_listener, port) = live_port();
        fs::write(
            home.join("server/lock"),
            format!(
                r#"{{"pid":1,"host":"127.0.0.1","port":{port},"host_version":"0.38.0"}}"#
            ),
        )
        .unwrap();
        assert!(discover_daemon(&home, Some("0.39.0")).is_err());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn stale_daemons_lists_only_live_stale_servers() {
        let home = temp_home("stale-list");
        let (_l1, stale_port) = live_port();
        let (_l2, fresh_port) = live_port();
        fs::write(
            home.join("server/instances/stale.json"),
            format!(
                r#"{{"server_id":"stale","pid":1,"host":"127.0.0.1","port":{stale_port},"started_at":100,"host_version":"0.36.1"}}"#
            ),
        )
        .unwrap();
        fs::write(
            home.join("server/instances/fresh.json"),
            format!(
                r#"{{"server_id":"fresh","pid":1,"host":"127.0.0.1","port":{fresh_port},"started_at":200,"host_version":"0.39.0"}}"#
            ),
        )
        .unwrap();
        fs::write(
            home.join("server/instances/stale-dead.json"),
            format!(
                r#"{{"server_id":"stale-dead","pid":1,"host":"127.0.0.1","port":{},"started_at":300,"host_version":"0.36.1"}}"#,
                dead_port()
            ),
        )
        .unwrap();
        let stale = stale_daemons(&home, Some("0.39.0"));
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].port, stale_port);
        // No CLI version known → nothing is provably stale → nothing to kill.
        assert!(stale_daemons(&home, None).is_empty());
        let _ = fs::remove_dir_all(&home);
    }

    /// shutdown_daemon over a real socket: the request is the graceful
    /// shutdown route carrying the bearer token.
    #[test]
    fn shutdown_daemon_posts_shutdown_route() {
        use std::io::Read as _;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = sock.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(request.starts_with("POST /api/v1/shutdown HTTP/1.1"));
            assert!(request.contains("Authorization: Bearer t0ken"));
            // Exit like a real daemon: close without answering.
        });
        shutdown_daemon(
            &DaemonAddr {
                host: "127.0.0.1".to_string(),
                port,
            },
            "t0ken",
        );
        server.join().unwrap();
    }

    #[test]
    fn skips_dead_instance_and_falls_back_to_lock() {
        let home = temp_home("fallback");
        let (_listener, lock_port) = live_port();
        fs::write(
            home.join("server/instances/dead.json"),
            format!(
                r#"{{"server_id":"dead","pid":1,"host":"127.0.0.1","port":{},"started_at":1,"heartbeat_at":1}}"#,
                dead_port()
            ),
        )
        .unwrap();
        fs::write(
            home.join("server/lock"),
            format!(r#"{{"pid":1,"host":"127.0.0.1","port":{lock_port}}}"#),
        )
        .unwrap();
        let addr = discover_daemon(&home, None).unwrap();
        assert_eq!(addr.port, lock_port);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn lock_only_still_works() {
        let home = temp_home("lock-only");
        // The 0.26/0.27 lock shape: started_at is an ISO string, no instances dir.
        fs::remove_dir_all(home.join("server/instances")).unwrap();
        let (_listener, port) = live_port();
        fs::write(
            home.join("server/lock"),
            format!(
                r#"{{"pid":1,"started_at":"2026-07-16T16:52:51.702Z","host":"127.0.0.1","port":{port},"host_version":"0.27.0"}}"#
            ),
        )
        .unwrap();
        let addr = discover_daemon(&home, None).unwrap();
        assert_eq!(addr.port, port);
        assert_eq!(addr.host, "127.0.0.1");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn wildcard_host_normalizes_to_loopback() {
        assert_eq!(normalize_host(Some("0.0.0.0")), "127.0.0.1");
        assert_eq!(normalize_host(Some("::")), "127.0.0.1");
        assert_eq!(normalize_host(Some("[::]")), "127.0.0.1");
        assert_eq!(normalize_host(Some("")), "127.0.0.1");
        assert_eq!(normalize_host(None), "127.0.0.1");
        assert_eq!(normalize_host(Some("192.168.1.2")), "192.168.1.2");
    }

    #[test]
    fn errors_when_nothing_is_reachable() {
        let home = temp_home("empty");
        fs::write(
            home.join("server/lock"),
            format!(r#"{{"pid":1,"host":"127.0.0.1","port":{}}}"#, dead_port()),
        )
        .unwrap();
        assert!(discover_daemon(&home, None).is_err());
        let _ = fs::remove_dir_all(&home);
    }

    /// Serves index.html over HTTP from the active asset source — disk in
    /// debug, embedded-in-exe in release (run `cargo test --release` to
    /// exercise the embedded path).
    #[test]
    fn serves_index_html_over_http() {
        use std::io::{Read, Write};
        let source = asset_source().expect("asset source");
        let port = crate::static_server::serve(source, 0).unwrap();
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 200 OK"), "response: {text:.200}");
        assert!(text.contains("text/html"), "response: {text:.200}");
        assert!(
            text.contains("Cache-Control: no-cache"),
            "stable entrypoints must revalidate: {text:.300}"
        );
        assert!(text.to_lowercase().contains("<html"), "response: {text:.200}");
    }

    /// The served index.html must reference assets the server can actually
    /// deliver with a script-executable MIME — catches a broken/partial
    /// asset source (e.g. non-recursive embed) that white-screens the app.
    #[test]
    fn serves_referenced_assets_with_correct_mime() {
        use std::io::{Read, Write};
        let source = asset_source().expect("asset source");
        let port = crate::static_server::serve(source, 0).unwrap();
        let get = |path: &str| -> String {
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .write_all(
                    format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).unwrap();
            String::from_utf8_lossy(&response).into_owned()
        };
        let index = get("/");
        let asset = {
            let start = index
                .find("\"/assets/")
                .map(|i| i + 1)
                .or_else(|| index.find("\"./assets/").map(|i| i + 2))
                .expect("index.html should reference an /assets/ bundle");
            let rest = &index[start..];
            rest[..rest.find('"').unwrap()].to_string()
        };
        let head = get(&asset);
        assert!(head.starts_with("HTTP/1.1 200 OK"), "{asset}: {head:.200}");
        assert!(
            head.contains("text/javascript"),
            "{asset} must be executable JS, not the SPA fallback: {head:.200}"
        );
        assert!(
            head.contains("Cache-Control: public, max-age=31536000, immutable"),
            "hashed assets should be cached across launches: {head:.300}"
        );
        let missing = get("/assets/definitely-missing.js");
        assert!(
            missing.starts_with("HTTP/1.1 404 Not Found"),
            "missing assets must not receive the SPA fallback: {missing:.200}"
        );
    }

    #[test]
    fn parses_bare_semver_only() {
        assert_eq!(parse_kimi_version("0.27.0\n"), Some("0.27.0".to_string()));
        assert_eq!(parse_kimi_version("  1.2.3  "), Some("1.2.3".to_string()));
        assert_eq!(parse_kimi_version("v0.27.0"), None);
        assert_eq!(parse_kimi_version("0.27.0-beta"), None);
        assert_eq!(parse_kimi_version(""), None);
    }

    #[test]
    fn enforces_minimum_kimi_version() {
        assert!(version_older("0.39.0", MIN_KIMI_VERSION));
        assert!(!version_older("0.39.1", MIN_KIMI_VERSION));
        assert!(!version_older("0.39.2", MIN_KIMI_VERSION));
        assert!(!version_older("1.0.0", MIN_KIMI_VERSION));
    }

    #[test]
    fn desktop_query_uses_official_bundle_flags() {
        let query = desktop_query();
        assert!(query.starts_with("?kimi_desktop&platform="));
        if cfg!(target_os = "macos") {
            assert_eq!(query, "?kimi_desktop&platform=darwin");
        }
    }

    /// Real `/api/v1/oauth/usage` shape captured from a 0.38.0 daemon: the
    /// weekly row lives in `summary`, the 5h row in `limits[]`.
    const OAUTH_USAGE_SAMPLE: &str = r#"{
        "kind": "ok",
        "summary": {"window": {"duration": 1, "unit": "week"}, "used": 3, "limit": 100,
                    "reset_at": "2026-08-29T13:17:18Z"},
        "limits": [{"window": {"duration": 5, "unit": "hour"}, "used": 7, "limit": 200,
                    "reset_at": "2026-08-23T19:17:18Z"}],
        "extra_usage": null
    }"#;

    #[test]
    fn extracts_plan_usage_from_oauth_response() {
        let data: Value = serde_json::from_str(OAUTH_USAGE_SAMPLE).unwrap();
        let plan = extract_plan_usage("http://127.0.0.1:1", &data).unwrap();
        assert_eq!(plan.weekly_pct, 3);
        assert_eq!(plan.hourly_pct, 4); // 7/200 rounds to 4%
        assert_eq!(plan.weekly_reset_at, "2026-08-29T13:17:18Z");
        assert_eq!(plan.hourly_reset_at, "2026-08-23T19:17:18Z");
    }

    /// The quota reading records which daemon produced it, so switching tabs
    /// does not show the previous connection's numbers.
    #[test]
    fn plan_usage_records_its_source_daemon() {
        let data: Value = serde_json::from_str(OAUTH_USAGE_SAMPLE).unwrap();
        let a = extract_plan_usage("http://127.0.0.1:58627", &data).unwrap();
        let b = extract_plan_usage("http://192.168.31.106:58630", &data).unwrap();
        assert_eq!(a.base, "http://127.0.0.1:58627");
        assert_eq!(b.base, "http://192.168.31.106:58630");
        assert_ne!(a.base, b.base, "cache key must distinguish connections");
        // The key is a cache detail, never sent to the page.
        let json = serde_json::to_string(&a).unwrap();
        assert!(!json.contains("58627"), "base must be skipped in JSON: {json}");
    }

    #[test]
    fn oauth_usage_rows_are_matched_by_name_when_window_missing() {
        let data: Value = serde_json::from_str(
            r#"{"kind":"ok","summary":null,
                "limits":[{"name":"Weekly limit","used":50,"limit":100},
                          {"name":"5h limit","used":10,"limit":100}]}"#,
        )
        .unwrap();
        let plan = extract_plan_usage("http://127.0.0.1:1", &data).unwrap();
        assert_eq!(plan.weekly_pct, 50);
        assert_eq!(plan.hourly_pct, 10);
    }

    #[test]
    fn oauth_usage_error_and_missing_rows_are_rejected() {
        let err: Value =
            serde_json::from_str(r#"{"kind":"error","message":"not logged in"}"#).unwrap();
        assert!(extract_plan_usage("http://127.0.0.1:1", &err)
            .unwrap_err()
            .contains("not logged in"));
        let partial: Value = serde_json::from_str(
            r#"{"kind":"ok","summary":null,
                "limits":[{"window":{"duration":1,"unit":"week"},"used":1,"limit":10}]}"#,
        )
        .unwrap();
        assert!(extract_plan_usage("http://127.0.0.1:1", &partial).is_err());
    }

    /// http_get_json over a real socket: request carries the bearer token,
    /// response body is parsed as JSON.
    #[test]
    fn http_get_json_roundtrip() {
        use std::io::{Read as _, Write as _};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = sock.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(request.starts_with("GET /api/v1/oauth/usage HTTP/1.1"));
            assert!(request.contains("Authorization: Bearer t0ken"));
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 10\r\nConnection: close\r\n\r\n{\"ok\":true}",
            )
            .unwrap();
        });
        let value = http_get_json(
            &format!("http://127.0.0.1:{port}"),
            "/api/v1/oauth/usage",
            "t0ken",
        )
        .unwrap();
        assert_eq!(value["ok"], true);
        server.join().unwrap();
    }

    #[test]
    fn http_get_json_rejects_error_status() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf);
            let _ = sock.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
        });
        let err = http_get_json(&format!("http://127.0.0.1:{port}"), "/", "x").unwrap_err();
        assert!(err.contains("401"), "{err}");
    }

    #[test]
    fn parses_sessions_for_title_and_menu() {
        let v: Value = serde_json::from_str(
            r#"{"data":{"items":[
                {"id":"s1","title":"修复登录","busy":false,"archived":false},
                {"id":"s2","title":"","busy":false,"archived":false},
                {"id":"s3","title":"旧会话","busy":false,"archived":true}
            ]}}"#,
        )
        .unwrap();
        let sessions = parse_sessions(&v).unwrap();
        assert_eq!(
            sessions,
            vec![
                ("s1".to_string(), "修复登录".to_string()),
                ("s2".to_string(), "未命名会话".to_string()),
            ]
        );
        assert!(parse_sessions(&serde_json::json!({ "data": {} })).is_none());
    }

    #[test]
    fn truncates_on_char_boundaries() {
        assert_eq!(truncate_chars("abcde", 3), "abc");
        assert_eq!(truncate_chars("会话标题很长", 4), "会话标题");
        assert_eq!(truncate_chars("短", 10), "短");
        assert_eq!(truncate_chars("", 10), "");
    }

    #[test]
    fn parses_rc_lock_fields() {
        let lock = parse_rc_lock(
            r#"{"pid":4321,"nonce":"x","local_origin":"http://127.0.0.1:58627",
                "device_id":"d","url":"https://code-rc.kimi.com/devices/d/?rc=1",
                "started_at":1}"#,
        )
        .unwrap();
        assert_eq!(lock.pid, 4321);
        assert_eq!(lock.local_origin, "http://127.0.0.1:58627");
        assert_eq!(lock.url, "https://code-rc.kimi.com/devices/d/?rc=1");
        assert!(parse_rc_lock(r#"{"pid":"not-a-number"}"#).is_none());
        assert!(parse_rc_lock("not json").is_none());
    }

    #[test]
    fn rc_lock_liveness_probes_recorded_origin() {
        let (_listener, port) = live_port();
        let live = RcLock {
            pid: 1,
            local_origin: format!("http://127.0.0.1:{port}"),
            url: String::new(),
        };
        assert!(rc_lock_alive(&live));
        let dead = RcLock {
            pid: 1,
            local_origin: format!("http://127.0.0.1:{}", dead_port()),
            url: String::new(),
        };
        assert!(!rc_lock_alive(&dead));
        assert!(!rc_lock_alive(&RcLock {
            pid: 1,
            local_origin: String::new(),
            url: String::new(),
        }));
    }

    /// Real releases/latest API shape: the macOS asset must be picked out of
    /// a mixed asset list with its url/size/digest.
    #[test]
    fn parses_release_assets_for_auto_update() {
        let raw = r#"{
          "tag_name": "v0.1.17",
          "html_url": "https://github.com/liujunGH/kimi-ui/releases/tag/v0.1.17",
          "body": "- notes",
          "assets": [
            {"name": "kimi-ui-windows-x64.zip", "size": 100,
             "digest": "sha256:1111", "browser_download_url": "https://x/w.zip"},
            {"name": "kimi-ui-macos-arm64.zip", "size": 18168065,
             "digest": "sha256:7960187b",
             "browser_download_url": "https://github.com/liujunGH/kimi-ui/releases/download/v0.1.17/kimi-ui-macos-arm64.zip"}
          ]
        }"#;
        let info = parse_release_json(raw).unwrap();
        assert_eq!(info.latest, "0.1.17");
        assert!(!info.has_update); // 0.1.17 is not newer than the current 0.1.18.
        assert_eq!(info.asset_url.ends_with("kimi-ui-macos-arm64.zip"), true);
        assert_eq!(info.asset_size, 18168065);
        assert_eq!(info.asset_digest, "sha256:7960187b");

        // No macOS asset -> no UpdateInfo at all (auto-update impossible).
        let raw_no_asset = r#"{"tag_name":"v9","html_url":"u","body":"","assets":[]}"#;
        assert!(parse_release_json(raw_no_asset).is_none());
    }

    #[test]
    fn finds_instance_pid_for_endpoint() {
        let home = temp_home("instance-pid");
        let (_listener, port) = live_port();
        fs::write(
            home.join("server/instances/a.json"),
            format!(
                r#"{{"server_id":"a","pid":4242,"host":"127.0.0.1","port":{port},"started_at":1,"heartbeat_at":1}}"#
            ),
        )
        .unwrap();
        let addr = DaemonAddr {
            host: "127.0.0.1".to_string(),
            port,
        };
        assert_eq!(instance_pid_for(&home, &addr), Some(4242));

        // A different port must not match this entry.
        let other = DaemonAddr {
            host: "127.0.0.1".to_string(),
            port: dead_port(),
        };
        assert_eq!(instance_pid_for(&home, &other), None);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn instance_pid_reads_legacy_lock_too() {
        let home = temp_home("instance-lock");
        fs::remove_dir_all(home.join("server/instances")).unwrap();
        let (_listener, port) = live_port();
        fs::write(
            home.join("server/lock"),
            format!(r#"{{"pid":777,"host":"127.0.0.1","port":{port}}}"#),
        )
        .unwrap();
        let addr = DaemonAddr {
            host: "127.0.0.1".to_string(),
            port,
        };
        assert_eq!(instance_pid_for(&home, &addr), Some(777));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn parses_daemon_base_into_address() {
        let addr = current_addr("http://127.0.0.1:58627").unwrap();
        assert_eq!(addr.host, "127.0.0.1");
        assert_eq!(addr.port, 58627);
        let lan = current_addr("http://192.168.31.106:60001").unwrap();
        assert_eq!(lan.host, "192.168.31.106");
        assert_eq!(lan.port, 60001);
        assert!(current_addr("http://127.0.0.1").is_none());
        assert!(current_addr("http://127.0.0.1:notaport").is_none());
    }

    #[test]
    fn recognizes_loopback_hosts() {
        for h in ["127.0.0.1", "localhost", "::1", "[::1]"] {
            assert!(is_loopback_host(h), "{h} should be loopback");
        }
        for h in ["192.168.31.106", "10.0.0.5", "0.0.0.0", "example.com"] {
            assert!(!is_loopback_host(h), "{h} should not be loopback");
        }
    }

    #[test]
    fn parses_remote_connection_addresses() {
        let c = parse_remote_connection("台式机", "192.168.31.106:58627", " tok ").unwrap();
        assert_eq!(c.base, "http://192.168.31.106:58627");
        assert_eq!(c.name, "台式机");
        assert_eq!(c.id, "192.168.31.106-58627");
        assert!(c.kind == ConnKind::Remote);
        assert_eq!(c.token, "tok"); // trimmed

        let bare = parse_remote_connection("", "10.0.0.8", "t").unwrap();
        assert_eq!(bare.base, "http://10.0.0.8:58627");
        assert_eq!(bare.name, "10.0.0.8:58627"); // name defaults to the address

        let with_scheme = parse_remote_connection("n", "http://10.0.0.9:60001", "t").unwrap();
        assert_eq!(with_scheme.base, "http://10.0.0.9:60001");

        assert!(parse_remote_connection("n", "   ", "t").is_err());
        assert!(parse_remote_connection("n", "http://", "t").is_err());
    }

    /// Remote views load the daemon's own UI (same-origin, so no `kimi_origin`
    /// and no CORS entry needed on the far side); local views go through the
    /// shell's stable loopback origin.
    #[test]
    fn builds_connection_urls_per_kind() {
        let local = Connection {
            id: "local".into(),
            name: "本地".into(),
            kind: ConnKind::Local,
            base: "http://127.0.0.1:58627".into(),
            token: "tk".into(),
        };
        let local_url = connection_url(&local).to_string();
        assert!(local_url.starts_with("http://127.0.0.1:"), "{local_url}");
        assert!(local_url.contains("kimi_origin="), "{local_url}");
        assert!(local_url.ends_with("#token=tk"), "{local_url}");

        let remote = Connection {
            id: "r".into(),
            name: "远程".into(),
            kind: ConnKind::Remote,
            base: "http://192.168.31.106:58627".into(),
            token: "tk".into(),
        };
        let remote_url = connection_url(&remote).to_string();
        assert!(remote_url.starts_with("http://192.168.31.106:58627/"), "{remote_url}");
        assert!(!remote_url.contains("kimi_origin"), "{remote_url}");
        assert!(remote_url.ends_with("#token=tk"), "{remote_url}");
    }

    /// The local tab's NAME must survive a restart, while its base/token stay
    /// out of the file (re-derived from the live daemon each launch).
    #[test]
    fn persists_local_name_but_not_local_credentials() {
        let conns = vec![
            Connection {
                id: "local".into(),
                name: "本机".into(),
                kind: ConnKind::Local,
                base: "http://127.0.0.1:58627".into(),
                token: "secret-local".into(),
            },
            Connection {
                id: "r".into(),
                name: "台式机".into(),
                kind: ConnKind::Remote,
                base: "http://10.0.0.9:60001".into(),
                token: "secret-remote".into(),
            },
        ];
        // Mirror save_connections' projection.
        let persisted: Vec<Connection> = conns
            .iter()
            .map(|c| match c.kind {
                ConnKind::Remote => c.clone(),
                ConnKind::Local => Connection {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    kind: ConnKind::Local,
                    base: String::new(),
                    token: String::new(),
                },
            })
            .collect();
        let raw = serde_json::to_string(&persisted).unwrap();

        // Local name kept, local secret dropped, remote intact.
        assert!(raw.contains("本机"), "local name must persist: {raw}");
        assert!(!raw.contains("secret-local"), "local token must not persist");
        assert!(raw.contains("secret-remote"));
        assert!(raw.contains("台式机"));

        // load_local_name reads it back; load_connections drops the local stub.
        let round: Vec<Connection> = serde_json::from_str(&raw).unwrap();
        let local = round.iter().find(|c| c.kind == ConnKind::Local).unwrap();
        assert_eq!(local.name, "本机");
        assert!(local.base.is_empty() && local.token.is_empty());
    }

    #[test]
    fn persists_only_remote_connections() {
        // The local daemon is derived fresh each launch; only remote
        // connections (and their tokens) are written to disk.
        let conns = vec![
            Connection {
                id: "local".into(),
                name: "本地".into(),
                kind: ConnKind::Local,
                base: "http://127.0.0.1:58627".into(),
                token: "secret-local".into(),
            },
            Connection {
                id: "r".into(),
                name: "远程".into(),
                kind: ConnKind::Remote,
                base: "http://10.0.0.9:60001".into(),
                token: "secret-remote".into(),
            },
        ];
        let remote: Vec<&Connection> =
            conns.iter().filter(|c| c.kind == ConnKind::Remote).collect();
        let raw = serde_json::to_string_pretty(&remote).unwrap();
        assert!(raw.contains("secret-remote"));
        assert!(!raw.contains("secret-local"));
        let round: Vec<Connection> = serde_json::from_str(&raw).unwrap();
        assert_eq!(round.len(), 1);
        assert_eq!(round[0].id, "r");
        assert!(round[0].kind == ConnKind::Remote);
    }

    #[test]
    fn daemon_candidates_orders_and_filters() {
        let home = temp_home("candidates");
        let (_l1, p1) = live_port();
        let (_l2, p2) = live_port();
        fs::write(
            home.join("server/instances/older.json"),
            format!(
                r#"{{"server_id":"older","pid":1,"host":"127.0.0.1","port":{p1},"started_at":100,"host_version":"0.42.0"}}"#
            ),
        )
        .unwrap();
        fs::write(
            home.join("server/instances/newer.json"),
            format!(
                r#"{{"server_id":"newer","pid":2,"host":"0.0.0.0","port":{p2},"started_at":200,"host_version":"0.42.0"}}"#
            ),
        )
        .unwrap();
        let found = daemon_candidates(&home, Some("0.42.0"));
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].port, p1); // longest-running first
        assert_eq!(found[0].host, "127.0.0.1");
        assert_eq!(found[1].port, p2);
        assert_eq!(found[1].host, "127.0.0.1"); // wildcard bind normalizes

        // A daemon older than the installed CLI is filtered out entirely.
        fs::write(
            home.join("server/instances/newer.json"),
            format!(
                r#"{{"server_id":"newer","pid":2,"host":"127.0.0.1","port":{p2},"started_at":200,"host_version":"0.41.0"}}"#
            ),
        )
        .unwrap();
        let filtered = daemon_candidates(&home, Some("0.42.0"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].port, p1);
        let _ = fs::remove_dir_all(&home);
    }
}
