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
        atomic::{AtomicU32, AtomicU8, Ordering},
        Mutex,
    },
    thread,
};

use serde_json::Value;
use tauri::{LogicalPosition, LogicalSize, Manager, Url, WebviewBuilder, WebviewUrl, WindowBuilder};
use tauri_plugin_notification::NotificationExt;

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
  new MutationObserver(patchDom).observe(document.documentElement, { childList: true, subtree: true });
  patchDom();

  // 4. Double-click on a drag region toggles maximize (zoom).
  document.addEventListener('dblclick', function (e) {
    var t = e.target;
    if (!t || !t.closest) return;
    if (!t.closest('[data-tauri-drag-region]')) return;
    if (t.closest('button, a, input, textarea, select, label, [role="button"], [contenteditable]')) return;
    invoke('toggle_maximize', {});
  }, true);

  // 5. Watchdog: warn once if the SPA's desktop classes vanish (official UI
  //    update renamed them) — the drag/desktop integration needs a shell update.
  if (isDesktop) {
    var domWarned = false;
    setInterval(function () {
      if (domWarned) return;
      var header = document.querySelector('.chat-header');
      var side = document.querySelector('.side');
      if ((header && !header.classList.contains('macos-desktop'))
        || (side && !side.classList.contains('macos-desktop'))) {
        domWarned = true;
        invoke('notify', {
          title: 'Kimi Code',
          body: '检测到官方界面结构更新，窗口拖拽和桌面布局可能已失效，请更新桌面壳'
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
fn focus_window(window: tauri::WebviewWindow) {
    let _ = window.unminimize();
    if let Err(e) = window.set_focus() {
        eprintln!("kimi-ui: 激活窗口失败：{e}");
    }
}

#[tauri::command]
fn toggle_maximize(window: tauri::WebviewWindow) {
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

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .manage(SharedDaemon::new(None))
        .invoke_handler(tauri::generate_handler![
            notify,
            focus_window,
            toggle_maximize,
            daemon_info,
            set_overlay
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
                    if let Some(state) = app_handle.try_state::<SharedDaemon>() {
                        *state.lock().unwrap() = Some(DaemonState {
                            base: launch.base,
                            token: launch.token,
                        });
                    }
                    if let Err(e) = main_wv.navigate(launch.url) {
                        eprintln!("kimi-ui: 打开 web UI 失败：{e}");
                    }
                }
                Err(e) => {
                    let msg = serde_json::to_string(&format!("启动失败：{e}"))
                        .unwrap_or_else(|_| "\"启动失败\"".to_string());
                    let _ = main_wv.eval(&format!("window.__kimiBootError({msg})"));
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running kimi-ui");
}
