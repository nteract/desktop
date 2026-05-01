//! HTTP read server for the blob store.
//!
//! Serves blobs by hash over unauthenticated localhost HTTP. This is safe
//! because blobs are content-addressed (256-bit hashes are not guessable),
//! the endpoint is read-only, and the data is non-secret (notebook outputs
//! the user produced locally).
//!
//! Endpoints:
//! - `GET /blob/{hash}` — raw bytes with `Content-Type` from metadata
//! - `GET /plugins/{name}` — embedded renderer plugin assets (JS/CSS)
//! - `GET /health` — 200 OK
//!
//! The server tries to bind a stable per-channel preferred port first
//! (see `runt_workspace::preferred_blob_port`), bumping to the next port on
//! collision (up to 10 attempts) before falling back to `127.0.0.1:0`. A
//! stable port keeps frozen MCP App CSPs pointing at a working origin across
//! daemon restarts. The server runs on the caller's tokio runtime and shuts
//! down when the process exits; no explicit cancellation is implemented yet.

use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tracing::{error, info, warn};

use crate::blob_store::BlobStore;
use crate::daemon::Daemon;
use crate::embedded_plugins;
use crate::task_supervisor::{spawn_best_effort, spawn_supervised};

/// How many consecutive ports past the preferred port we'll try before
/// falling back to an OS-assigned port. Sourced from `runt_workspace` so the
/// bump budget stays in sync with the per-channel range carve-out.
const PREFERRED_PORT_ATTEMPTS: u16 = runt_workspace::PREFERRED_BLOB_PORT_RANGE;

/// Start the blob HTTP server.
///
/// Returns the port the server is listening on. The server runs as a
/// spawned task on the current tokio runtime.
///
/// When `daemon` is provided, a panic in the accept loop triggers shutdown.
/// Pass `None` in tests where no daemon is available.
///
/// When `use_preferred_port` is `true`, binds against the channel's
/// preferred port with a bounded bump range before falling back to an
/// OS-assigned port. When `false`, skips straight to OS-assigned.
/// Integration tests and other contexts that run dozens of daemons in
/// close proximity should pass `false` to avoid `EADDRINUSE` retry
/// chains pushing boot past their `wait_for_daemon` timeout.
pub async fn start_blob_server(
    store: Arc<BlobStore>,
    daemon: Option<Arc<Daemon>>,
    use_preferred_port: bool,
) -> std::io::Result<u16> {
    let listener = if use_preferred_port {
        bind_preferred_or_random().await?
    } else {
        TcpListener::bind("127.0.0.1:0").await?
    };
    start_blob_server_with_listener(listener, store, daemon).await
}

/// Start the blob HTTP server on an already-bound listener. Exposed for tests
/// that want to pin the server to a random OS-assigned port.
async fn start_blob_server_with_listener(
    listener: TcpListener,
    store: Arc<BlobStore>,
    daemon: Option<Arc<Daemon>>,
) -> std::io::Result<u16> {
    let port = listener.local_addr()?.port();

    info!("[blob-server] Listening on http://127.0.0.1:{}", port);

    spawn_supervised(
        "blob-accept-loop",
        async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let store = store.clone();
                        let io = TokioIo::new(stream);
                        spawn_best_effort("blob-connection", async move {
                            let service = service_fn(move |req| handle_request(req, store.clone()));
                            if let Err(e) =
                                http1::Builder::new().serve_connection(io, service).await
                            {
                                if !e.is_incomplete_message() && !e.is_canceled() {
                                    error!("[blob-server] Connection error: {}", e);
                                }
                            }
                        });
                    }
                    Err(e) => {
                        error!("[blob-server] Accept error: {}", e);
                    }
                }
            }
        },
        move |_| {
            if let Some(d) = daemon {
                tokio::spawn(async move { d.trigger_shutdown().await });
            }
        },
    );

    Ok(port)
}

/// Bind the blob server port.
///
/// Tries the channel's preferred port first, bumps to the next port on
/// `EADDRINUSE`, and falls back to an OS-assigned port after
/// `PREFERRED_PORT_ATTEMPTS` consecutive collisions.
async fn bind_preferred_or_random() -> std::io::Result<TcpListener> {
    let preferred = runt_workspace::preferred_blob_port();
    for offset in 0..PREFERRED_PORT_ATTEMPTS {
        let port = preferred.saturating_add(offset);
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => return Ok(listener),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(e) => return Err(e),
        }
    }
    warn!(
        "[blob-server] preferred ports {}..{} all in use, falling back to OS-assigned port",
        preferred,
        preferred.saturating_add(PREFERRED_PORT_ATTEMPTS - 1)
    );
    TcpListener::bind("127.0.0.1:0").await
}

/// Handle a single HTTP request.
async fn handle_request(
    req: Request<hyper::body::Incoming>,
    store: Arc<BlobStore>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let path = req.uri().path();
    let method = req.method();

    let response = if method != Method::GET {
        text_response(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed")
    } else if path == "/health" {
        text_response(StatusCode::OK, "OK")
    } else if let Some(hash) = path.strip_prefix("/blob/") {
        serve_blob(&store, hash).await
    } else if let Some(name) = path.strip_prefix("/plugins/") {
        serve_plugin(name).await
    } else {
        text_response(StatusCode::NOT_FOUND, "Not Found")
    };

    Ok(response)
}

/// Serve a blob by hash with correct Content-Type.
async fn serve_blob(store: &BlobStore, hash: &str) -> Response<Full<Bytes>> {
    let (blob_result, meta_result) = tokio::join!(store.get(hash), store.get_meta(hash));

    match blob_result {
        Ok(Some(data)) => {
            let content_type = meta_result
                .ok()
                .flatten()
                .map(|m| m.media_type)
                .unwrap_or_else(|| "application/octet-stream".to_string());

            Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", content_type)
                .header("Content-Length", data.len().to_string())
                .header("Cache-Control", "public, max-age=31536000, immutable")
                .header("Access-Control-Allow-Origin", "*")
                .header("X-Content-Type-Options", "nosniff")
                .body(Full::new(Bytes::from(data)))
                .unwrap_or_else(|_| {
                    text_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
                })
        }
        Ok(None) => text_response(StatusCode::NOT_FOUND, "Not Found"),
        Err(_) => text_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
    }
}

/// Serve a renderer plugin asset.
///
/// `.js` responses are wrapped in the MCP App IIFE loader on the way out
/// (see [`wrap_for_mcp_app`]). `.css` / `.wasm` are passed through untouched.
/// Dev worktree daemons prefer the workspace's on-disk plugin assets so
/// `cargo xtask renderer-plugins` can refresh bundles without a daemon
/// rebuild; release builds and dev builds missing an on-disk asset fall
/// through to the embedded copy.
async fn serve_plugin(name: &str) -> Response<Full<Bytes>> {
    if !is_valid_plugin_name(name) {
        return text_response(StatusCode::NOT_FOUND, "Not Found");
    }

    let dev_assets_dir = dev_plugin_assets_dir();
    serve_plugin_with_dev_assets(name, dev_assets_dir.as_deref()).await
}

async fn serve_plugin_with_dev_assets(
    name: &str,
    dev_assets_dir: Option<&Path>,
) -> Response<Full<Bytes>> {
    if !is_valid_plugin_name(name) {
        return text_response(StatusCode::NOT_FOUND, "Not Found");
    }

    if let Some(dir) = dev_assets_dir {
        if let Some(response) = serve_dev_plugin_file(name, dir).await {
            return response;
        }
    }

    serve_embedded_plugin(name)
}

fn is_valid_plugin_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && !name.contains("..")
}

fn dev_plugin_assets_dir() -> Option<PathBuf> {
    if !runt_workspace::is_dev_mode() {
        return None;
    }

    let workspace = runt_workspace::get_workspace_path()?;
    // Canonical dev source: the notebook renderer-plugin dir. The raw CJS
    // bundles living here are wrapped on the way out by `wrap_for_mcp_app`
    // for MCP App consumers and served verbatim to the notebook app.
    Some(workspace.join("apps/notebook/src/renderer-plugins"))
}

async fn serve_dev_plugin_file(name: &str, dir: &Path) -> Option<Response<Full<Bytes>>> {
    let content_type = embedded_plugins::content_type_for(name)?;
    let path = dir.join(name);

    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let body = transform_plugin_body(name, bytes);
            Some(
                Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", content_type)
                    .header("Content-Length", body.len().to_string())
                    .header("Cache-Control", "no-store")
                    .header("Access-Control-Allow-Origin", "*")
                    .header("X-Content-Type-Options", "nosniff")
                    .body(Full::new(Bytes::from(body)))
                    .unwrap_or_else(|_| {
                        text_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
                    }),
            )
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            warn!(
                "[blob-server] failed to read dev plugin asset {}: {}",
                path.display(),
                e
            );
            Some(text_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
            ))
        }
    }
}

/// Serve an embedded renderer plugin asset (JS, CSS, or WASM).
///
/// Plugins are embedded in the binary at compile time via `include_bytes!`.
/// `.css` and `.wasm` are served zero-copy via `Bytes::from_static`; `.js`
/// gets run through [`wrap_for_mcp_app`] because runtimed embeds the raw CJS
/// bundle from the notebook renderer-plugin dir (one canonical location,
/// two shapes at the edge).
fn serve_embedded_plugin(name: &str) -> Response<Full<Bytes>> {
    let Some((bytes, content_type)) = embedded_plugins::get(name) else {
        return text_response(StatusCode::NOT_FOUND, "Not Found");
    };

    let (body, body_len) = if name.ends_with(".js") {
        let wrapped = wrap_for_mcp_app(bytes);
        let len = wrapped.len();
        (Full::new(Bytes::from(wrapped)), len)
    } else {
        (Full::new(Bytes::from_static(bytes)), bytes.len())
    };

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", content_type)
        .header("Content-Length", body_len.to_string())
        .header("Cache-Control", "public, max-age=86400")
        .header("Access-Control-Allow-Origin", "*")
        .header("X-Content-Type-Options", "nosniff")
        .body(body)
        .unwrap_or_else(|_| {
            text_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
        })
}

/// Apply the MCP App IIFE wrapper to a renderer-plugin body, if the body is
/// JavaScript. Non-`.js` assets pass through untouched.
///
/// Equivalent to `apps/mcp-app/src/lib/wrap-plugin.js::wrapForMcpApp` - the
/// `embedded_plugins_wrap_matches_js_helper` test in
/// `embedded_plugins.rs` pins the two in lockstep.
fn transform_plugin_body(name: &str, raw: Vec<u8>) -> Vec<u8> {
    if name.ends_with(".js") {
        wrap_for_mcp_app(&raw)
    } else {
        raw
    }
}

/// Wrap a raw CJS renderer plugin in an IIFE for MCP App loading.
///
/// The wrapper matches `apps/mcp-app/src/lib/wrap-plugin.js::wrapForMcpApp`
/// byte-for-byte (the `wrap_for_mcp_app_matches_js_helper` test pins this).
///
/// Steps:
///   1. Local `module`/`exports`/`require` so the plugin doesn't leak into
///      global scope.
///   2. `require` resolves via `window.__nteract.require` so React and the
///      other MCP App-provided deps are reachable.
///   3. If the plugin exported an `install(nteract)` function, call it with
///      `window.__nteract` so it self-registers.
pub(crate) fn wrap_for_mcp_app(code: &[u8]) -> Vec<u8> {
    const PROLOGUE: &[u8] = b"(function(){\nvar exports={},module={exports:exports};\nvar require=window.__nteract.require;\n";
    const EPILOGUE: &[u8] = b"\n;var _i=module.exports&&module.exports.install;\nif(typeof _i==='function')_i(window.__nteract)\n})();";
    let mut out = Vec::with_capacity(PROLOGUE.len() + code.len() + EPILOGUE.len());
    out.extend_from_slice(PROLOGUE);
    out.extend_from_slice(code);
    out.extend_from_slice(EPILOGUE);
    out
}

/// Build a simple text response.
#[allow(clippy::expect_used)] // Response::builder only fails with invalid StatusCode, we use valid enum values
fn text_response(status: StatusCode, body: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("Access-Control-Allow-Origin", "*")
        .body(Full::new(Bytes::from(body.to_string())))
        .expect("response builder should not fail")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn setup() -> (TempDir, Arc<BlobStore>, u16) {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(BlobStore::new(dir.path().join("blobs")));
        // Tests bind :0 explicitly to avoid fighting a locally-running dev
        // daemon for the channel's preferred port.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = start_blob_server_with_listener(listener, store.clone(), None)
            .await
            .unwrap();
        // Give the server a moment to start accepting
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        (dir, store, port)
    }

    async fn get(port: u16, path: &str) -> (StatusCode, Vec<(String, String)>, Vec<u8>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpStream;

        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port))
            .await
            .unwrap();
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            path
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();

        let response = String::from_utf8_lossy(&buf);
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));

        let mut lines = head.lines();
        let status_line = lines.next().unwrap_or("");
        let status_code = status_line
            .split_whitespace()
            .nth(1)
            .unwrap_or("0")
            .parse::<u16>()
            .unwrap_or(0);

        let headers: Vec<(String, String)> = lines
            .filter_map(|line| {
                let (key, value) = line.split_once(": ")?;
                Some((key.to_lowercase(), value.to_string()))
            })
            .collect();

        (
            StatusCode::from_u16(status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            headers,
            body.as_bytes().to_vec(),
        )
    }

    fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }

    fn response_header(response: &Response<Full<Bytes>>, name: &str) -> Option<String> {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned)
    }

    async fn response_body(response: Response<Full<Bytes>>) -> Vec<u8> {
        use http_body_util::BodyExt;

        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec()
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let (_dir, _store, port) = setup().await;
        let (status, _, body) = get(port, "/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"OK");
    }

    #[tokio::test]
    async fn test_blob_not_found() {
        let (_dir, _store, port) = setup().await;
        let fake_hash = "a".repeat(64);
        let (status, _, _) = get(port, &format!("/blob/{}", fake_hash)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_serve_blob_with_content_type() {
        let (_dir, store, port) = setup().await;

        let data = b"fake png data";
        let hash = store.put(data, "image/png").await.unwrap();

        let (status, headers, body) = get(port, &format!("/blob/{}", hash)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, data);
        assert_eq!(
            header_value(&headers, "content-type"),
            Some("image/png".into())
        );
        assert_eq!(
            header_value(&headers, "cache-control"),
            Some("public, max-age=31536000, immutable".into())
        );
        assert_eq!(
            header_value(&headers, "access-control-allow-origin"),
            Some("*".into())
        );
    }

    #[tokio::test]
    async fn test_unknown_path_returns_404() {
        let (_dir, _store, port) = setup().await;
        let (status, _, _) = get(port, "/unknown").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_two_servers_get_different_ports() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(BlobStore::new(dir.path().join("blobs")));
        let listener1 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listener2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port1 = start_blob_server_with_listener(listener1, store.clone(), None)
            .await
            .unwrap();
        let port2 = start_blob_server_with_listener(listener2, store.clone(), None)
            .await
            .unwrap();
        assert_ne!(port1, port2);
    }

    #[tokio::test]
    async fn test_blob_server_binds_loopback_only() {
        let listener = bind_preferred_or_random().await.unwrap();
        let addr = listener.local_addr().unwrap();

        assert_eq!(
            addr.ip(),
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        );
    }

    #[tokio::test]
    async fn test_bind_bumps_on_collision() {
        // Hold the preferred port so the first attempt hits AddrInUse.
        let preferred = runt_workspace::preferred_blob_port();
        let Ok(blocker) = TcpListener::bind(("127.0.0.1", preferred)).await else {
            // Preferred port already in use by something else (a running
            // daemon, parallel test). The bump path will still be exercised —
            // just skip the explicit assertion about which port we got.
            return;
        };

        let port = bind_preferred_or_random()
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let bump_range = (preferred + 1)..=(preferred + PREFERRED_PORT_ATTEMPTS - 1);
        assert!(
            bump_range.contains(&port),
            "port {port} should be in the bump range {bump_range:?}",
        );
        drop(blocker);
    }

    #[tokio::test]
    async fn test_embedded_plugin_served() {
        let response = serve_embedded_plugin("plotly.js");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_header(&response, "content-type"),
            Some("application/javascript; charset=utf-8".into())
        );
        assert_eq!(
            response_header(&response, "cache-control"),
            Some("public, max-age=86400".into())
        );
        assert_eq!(
            response_header(&response, "access-control-allow-origin"),
            Some("*".into())
        );
    }

    #[tokio::test]
    async fn test_plugin_http_route_serves_known_asset() {
        let (_dir, _store, port) = setup().await;
        let (status, headers, _body) = get(port, "/plugins/plotly.js").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            header_value(&headers, "content-type"),
            Some("application/javascript; charset=utf-8".into())
        );
        assert_eq!(
            header_value(&headers, "access-control-allow-origin"),
            Some("*".into())
        );
    }

    #[tokio::test]
    async fn test_dev_filesystem_plugin_wins_over_embedded() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join("plugins");
        tokio::fs::create_dir_all(&plugin_dir).await.unwrap();
        tokio::fs::write(plugin_dir.join("plotly.js"), b"dev plotly")
            .await
            .unwrap();

        let response = serve_plugin_with_dev_assets("plotly.js", Some(&plugin_dir)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_header(&response, "content-type"),
            Some("application/javascript; charset=utf-8".into())
        );
        assert_eq!(
            response_header(&response, "cache-control"),
            Some("no-store".into())
        );
        assert_eq!(
            response_header(&response, "access-control-allow-origin"),
            Some("*".into())
        );
        // Dev-path .js is wrapped on serve, same as the embedded path.
        let body = response_body(response).await;
        let expected = wrap_for_mcp_app(b"dev plotly");
        assert_eq!(body, expected);
    }

    #[tokio::test]
    async fn test_dev_filesystem_css_passes_through_unwrapped() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join("plugins");
        tokio::fs::create_dir_all(&plugin_dir).await.unwrap();
        tokio::fs::write(plugin_dir.join("markdown.css"), b".md{color:red}")
            .await
            .unwrap();

        let response = serve_plugin_with_dev_assets("markdown.css", Some(&plugin_dir)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_body(response).await, b".md{color:red}");
    }

    #[test]
    fn wrap_for_mcp_app_matches_js_helper() {
        // Byte-equal pin against apps/mcp-app/src/lib/wrap-plugin.js.
        // If the JS helper is updated, copy the template here verbatim.
        let raw = "module.exports = { install: (n) => n.register('x', {}) };";
        let wrapped = wrap_for_mcp_app(raw.as_bytes());
        let expected = "(function(){\nvar exports={},module={exports:exports};\nvar require=window.__nteract.require;\nmodule.exports = { install: (n) => n.register('x', {}) };\n;var _i=module.exports&&module.exports.install;\nif(typeof _i==='function')_i(window.__nteract)\n})();";
        assert_eq!(std::str::from_utf8(&wrapped).unwrap(), expected);
    }

    #[tokio::test]
    async fn test_dev_filesystem_missing_asset_falls_back_to_embedded() {
        let dir = TempDir::new().unwrap();
        let response = serve_plugin_with_dev_assets("plotly.js", Some(dir.path())).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_header(&response, "cache-control"),
            Some("public, max-age=86400".into())
        );
    }

    #[tokio::test]
    async fn test_plugin_unknown_returns_404() {
        let (_dir, _store, port) = setup().await;
        let (status, _, _) = get(port, "/plugins/nonexistent.js").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_plugin_path_traversal_rejected() {
        let (_dir, _store, port) = setup().await;
        let (status, _, _) = get(port, "/plugins/../secret").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_blob_path_traversal_rejected() {
        let (_dir, _store, port) = setup().await;
        let (status, _, _) = get(port, "/blob/../secret").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
