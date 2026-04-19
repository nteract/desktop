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
//! The server binds `127.0.0.1:0` (OS-assigned random port) and runs on
//! the caller's tokio runtime. It shuts down when the process exits; no
//! explicit cancellation is implemented yet.

use std::convert::Infallible;
use std::sync::Arc;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tracing::{error, info};

use crate::blob_store::BlobStore;
use crate::daemon::Daemon;
use crate::embedded_plugins;
use crate::task_supervisor::{spawn_best_effort, spawn_supervised};

/// Start the blob HTTP server on a random localhost port.
///
/// Returns the port the server is listening on. The server runs as a
/// spawned task on the current tokio runtime.
///
/// When `daemon` is provided, a panic in the accept loop triggers shutdown.
/// Pass `None` in tests where no daemon is available.
pub async fn start_blob_server(
    store: Arc<BlobStore>,
    daemon: Option<Arc<Daemon>>,
) -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
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
        serve_embedded_plugin(name)
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

/// Serve an embedded renderer plugin asset (JS or CSS).
///
/// Plugins are embedded in the binary at compile time via `include_bytes!`
/// and served directly from memory — zero-copy via `Bytes::from_static`.
fn serve_embedded_plugin(name: &str) -> Response<Full<Bytes>> {
    if name.contains('/') || name.contains("..") {
        return text_response(StatusCode::NOT_FOUND, "Not Found");
    }

    match embedded_plugins::get(name) {
        Some((bytes, content_type)) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", content_type)
            .header("Content-Length", bytes.len().to_string())
            .header("Cache-Control", "public, max-age=86400")
            .header("Access-Control-Allow-Origin", "*")
            .header("X-Content-Type-Options", "nosniff")
            .body(Full::new(Bytes::from_static(bytes)))
            .unwrap_or_else(|_| {
                text_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
            }),
        None => text_response(StatusCode::NOT_FOUND, "Not Found"),
    }
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
        let port = start_blob_server(store.clone(), None).await.unwrap();
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
        let port1 = start_blob_server(store.clone(), None).await.unwrap();
        let port2 = start_blob_server(store.clone(), None).await.unwrap();
        assert_ne!(port1, port2);
    }

    #[tokio::test]
    async fn test_embedded_plugin_served() {
        let (_dir, _store, port) = setup().await;
        // This test only verifies the route exists and returns 200 or 404
        // depending on whether plugins were built before compilation.
        // The embedded_plugins module is generated by build.rs.
        let (status, headers, _body) = get(port, "/plugins/plotly.js").await;
        if status == StatusCode::OK {
            assert_eq!(
                header_value(&headers, "content-type"),
                Some("application/javascript; charset=utf-8".into())
            );
            assert_eq!(
                header_value(&headers, "cache-control"),
                Some("public, max-age=86400".into())
            );
            assert_eq!(
                header_value(&headers, "access-control-allow-origin"),
                Some("*".into())
            );
        }
        // If 404, plugins weren't built — that's expected on clean checkouts
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
}
