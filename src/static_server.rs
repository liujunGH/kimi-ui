//! Minimal loopback static file server for the customized kimi-web bundle.
//!
//! The shell builds the SPA from the fork (see scripts/build-web.sh) and
//! serves it from 127.0.0.1 so the URL-hash credential handoff works exactly
//! like the official daemon-hosted flow (`/#token=...&daemon_base=...`).
//! No dependencies — std::net only. GET only, per-connection threads, SPA
//! fallback to index.html for client-side routes, path-traversal guarded.

use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    thread,
};

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

fn handle(mut stream: TcpStream, root: &Path) {
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

    let file = resolve(root, &normalized);
    match fs::read(&file) {
        Ok(body) => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\n\r\n",
                content_type(&file),
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            if method == "GET" {
                let _ = stream.write_all(&body);
            }
        }
        Err(_) => {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        }
    }
}

/// Serve `root` on 127.0.0.1 in the background; returns the bound port.
/// Falls back to an OS-assigned port when the preferred one is taken.
pub fn serve(root: PathBuf, preferred_port: u16) -> std::io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", preferred_port))
        .or_else(|_| TcpListener::bind(("127.0.0.1", 0)))?;
    let port = listener.local_addr()?.port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(stream) = stream {
                let root = root.clone();
                thread::spawn(move || handle(stream, &root));
            }
        }
    });
    Ok(port)
}
