use std::path::{Path, PathBuf};
use tiny_http::{Header, Request, Response};

const DEFAULT_UI_DIR: &str = "/data/admin";

fn ui_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("ZTE_AGENT_UI_DIR").unwrap_or_else(|_| DEFAULT_UI_DIR.to_string()),
    )
}

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "html" => "text/html; charset=utf-8",
        "js" => "application/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "txt" => "text/plain",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

fn send_404(request: Request) {
    let _ = request.respond(
        Response::from_string("404 Not Found")
            .with_status_code(404u16)
            .with_header(Header::from_bytes("Content-Type", "text/plain").unwrap()),
    );
}

pub fn serve(request: Request, path: &str, head: bool) {
    // UI is served at the site root: "/" → index.html, "/login/" → login/index.html.
    let rel = path.strip_prefix('/').unwrap_or(path);

    // Path traversal guard: reject any segment that is ".."
    for seg in rel.split('/') {
        if seg == ".." {
            send_404(request);
            return;
        }
    }

    let root = ui_dir();

    // Resolve candidate path
    let candidate: PathBuf = if rel.is_empty() {
        root.clone()
    } else {
        root.join(rel)
    };

    // Determine whether the path has a file extension
    let has_ext = Path::new(rel)
        .extension()
        .map(|e| !e.is_empty())
        .unwrap_or(false);

    // Try to resolve to a concrete file path
    let file_path = resolve_file(&root, &candidate, has_ext);

    let file_path = match file_path {
        Some(p) => p,
        None => {
            send_404(request);
            return;
        }
    };

    // Canonical-path check: ensure resolved path is still under root
    // Use the resolved path directly (no symlink resolution needed here; the
    // device rootfs doesn't have symlink tricks under /data/admin).
    if !file_path.starts_with(&root) {
        send_404(request);
        return;
    }

    let ext = file_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let mime = mime_for(ext);

    // Cache-Control: immutable for hashed Next.js static assets, no-cache elsewhere
    let cache_control = if path.starts_with("/_next/static/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };

    // HEAD (Next.js <Link> prefetch): confirm existence with headers, no body.
    if head {
        let _ = request.respond(
            Response::empty(200u16)
                .with_header(Header::from_bytes("Content-Type", mime).unwrap())
                .with_header(Header::from_bytes("Cache-Control", cache_control).unwrap()),
        );
        return;
    }

    let file = match std::fs::File::open(&file_path) {
        Ok(f) => f,
        Err(_) => {
            send_404(request);
            return;
        }
    };

    let _ = request.respond(
        Response::from_file(file)
            .with_status_code(200u16)
            .with_header(Header::from_bytes("Content-Type", mime).unwrap())
            .with_header(Header::from_bytes("Cache-Control", cache_control).unwrap()),
    );
}

/// Try to find a readable file at or under `candidate`.
/// Returns `None` if nothing found (caller should 404).
fn resolve_file(root: &Path, candidate: &Path, has_ext: bool) -> Option<PathBuf> {
    if has_ext {
        // Exact file expected; either it exists or 404
        if candidate.is_file() {
            Some(candidate.to_path_buf())
        } else {
            None
        }
    } else {
        // No extension: try <candidate>/index.html first
        let index = candidate.join("index.html");
        if index.is_file() {
            return Some(index);
        }
        // SPA fallback to root index.html
        let spa = root.join("index.html");
        if spa.is_file() {
            Some(spa)
        } else {
            None
        }
    }
}
