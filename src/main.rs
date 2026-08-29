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

/// Status strip height (collapsed) and overlay height (card open).
const STRIP: f64 = 28.0;
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
fn attach(home: &Path, cli_version: Option<&str>) -> Result<Launch, String> {
    let addr = discover_daemon(home, cli_version)?;
    launch_at(home, addr)
}

/// Build launch details for a specific reachable server.
fn launch_at(home: &Path, addr: DaemonAddr) -> Result<Launch, String> {
    let token = fs::read_to_string(home.join("server.token"))
        .map_err(|e| format!("读取 server.token 失败：{e}"))?;
    let token = token.trim().to_string();

    let desktop_query = desktop_query();
    let base = format!("http://{}:{}", addr.host, addr.port);
    let url = format!("{base}/{desktop_query}#token={token}")
        .parse()
        .map_err(|e| format!("构造 web UI 地址失败：{e}"))?;
    Ok(Launch { base, token, url })
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

/// Discover a live daemon address: scan the multi-instance registry
/// (`server/instances/*.json`, longest-running first), then fall back to the
/// legacy single-instance `server/lock`. Mirrors kap-server's own discovery
/// order (`packages/kap-server/src/instanceRegistry.ts`). Every candidate is
/// verified with a TCP connect, so stale files from crashed daemons are
/// skipped instead of fatal. Daemons older than the installed CLI are skipped
/// too, so an upgraded CLI is not shadowed by a long-running old server.
fn discover_daemon(home: &Path, cli_version: Option<&str>) -> Result<DaemonAddr, String> {
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

    for (_, host, port) in candidates {
        if tcp_alive(&host, port) {
            return Ok(DaemonAddr { host, port });
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

/// Wire the standard behaviors onto a main-webview builder.
fn main_webview_builder() -> WebviewBuilder<tauri::Wry> {
    WebviewBuilder::new("main", WebviewUrl::App("index.html".into()))
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
    if let Some(wv) = app.get_webview("main") {
        let _ = wv.eval(if frozen { FREEZE_JS } else { UNFREEZE_JS });
    }
}

/// Toggle Safari Web Inspector on the main webview (memory/DOM profiling).
#[tauri::command]
fn toggle_devtools(app: tauri::AppHandle) {
    if let Some(wv) = app.get_webview("main") {
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

/// The status webview asks for daemon connection details once it boots.
#[tauri::command]
fn daemon_info(state: tauri::State<'_, SharedDaemon>) -> Result<Value, String> {
    let guard = state.lock().map_err(|e| e.to_string())?;
    let s = guard.as_ref().ok_or_else(|| "daemon 尚未就绪".to_string())?;
    Ok(serde_json::json!({ "base": s.base, "token": s.token }))
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

/// Recompute the webviews' bounds. The main webview never moves; the status
/// webview slides between the collapsed strip and the overlay height.
fn layout_strip(app: &tauri::AppHandle) {
    let Some(window) = app.get_window("main") else { return };
    let (Some(main_wv), Some(status_wv)) = (app.get_webview("main"), app.get_webview("status"))
    else {
        return;
    };
    let (Ok(size), Ok(scale)) = (window.inner_size(), window.scale_factor()) else { return };
    let w = size.width as f64 / scale;
    let h = size.height as f64 / scale;
    let overlay = OVERLAY_OPEN.load(Ordering::Relaxed);
    let status_h = if overlay { OVERLAY_H } else { STRIP };
    let _ = main_wv.set_size(LogicalSize::new(w, (h - STRIP).max(240.0)));
    let _ = status_wv.set_position(LogicalPosition::new(0.0, h - status_h));
    let _ = status_wv.set_size(LogicalSize::new(w, status_h));
}

// ---------------------------------------------------------------------------
// Update check: compares the latest GitHub release tag with CARGO_PKG_VERSION.
// Uses the gh CLI (carries the user's GitHub auth — the repo is private) and
// falls back to the anonymous API once the repo is public.
// ---------------------------------------------------------------------------

/// Latest-release info exposed to the status page.
#[derive(Clone, serde::Serialize)]
struct UpdateInfo {
    latest: String,
    url: String,
    has_update: bool,
    /// Release notes (markdown), rendered by the status page's update card.
    notes: String,
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

fn fetch_latest_release() -> Option<UpdateInfo> {
    let gh = find_executable("gh")?;
    let out = no_console(Command::new(gh))
        .args(["api", "repos/liujunGH/kimi-ui/releases/latest"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let json: Value = serde_json::from_slice(&out.stdout).ok()?;
    let latest = json["tag_name"].as_str()?.trim_start_matches('v').to_string();
    let url = json["html_url"].as_str()?.to_string();
    let notes = json["body"].as_str().unwrap_or("").trim().to_string();
    Some(UpdateInfo {
        has_update: version_newer(&latest, env!("CARGO_PKG_VERSION")),
        latest,
        url,
        notes,
    })
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

#[tauri::command]
fn open_url(url: String) {
    if let Ok(u) = url.parse::<Url>() {
        open_in_system_browser(&u);
    }
}

// ---------------------------------------------------------------------------
// Active session & window title.
//
// The injected script reports the SPA's route (`/sessions/<id>` or the
// new-session root) via `set_active_session`. A 30s refresher keeps an ordered
// (id, title) list from `GET /api/v1/sessions` which feeds both the window
// title — visible in Cmd-Tab / Mission Control even though the title bar text
// itself is hidden — and the dynamic 会话 (recent sessions) menu.
// ---------------------------------------------------------------------------

/// Session the SPA is currently viewing, from its URL route.
static ACTIVE_SESSION: Mutex<Option<String>> = Mutex::new(None);
/// Ordered (id, title) pairs of recent non-archived sessions (API order).
static SESSION_TITLES: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// How many recent sessions the 会话 menu shows.
const MENU_SESSIONS: usize = 8;

fn daemon_state(app: &tauri::AppHandle) -> Option<DaemonState> {
    app.try_state::<SharedDaemon>()
        .and_then(|s| s.lock().ok().and_then(|g| g.clone()))
}

/// Recent non-archived sessions as ordered (id, title) pairs.
fn fetch_sessions(daemon: &DaemonState) -> Result<Vec<(String, String)>, String> {
    let v = http_get_json(&daemon.base, "/api/v1/sessions?page_size=50", &daemon.token)?;
    parse_sessions(&v).ok_or_else(|| "会话列表结构可能已变化".to_string())
}

/// `(id, title)` pairs from a `GET /api/v1/sessions` payload; None when the
/// envelope shape drifted.
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

/// Truncate on char boundaries so CJK titles never panic mid-codepoint.
fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// "<session title> — Kimi Code", or the plain app name without one.
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

fn refresh_window_title(app: &tauri::AppHandle) {
    if let Some(window) = app.get_window("main") {
        let title = window_title_text();
        let _ = window.set_title(&title);
    }
}

/// Fetch the session list once, then update the title and menu. Cheap
/// loopback call, so running it on every route change is fine.
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

/// Called by the injected script whenever the SPA's route changes.
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

// ---------------------------------------------------------------------------
// Native menu bar.
//
// macOS top level must be submenus only. The 会话 submenu is rebuilt
// wholesale (app.set_menu) whenever the recent-session list changes — menu
// types are not storable off the main thread, and full rebuilds are rare
// because they are skipped when the list did not change.
// ---------------------------------------------------------------------------

/// Last session list the menu was built from; skip rebuilds when unchanged.
static MENU_BUILT: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

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

/// Rebuild the menu when the recent-sessions list changed. Runs the build on
/// the main thread — NSApplication's main menu must only be touched there.
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

/// In-page SPA navigation: calling the SPA's own wrapped history.pushState
/// (our document-start wrapper sits beneath it) makes the router react
/// without a reload; the catch fallback covers a not-yet-booted page.
fn eval_main_navigation(app: &tauri::AppHandle, path: &str) {
    if let Some(wv) = app.get_webview("main") {
        let path_json = serde_json::json!(path).to_string();
        let _ = wv.eval(&format!(
            "try{{history.pushState({{}},'',{path_json})}}catch(e){{location.assign({path_json})}}"
        ));
    }
}

/// Menu-bar actions. Registered once in setup; `event.id()` is the
/// MenuItem id we assigned (or "open-session:<id>").
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
        .manage(SharedDaemon::new(None))
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
            remote_control
        ])
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

            // Main webview: the official web UI, stops above the strip.
            let main_wv = window.add_child(
                main_webview_builder(),
                LogicalPosition::new(0.0, 0.0),
                LogicalSize::new(w, h - STRIP),
            )?;
            // Status webview: the shell's own UI surface (transparent so the
            // overlay cards float over the main webview).
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

            let app_handle = app.handle().clone();
            thread::spawn(move || match connect_daemon() {
                Ok(launch) => {
                    let url = custom_ui_url(&launch.base, &launch.token)
                        .unwrap_or(launch.url);
                    if let Some(state) = app_handle.try_state::<SharedDaemon>() {
                        *state.lock().unwrap() = Some(DaemonState {
                            base: launch.base,
                            token: launch.token,
                        });
                    }
                    if let Err(e) = main_wv.navigate(url) {
                        eprintln!("kimi-ui: 打开 web UI 失败：{e}");
                    }
                }
                Err(e) => {
                    let msg = serde_json::to_string(&e)
                        .unwrap_or_else(|_| "{\"kind\":\"unknown\"}".to_string());
                    let _ = main_wv.eval(&format!("window.__kimiBootError({msg})"));
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
                        let _ = child.kill();
                        let _ = child.wait();
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
fn extract_plan_usage(data: &Value) -> Result<PlanUsage, String> {
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
    let stale = PLAN_USAGE
        .lock()
        .map(|u| u.as_ref().map_or(true, |u| u.fetched_at + FETCH_TTL_SECS < now_secs()))
        .unwrap_or(true);
    if stale && !FETCH_RUNNING.swap(true, Ordering::Relaxed) {
        thread::spawn(move || {
            match http_get_json(&daemon.base, "/api/v1/oauth/usage", &daemon.token)
                .and_then(|v| extract_plan_usage(&v["data"]))
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
        let plan = extract_plan_usage(&data).unwrap();
        assert_eq!(plan.weekly_pct, 3);
        assert_eq!(plan.hourly_pct, 4); // 7/200 rounds to 4%
        assert_eq!(plan.weekly_reset_at, "2026-08-29T13:17:18Z");
        assert_eq!(plan.hourly_reset_at, "2026-08-23T19:17:18Z");
    }

    #[test]
    fn oauth_usage_rows_are_matched_by_name_when_window_missing() {
        let data: Value = serde_json::from_str(
            r#"{"kind":"ok","summary":null,
                "limits":[{"name":"Weekly limit","used":50,"limit":100},
                          {"name":"5h limit","used":10,"limit":100}]}"#,
        )
        .unwrap();
        let plan = extract_plan_usage(&data).unwrap();
        assert_eq!(plan.weekly_pct, 50);
        assert_eq!(plan.hourly_pct, 10);
    }

    #[test]
    fn oauth_usage_error_and_missing_rows_are_rejected() {
        let err: Value =
            serde_json::from_str(r#"{"kind":"error","message":"not logged in"}"#).unwrap();
        assert!(extract_plan_usage(&err).unwrap_err().contains("not logged in"));
        let partial: Value = serde_json::from_str(
            r#"{"kind":"ok","summary":null,
                "limits":[{"window":{"duration":1,"unit":"week"},"used":1,"limit":10}]}"#,
        )
        .unwrap();
        assert!(extract_plan_usage(&partial).is_err());
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
}
