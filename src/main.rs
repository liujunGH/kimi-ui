//! kimi-ui: minimal desktop shell for the Kimi Code web UI.
//!
//! Launch flow:
//!   1. open a window with a tiny local placeholder page immediately;
//!   2. on a background thread, run `kimi server run` (spawns or reuses the
//!      local daemon), then read host/port from `server/lock` and the bearer
//!      token from `server.token` under $KIMI_CODE_HOME (default ~/.kimi-code);
//!   3. navigate the window to `http://host:port/#token=<token>` (plus
//!      `?kimi_desktop&platform=darwin` on macOS, see below).
//!
//! Desktop integrations (injected script + native commands):
//!   - `window.Notification` polyfill -> native notifications (WKWebView has
//!     none, so the SPA's completion/question/approval alerts are lost);
//!   - `window.focus()` -> raise the native window;
//!   - downloads land in ~/Downloads with de-duplicated filenames;
//!   - external links open in the system browser;
//!   - on macOS the window uses a hidden-inset title bar and the SPA's
//!     `macos-desktop` layout; the SPA's `-webkit-app-region` drag areas (wry
//!     ignores that CSS) are mirrored to Tauri's `data-tauri-drag-region`, and
//!     the "internal testing" badge that `?kimi_desktop` enables is hidden.
//!
//! The daemon exits by itself 60s after the last client disconnects, so the
//! shell does not manage its lifecycle.

use std::{fs, path::PathBuf, process::Command, thread};

use serde_json::Value;
use tauri::{Url, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_notification::NotificationExt;

/// Script injected at document start on every page (WKUserScript is not
/// affected by the SPA's CSP).
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

  // 2. window.focus() -> raise the native window (SPA calls it when a
  //    notification is clicked).
  window.focus = function () { invoke('focus_window', {}); };

  // 3. Mirror the SPA's -webkit-app-region drag areas to Tauri drag regions,
  //    and hide the internal-build badge (direct style manipulation: the
  //    page CSP may block injected <style> elements).
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
  }
  new MutationObserver(patchDom).observe(document.documentElement, { childList: true, subtree: true });
  patchDom();
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

/// Ensure the local daemon is running and build the web UI URL.
fn launch_url() -> Result<Url, String> {
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
    let token = token.trim();

    // On macOS the window uses a hidden-inset title bar, so opt into the
    // SPA's desktop layout (its macos-desktop styles reserve space for the
    // traffic lights); the badge this enables is hidden by the init script.
    let desktop_query = if cfg!(target_os = "macos") {
        "?kimi_desktop&platform=darwin"
    } else {
        ""
    };
    format!("http://{host}:{port}/{desktop_query}#token={token}")
        .parse()
        .map_err(|e| format!("构造 web UI 地址失败：{e}"))
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

#[tauri::command]
fn notify(app: tauri::AppHandle, title: String, body: String) {
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        eprintln!("kimi-ui: 通知发送失败：{e}");
    }
}

#[tauri::command]
fn focus_window(window: tauri::WebviewWindow) {
    let _ = window.unminimize();
    if let Err(e) = window.set_focus() {
        eprintln!("kimi-ui: 激活窗口失败：{e}");
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![notify, focus_window])
        .setup(|app| {
            let builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("Kimi Code")
                .inner_size(1280.0, 840.0)
                .min_inner_size(860.0, 560.0)
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
                });
            #[cfg(target_os = "macos")]
            let builder = builder
                .title_bar_style(tauri::TitleBarStyle::Overlay)
                .hidden_title(true);
            let window = builder.build()?;

            thread::spawn(move || match launch_url() {
                Ok(url) => {
                    if let Err(e) = window.navigate(url) {
                        eprintln!("kimi-ui: 打开 web UI 失败：{e}");
                    }
                }
                Err(e) => {
                    let msg = serde_json::to_string(&format!("启动失败：{e}"))
                        .unwrap_or_else(|_| "\"启动失败\"".to_string());
                    let _ = window.eval(&format!("window.__kimiBootError({msg})"));
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running kimi-ui");
}
