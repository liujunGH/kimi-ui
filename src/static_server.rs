//! Minimal loopback static file server for the customized kimi-web bundle.
//!
//! The shell builds the SPA from the fork (see scripts/build-web.sh) and
//! serves it from 127.0.0.1 so the URL-hash credential handoff works exactly
//! like the official daemon-hosted flow (`/#token=...&daemon_base=...`).
//! No dependencies — std::net only. GET only, per-connection threads, SPA
//! fallback to index.html for client-side routes, path-traversal guarded.
//!
//! Assets come from an `AssetSource`: a directory on disk (dev builds) or an
//! in-memory map (release builds embed web-dist/ into the exe for
//! single-file distribution).

use std::{
    borrow::Cow,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::Arc,
    thread,
};
#[cfg(debug_assertions)]
use std::{fs, path::PathBuf};
#[cfg(not(debug_assertions))]
use std::collections::HashMap;

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") | Some("map") => "application/json; charset=utf-8",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("wasm") => "application/wasm",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Where served assets come from.
pub enum AssetSource {
    /// A directory on disk (dev builds: the freshly built web-dist/).
    #[cfg(debug_assertions)]
    Dir(PathBuf),
    /// In-memory map (release builds: web-dist/ embedded into the exe).
    /// Keys are `/`-separated paths relative to the bundle root.
    #[cfg(not(debug_assertions))]
    Memory(HashMap<String, (&'static [u8], &'static str)>),
}

impl AssetSource {
    /// Build an in-memory source from `(relative_path, bytes)` pairs.
    #[cfg(not(debug_assertions))]
    pub fn from_memory<I, S>(files: I) -> Self
    where
        I: IntoIterator<Item = (S, &'static [u8])>,
        S: Into<String>,
    {
        let mut map = HashMap::new();
        for (path, bytes) in files {
            // Embedded dirs may carry platform separators; normalize.
            let key = path.into().replace('\\', "/");
            let key = key.strip_prefix("./").unwrap_or(&key).to_string();
            let mime = content_type(Path::new(&key));
            map.insert(key, (bytes, mime));
        }
        AssetSource::Memory(map)
    }

    /// Fetch an asset by URL path; SPA client-side routes fall back to
    /// index.html. `None` only when even index.html is absent.
    fn lookup(&self, url_path: &str) -> Option<(Cow<'_, [u8]>, &'static str)> {
        match self {
            #[cfg(debug_assertions)]
            AssetSource::Dir(root) => {
                let file = resolve(root, url_path);
                fs::read(&file)
                    .ok()
                    .map(|body| (Cow::Owned(body), content_type(&file)))
            }
            #[cfg(not(debug_assertions))]
            AssetSource::Memory(map) => {
                let clean = url_path.trim_start_matches('/');
                let clean = if clean.is_empty() { "index.html" } else { clean };
                map.get(clean)
                    .or_else(|| map.get(&format!("{clean}/index.html")))
                    .or_else(|| map.get("index.html"))
                    .map(|(body, mime)| (Cow::Borrowed(*body), *mime))
            }
        }
    }
}

#[cfg(debug_assertions)]
fn resolve(root: &Path, url_path: &str) -> PathBuf {
    let clean = url_path.trim_start_matches('/');
    let mut candidate = root.join(clean);
    // SPA client-side routes fall back to index.html.
    if candidate.is_dir() {
        candidate = candidate.join("index.html");
    }
    if !candidate.is_file() {
        candidate = root.join("index.html");
    }
    candidate
}

fn handle(mut stream: TcpStream, source: &AssetSource) {
    let mut buf = [0u8; 8192];
    let Ok(n) = stream.read(&mut buf) else { return };
    let request = String::from_utf8_lossy(&buf[..n]);
    let Some(line) = request.lines().next() else { return };
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target), _) = (parts.next(), parts.next(), parts.next()) else {
        return;
    };
    if method != "GET" && method != "HEAD" {
        let _ = stream.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n");
        return;
    }
    let path_only = target.split(['?', '#']).next().unwrap_or("/");
    // Percent-decode (the only escapes we expect in asset paths).
    let decoded = {
        let mut out = String::new();
        let mut chars = path_only.chars();
        while let Some(c) = chars.next() {
            if c == '%' {
                let h: String = chars.by_ref().take(2).collect();
                out.push(char::from_u32(u32::from_str_radix(&h, 16).unwrap_or(37)).unwrap_or('%'));
            } else {
                out.push(c);
            }
        }
        out
    };
    // Path traversal guard: reject anything escaping the root.
    let normalized = decoded.replace('\\', "/");
    if normalized.split('/').any(|seg| seg == "..") {
        let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
        return;
    }

    match source.lookup(&normalized) {
        Some((body, mime)) => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            if method == "GET" {
                let _ = stream.write_all(&body);
            }
        }
        None => {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        }
    }
}

/// Serve `source` on 127.0.0.1 in the background; returns the bound port.
/// Falls back to an OS-assigned port when the preferred one is taken.
pub fn serve(source: AssetSource, preferred_port: u16) -> std::io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", preferred_port))
        .or_else(|_| TcpListener::bind(("127.0.0.1", 0)))?;
    let port = listener.local_addr()?.port();
    let source = Arc::new(source);
    thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(stream) = stream {
                let source = Arc::clone(&source);
                thread::spawn(move || handle(stream, &source));
            }
        }
    });
    Ok(port)
}
