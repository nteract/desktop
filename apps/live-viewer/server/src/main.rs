//! Live notebook viewer — WebSocket frame relay for the runtimed daemon.
//!
//! Bridges the daemon's Unix socket to browser WebSocket clients on the tailnet.
//! Each browser connection gets its own RelayHandle to the daemon — the browser
//! owns the Automerge sync state via WASM, and this server just pipes bytes.
//!
//! Architecture:
//! - Browser opens WebSocket to `/ws/open?path=...` or `/ws/join?id=...`
//! - Server creates a relay connection to the daemon for that notebook
//! - Daemon frames → binary WebSocket messages → browser
//! - Browser frames → relay.forward_frame() → daemon
//! - Read-only mode: only outbound frame types 0x00 (sync) and 0x05 (RuntimeStateSync) allowed

use std::path::PathBuf;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Query, State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::Router;
use futures::stream::StreamExt;
use futures::SinkExt;
use notebook_sync::relay::RelayHandle;
use serde::Deserialize;
use tokio::sync::mpsc;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tracing::{info, warn};

const PORT: u16 = 8743;

#[derive(Clone)]
struct AppState {
    socket_path: PathBuf,
}

#[derive(Deserialize)]
struct OpenParams {
    path: String,
}

#[derive(Deserialize)]
struct JoinParams {
    id: String,
}

// ─── WebSocket handlers ─────────────────────────────────────────────────

async fn ws_open(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(params): Query<OpenParams>,
) -> impl IntoResponse {
    let path = PathBuf::from(&params.path);
    info!("[relay] open request: {:?}", path);
    ws.on_upgrade(move |socket| handle_relay_connection(socket, state, RelayTarget::Open(path)))
}

async fn ws_join(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(params): Query<JoinParams>,
) -> impl IntoResponse {
    info!("[relay] join request: {}", &params.id);
    ws.on_upgrade(move |socket| {
        handle_relay_connection(socket, state, RelayTarget::Join(params.id))
    })
}

enum RelayTarget {
    Open(PathBuf),
    Join(String),
}

/// Handle a single WebSocket connection — create a relay to the daemon and pipe frames.
async fn handle_relay_connection(socket: WebSocket, state: AppState, target: RelayTarget) {
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // Connect to daemon as a relay
    let relay: RelayHandle = match &target {
        RelayTarget::Open(path) => {
            match notebook_sync::connect::connect_open_relay(
                state.socket_path.clone(),
                path.clone(),
                frame_tx,
            )
            .await
            {
                Ok(result) => {
                    info!(
                        "[relay] connected to notebook {} (path={:?})",
                        result.info.notebook_id,
                        path.file_name()
                    );
                    result.handle
                }
                Err(e) => {
                    warn!("[relay] open failed: {e}");
                    return;
                }
            }
        }
        RelayTarget::Join(id) => {
            match notebook_sync::connect::connect_relay(
                state.socket_path.clone(),
                id.clone(),
                frame_tx,
            )
            .await
            {
                Ok(result) => {
                    info!("[relay] joined notebook {}", id);
                    result.handle
                }
                Err(e) => {
                    warn!("[relay] join failed: {e}");
                    return;
                }
            }
        }
    };

    let notebook_id = relay.notebook_id();
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Task: daemon frames → WebSocket (binary)
    let outbound = tokio::spawn(async move {
        while let Some(frame) = frame_rx.recv().await {
            if ws_tx.send(Message::Binary(frame.into())).await.is_err() {
                break;
            }
        }
    });

    // Task: WebSocket → daemon (forward_frame)
    let relay_clone = relay.clone();
    let inbound = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            match msg {
                Message::Binary(data) => {
                    if data.is_empty() {
                        continue;
                    }
                    let frame_type = data[0];
                    // Read-only: only allow sync frames from browser
                    if frame_type == 0x00 || frame_type == 0x05 || frame_type == 0x06 {
                        if let Err(e) = relay_clone
                            .forward_frame(frame_type, data[1..].to_vec())
                            .await
                        {
                            warn!("[relay] forward_frame error: {e}");
                            break;
                        }
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    // Wait for either direction to finish
    tokio::select! {
        _ = outbound => {},
        _ = inbound => {},
    }

    info!(
        "[relay] disconnected from notebook {}",
        &notebook_id[..8.min(notebook_id.len())]
    );
}

// ─── REST endpoints ─────────────────────────────────────────────────────

async fn handle_index() -> Html<&'static str> {
    Html(include_str!("../../client/index.html"))
}

/// List active notebook rooms on the daemon.
async fn handle_list(State(state): State<AppState>) -> impl IntoResponse {
    let client = runtimed_client::client::PoolClient::new(state.socket_path.clone());
    match client.list_rooms().await {
        Ok(rooms) => {
            let json = serde_json::to_string(&rooms).unwrap_or_default();
            (StatusCode::OK, [("content-type", "application/json")], json)
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [("content-type", "application/json")],
            format!("{{\"error\":\"{e}\"}}"),
        ),
    }
}

// ─── Main ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "live_viewer_server=info".into()),
        )
        .init();

    let socket_path =
        runtimed_client::socket_path_for_channel(runtimed_client::BuildChannel::Nightly);
    info!("daemon socket: {:?}", socket_path);

    let state = AppState { socket_path };

    // Serve the Vite build output if available, otherwise fall back to test HTML.
    // Check env var first, then look relative to the cargo manifest dir (dev),
    // then relative to the binary.
    let dist_path = std::env::var("LIVE_VIEWER_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let manifest_relative = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../dist");
            if manifest_relative.join("index.html").exists() {
                return manifest_relative;
            }
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.to_path_buf()))
                .unwrap_or_default()
                .join("dist")
        });

    let has_dist = dist_path.join("index.html").exists();
    if has_dist {
        info!("serving built app from {:?}", dist_path);
    } else {
        info!("no dist/ found, serving embedded test page");
    }

    let mut app = Router::new()
        .route("/api/notebooks", get(handle_list))
        .route("/ws/open", get(ws_open))
        .route("/ws/join", get(ws_join))
        .layer(CorsLayer::permissive())
        .with_state(state);

    if has_dist {
        app =
            app.fallback_service(ServeDir::new(&dist_path).append_index_html_on_directories(true));
    } else {
        app = app.route("/", get(handle_index));
    }

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{PORT}"))
        .await
        .expect("failed to bind");

    let hostname = std::process::Command::new("sh")
        .arg("-c")
        .arg(r#"tailscale status --json 2>/dev/null | python3 -c "import sys,json; print(json.load(sys.stdin).get('Self',{}).get('DNSName','').rstrip('.'))" 2>/dev/null"#)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "0.0.0.0".into());

    info!("nteract live viewer (frame relay)");
    info!("  Tailnet: http://{}:{}", hostname, PORT);
    info!("  Local:   http://localhost:{}", PORT);

    axum::serve(listener, app).await.unwrap();
}
