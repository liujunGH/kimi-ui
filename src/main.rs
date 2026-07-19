//! kimi-ui: minimal desktop shell for the Kimi Code web UI.
//!
//! Launch flow:
//!   1. open a window with a tiny local placeholder page immediately;
//!   2. on a background thread, run `kimi server run` (spawns or reuses the
//!      local daemon), then read host/port from `server/lock` and the bearer
//!      token from `server.token` under $KIMI_CODE_HOME (default ~/.kimi-code);
//!   3. navigate the main webview to `http://host:port/#token=<token>` (plus
//!      `?kimi_desktop&platform=darwin` on macOS).
//!
//! Window layout (nothing shifts the SPA):
//!   - a bare window holds two child webviews;
//!   - the main webview (the official web UI) always stops `STRIP` px short
//!     of the window's bottom edge — it never resizes afterwards;
//!   - a transparent, shell-owned "status" webview sits in that strip. On
//!     demand it grows upward *over* the main webview (the main webview
//!     does NOT move) to float a card: context-usage detail or the 蜂群
//!     (swarm) roster. It talks to the daemon directly (REST + its own
//!     WebSocket), so it has ZERO DOM coupling to the SPA and is the
//!     shell's extensible UI surface.
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
//!   - streaming thinking blocks height-capped (no more chat climbing);
//!   - double-digit ordered-list numbers unclipped;
//!   - watchdog warns once if the SPA's desktop classes vanish.
//!
//! NOTE: the main page is a *remote* origin to Tauri, so `capabilities/`
//! must list the daemon URL under `remote.urls` — otherwise every IPC
//! invoke from the injected script is silently denied.
//!
//! The daemon exits by itself 60s after the last client disconnects, so the
//! shell does not manage its lifecycle.

use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering},
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

/// Unread-notification count shown on the Dock icon (macOS).
static BADGE_COUNT: AtomicU32 = AtomicU32::new(0);
/// Which overlay card is open: 0 = none, 1 = usage detail, 2 = swarm roster.
static OVERLAY_MODE: AtomicU8 = AtomicU8::new(0);

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

  // 3. Drag-region mirroring, badge hiding, thinking cap, list fix.
  function patchDom() {
    var els = document.querySelectorAll('.side.macos-desktop .ch, .chat-header.macos-desktop');
    for (var i = 0; i < els.length; i++) {
      if (els[i].getAttribute('data-tauri-drag-region') !== 'deep') {
        els[i].setAttribute('data-tauri-drag-region', 'deep');
      }
    }
    var pills = document.querySelectorAll('.internal-build-tag');
    for (var j = 0; j < pills.length; j++) {
      pills[j].style.display = 'none';
    }
    var streaming = document.querySelectorAll('.tc-wrap:not(.is-collapsed) pre.tc');
    for (var k = 0; k < streaming.length; k++) {
      streaming[k].style.maxHeight = '9em';
      streaming[k].style.overflowY = 'auto';
    }
    var collapsed = document.querySelectorAll('.tc-wrap.is-collapsed pre.tc');
    for (var m = 0; m < collapsed.length; m++) {
      collapsed[m].style.maxHeight = '';
      collapsed[m].style.overflowY = '';
    }
    var ols = document.querySelectorAll('.md ol');
    for (var n = 0; n < ols.length; n++) {
      if (ols[n].style.paddingLeft !== '2.2em') ols[n].style.paddingLeft = '2.2em';
    }
  }
  // The observer only marks dirty and a 150ms coalesced run does the work:
  // the SPA's virtualized list mutates the DOM continuously while scrolling
  // and streaming, and running five full-document querySelectorAll per
  // mutation was the main jank source.
  var patchScheduled = false;
  function schedulePatch() {
    if (patchScheduled) return;
    patchScheduled = true;
    setTimeout(function () {
      patchScheduled = false;
      patchDom();
    }, 150);
  }
  new MutationObserver(schedulePatch).observe(document.documentElement, { childList: true, subtree: true });
  patchDom();

  // 4. Double-click on a drag region toggles maximize (zoom).
  document.addEventListener('dblclick', function (e) {
    var t = e.target;
    if (!t || !t.closest) return;
    if (!t.closest('[data-tauri-drag-region]')) return;
    if (t.closest('button, a, input, textarea, select, label, [role="button"], [contenteditable]')) return;
    invoke('toggle_maximize', {});
  }, true);

  // 5. Watchdog: verify each selector group this shell depends on. If an
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
      if (pill && pill.style.display !== 'none' && pill.offsetParent !== null) {
        broken.push('角标隐藏');
      }
      var tc = document.querySelector('.tc-wrap:not(.is-collapsed) pre.tc');
      if (tc && tc.style.maxHeight !== '9em') {
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
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("kimi");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let candidates = [
        home_dir().join(".kimi-code/bin/kimi"),
        PathBuf::from("/opt/homebrew/bin/kimi"),
        PathBuf::from("/usr/local/bin/kimi"),
    ];
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

/// Ensure the local daemon is running and discover its address/credentials.
fn connect_daemon() -> Result<Launch, String> {
    let kimi = find_kimi().ok_or("找不到 kimi CLI，请先安装 Kimi Code")?;
    let output = Command::new(kimi)
        .args(["server", "run"])
        .output()
        .map_err(|e| format!("执行 `kimi server run` 失败：{e}"))?;
    if !output.status.success() {
        return Err(format!(
            "`kimi server run` 退出码非零：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let home = kimi_home();
    let lock_raw = fs::read_to_string(home.join("server/lock"))
        .map_err(|e| format!("读取 server/lock 失败：{e}"))?;
    let lock: Value =
        serde_json::from_str(&lock_raw).map_err(|e| format!("解析 server/lock 失败：{e}"))?;
    let host = lock["host"].as_str().unwrap_or("127.0.0.1");
    let port = lock["port"].as_u64().unwrap_or(58627);

    let token = fs::read_to_string(home.join("server.token"))
        .map_err(|e| format!("读取 server.token 失败（可先运行一次 `kimi web`）：{e}"))?;
    let token = token.trim().to_string();

    let desktop_query = if cfg!(target_os = "macos") {
        "?kimi_desktop&platform=darwin"
    } else {
        ""
    };
    let base = format!("http://{host}:{port}");
    let url = format!("{base}/{desktop_query}#token={token}")
        .parse()
        .map_err(|e| format!("构造 web UI 地址失败：{e}"))?;
    Ok(Launch { base, token, url })
}

/// Directory holding the customized kimi-web bundle (from the fork build):
/// `<exe>/../Resources/web` when packaged, `<project>/web-dist` in dev.
fn web_root() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(res) = exe
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("Resources/web"))
        {
            if res.is_dir() {
                return Some(res);
            }
        }
    }
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web-dist");
    dev.is_dir().then_some(dev)
}

/// URL of the customized UI served by our loopback static server, with the
/// token and live daemon origin handed over via the URL hash (same handoff
/// shape the official daemon-hosted flow uses).
fn custom_ui_url(base: &str, token: &str) -> Option<Url> {
    let root = web_root()?;
    let port = static_server::serve(root, 58628).ok()?;
    let enc_base = base.replace(':', "%3A").replace('/', "%2F");
    let desktop_query = if cfg!(target_os = "macos") {
        "?kimi_desktop&platform=darwin"
    } else {
        ""
    };
    format!("http://127.0.0.1:{port}/{desktop_query}&daemon_base={enc_base}#token={token}")
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
    }
}

/// The status webview asks for daemon connection details once it boots.
#[tauri::command]
fn daemon_info(state: tauri::State<'_, SharedDaemon>) -> Result<Value, String> {
    let guard = state.lock().map_err(|e| e.to_string())?;
    let s = guard.as_ref().ok_or_else(|| "daemon 尚未就绪".to_string())?;
    Ok(serde_json::json!({ "base": s.base, "token": s.token }))
}

/// Open/close an overlay card in the status webview ("none" | "usage" | "swarm").
#[tauri::command]
fn set_overlay(app: tauri::AppHandle, mode: String) {
    let mode = match mode.as_str() {
        "usage" => 1u8,
        "swarm" => 2u8,
        _ => 0u8,
    };
    OVERLAY_MODE.store(mode, Ordering::Relaxed);
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
    let overlay = OVERLAY_MODE.load(Ordering::Relaxed) != 0;
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
    let out = Command::new(gh)
        .args([
            "api",
            "repos/liujunGH/kimi-ui/releases/latest",
            "--jq",
            ".tag_name + \" \" + .html_url",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())?;
    let (tag, url) = out.split_once(' ')?;
    let latest = tag.trim_start_matches('v').to_string();
    Some(UpdateInfo {
        has_update: version_newer(&latest, env!("CARGO_PKG_VERSION")),
        latest,
        url: url.to_string(),
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
            open_url
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
                    }
                    _ => {}
                });
            }

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
                    let msg = serde_json::to_string(&format!("启动失败：{e}"))
                        .unwrap_or_else(|_| "\"启动失败\"".to_string());
                    let _ = main_wv.eval(&format!("window.__kimiBootError({msg})"));
                }
            });
            start_update_check();
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running kimi-ui");
}

// ---------------------------------------------------------------------------
// Plan-usage scraping.
//
// There is NO REST/cloud endpoint for membership quota (verified
// exhaustively), but the TUI's `/usage` command renders it. So every ~10min
// we boot a headless TUI in an EMBEDDED PTY (with a throwaway
// KIMI_CODE_HOME holding a copy of the credentials), send `/usage`, and
// parse the rendered screen via a vt100 parser — no external tmux needed.
// ---------------------------------------------------------------------------

/// Plan quota as rendered by the TUI's `/usage`.
#[derive(Clone, serde::Serialize)]
struct PlanUsage {
    weekly_pct: u32,
    weekly_reset: String,
    hourly_pct: u32,
    hourly_reset: String,
    fetched_at: u64,
}

static PLAN_USAGE: Mutex<Option<PlanUsage>> = Mutex::new(None);
static SCRAPE_RUNNING: AtomicBool = AtomicBool::new(false);

/// Scrape TTL: the status page may ask often, we scrape at most this often.
const SCRAPE_TTL_SECS: u64 = 600;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Parse one "X% used ... resets in Y" line of the /usage box.
fn parse_usage_line(line: &str) -> Option<(u32, String)> {
    let pct_end = line.find("% used")?;
    let digits: String = line[..pct_end]
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    let pct: u32 = digits.parse().ok()?;
    let reset = line
        .split("resets in ")
        .nth(1)?
        .trim()
        .trim_matches('│')
        .trim()
        .to_string();
    Some((pct, reset))
}

/// Copy a file or directory tree (small credential dirs only).
fn copy_tree(src: &PathBuf, dst: &PathBuf) -> std::io::Result<()> {
    if src.is_dir() {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else if src.is_file() {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dst)?;
    }
    Ok(())
}

/// Spawn `program` in an embedded PTY, dismiss first-run dialogs, send
/// `input`, and return the final rendered screen via a vt100 parser
/// (same fidelity as `tmux capture-pane`, but zero external dependencies).
fn run_in_pty(
    program: &PathBuf,
    cwd: &PathBuf,
    envs: &[(&str, &str)],
    input: &str,
    boot_wait: Duration,
    after_input_wait: Duration,
) -> Result<String, String> {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    use std::io::{Read, Write};

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 50,
            cols: 200,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())?;
    let mut cmd = CommandBuilder::new(program);
    cmd.cwd(cwd);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("spawn 失败：{e}"))?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut buf = [0u8; 16384];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    thread::sleep(boot_wait);
    // Dismiss any first-run dialog (e.g. kimi-cli migration prompt).
    let _ = writer.write_all(b"\x1b");
    thread::sleep(Duration::from_millis(800));
    let _ = writer.write_all(input.as_bytes());
    let _ = writer.write_all(b"\r");
    thread::sleep(after_input_wait);

    let _ = child.kill();
    let _ = child.wait();
    drop(writer);
    thread::sleep(Duration::from_millis(300));
    let mut raw = Vec::new();
    while let Ok(chunk) = rx.try_recv() {
        raw.extend_from_slice(&chunk);
    }
    let mut parser = vt100::Parser::new(50, 200, 0);
    parser.process(&raw);
    Ok(parser.screen().contents())
}

/// Boot a headless TUI in an embedded PTY, send /usage, parse the output.
/// Takes ~10s.
fn scrape_plan_usage() -> Result<PlanUsage, String> {
    let kimi = find_kimi().ok_or("找不到 kimi CLI")?;

    // Throwaway home with a copy of the credentials, so probe sessions and
    // their junk never touch the user's real data directory.
    let sandbox = std::env::temp_dir().join(format!("kimi-usage-home-{}", std::process::id()));
    let _ = fs::remove_dir_all(&sandbox);
    fs::create_dir_all(&sandbox).map_err(|e| e.to_string())?;
    let real_home = kimi_home();
    // Migration markers and device id included so the sandboxed TUI does not
    // stop at first-run dialogs; an Escape is sent anyway as a fallback.
    for item in ["config.toml", "credentials", "oauth", "device_id", "migration-report.json"] {
        let src = real_home.join(item);
        if src.exists() {
            let _ = copy_tree(&src, &sandbox.join(item));
        }
    }
    let probe = sandbox.join("probe");
    fs::create_dir_all(&probe).map_err(|e| e.to_string())?;

    let home_str = sandbox.display().to_string();
    let result = run_in_pty(
        &kimi,
        &probe,
        &[("KIMI_CODE_HOME", home_str.as_str())],
        "/usage",
        Duration::from_secs(6),
        Duration::from_secs(4),
    );
    let _ = fs::remove_dir_all(&sandbox);
    let text = result?;

    let mut weekly = None;
    let mut hourly = None;
    for line in text.lines() {
        if line.contains("Weekly limit") {
            weekly = parse_usage_line(line);
        } else if line.contains("5h limit") {
            hourly = parse_usage_line(line);
        }
    }
    match (weekly, hourly) {
        (Some((weekly_pct, weekly_reset)), Some((hourly_pct, hourly_reset))) => Ok(PlanUsage {
            weekly_pct,
            weekly_reset,
            hourly_pct,
            hourly_reset,
            fetched_at: now_secs(),
        }),
        _ => Err("解析 /usage 输出失败（TUI 格式可能已变化）".to_string()),
    }
}

/// The status page asks for plan quota; we return the cache and refresh it
/// in the background when stale.
#[tauri::command]
fn plan_usage() -> Value {
    let stale = PLAN_USAGE
        .lock()
        .map(|u| u.as_ref().map_or(true, |u| u.fetched_at + SCRAPE_TTL_SECS < now_secs()))
        .unwrap_or(true);
    if stale && !SCRAPE_RUNNING.swap(true, Ordering::Relaxed) {
        thread::spawn(|| {
            match scrape_plan_usage() {
                Ok(u) => {
                    if let Ok(mut guard) = PLAN_USAGE.lock() {
                        *guard = Some(u);
                    }
                }
                Err(e) => eprintln!("kimi-ui: 额度抓取失败：{e}"),
            }
            SCRAPE_RUNNING.store(false, Ordering::Relaxed);
        });
    }
    let guard = PLAN_USAGE.lock().ok();
    match guard.as_ref().and_then(|g| g.as_ref()) {
        Some(u) => serde_json::to_value(u).unwrap_or_else(|_| Value::Null),
        None => serde_json::json!({ "loading": true }),
    }
}
