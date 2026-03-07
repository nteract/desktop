use anyhow::Result;
use chrono::Utc;
use serde::Serialize;

use futures::future::{select, Either};
use futures::StreamExt;
use log::{debug, error, info};
use muda::{
    accelerator::{Accelerator, Code, Modifiers},
    Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
};
use rust_embed::Embed;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use jupyter_protocol::{
    media::MediaType, ConnectionInfo, ExecuteRequest, ExpressionResult, JupyterMessage,
    JupyterMessageContent, KernelInfoRequest,
};
use tauri_jupyter::WebViewJupyterMessage;

use std::path::PathBuf;
use tao::{
    dpi::Size,
    event::{ElementState, Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop, EventLoopBuilder},
    keyboard::{Key, ModifiersState},
    window::{Icon, Window, WindowBuilder},
};
use tokio::fs;
use wry::{
    http::{Method, Request, Response},
    WebViewBuilder,
};

#[derive(Embed)]
#[folder = "../../apps/sidecar/dist"]
struct Asset;

// Menu item IDs
const MENU_QUIT_ID: &str = "quit";
const MENU_CLOSE_ID: &str = "close";

/// Load the app icon from embedded PNG data
fn load_icon() -> Option<Icon> {
    let icon_bytes = include_bytes!("../icons/icon.png");
    let img = image::load_from_memory(icon_bytes).ok()?.into_rgba8();
    let (width, height) = img.dimensions();
    Icon::from_rgba(img.into_raw(), width, height).ok()
}

/// Type alias for backwards compatibility
type WryJupyterMessage = WebViewJupyterMessage;

/// Entry in the debug dump file - wraps a message with metadata for analysis
#[derive(Serialize)]
struct DumpEntry {
    /// ISO 8601 timestamp when the message was logged
    ts: String,
    /// Direction: "out" = sent to kernel, "in" = received from kernel
    dir: &'static str,
    /// Channel: "shell", "iopub", "control", etc.
    ch: &'static str,
    /// The actual Jupyter message
    msg: WryJupyterMessage,
}

impl DumpEntry {
    fn new(dir: &'static str, ch: &'static str, message: JupyterMessage) -> Self {
        Self {
            ts: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            dir,
            ch,
            msg: message.into(),
        }
    }
}

/// Write a dump entry to the file if dump is enabled
fn write_dump_entry(dump_file: &Option<Arc<Mutex<std::fs::File>>>, entry: DumpEntry) {
    if let Some(ref file) = dump_file {
        if let Ok(json) = serde_json::to_string(&entry) {
            if let Ok(mut f) = file.lock() {
                let _ = writeln!(f, "{}", json);
                let _ = f.flush();
            }
        }
    }
}

#[derive(Debug, Clone)]
enum SidecarEvent {
    JupyterMessage(Box<JupyterMessage>),
    KernelCwd { cwd: String },
    KernelStatus { status: KernelConnectionStatus },
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum KernelConnectionStatus {
    Connected,
}

async fn run(
    connection_file_path: &PathBuf,
    event_loop: EventLoop<SidecarEvent>,
    window: Window,
    dump_file: Option<Arc<Mutex<std::fs::File>>>,
) -> anyhow::Result<()> {
    let content = fs::read_to_string(&connection_file_path).await?;
    let connection_info = serde_json::from_str::<ConnectionInfo>(&content)?;

    // Check if kernel is alive before trying to connect
    // This prevents hanging on dead kernels since ZeroMQ connections don't fail-fast
    if !check_kernel_heartbeat(&connection_info, Duration::from_secs(2)).await {
        anyhow::bail!(
            "Kernel is not responding (heartbeat failed). The kernel may have exited or the connection file may be stale."
        );
    }

    let session_id = format!("sidecar-{}", uuid::Uuid::new_v4());

    let mut iopub =
        runtimelib::create_client_iopub_connection(&connection_info, "", &session_id).await?;

    // Create a single shell connection with explicit identity for stdin support
    let identity = runtimelib::peer_identity_for_session(&session_id)?;
    let mut shell = runtimelib::create_client_shell_connection_with_identity(
        &connection_info,
        &session_id,
        identity,
    )
    .await?;

    // Do kernel_info and cwd requests BEFORE splitting the shell connection
    // This ensures we only have one shell connection to the kernel
    let kernel_info_result = request_kernel_info_on_shell(&mut shell, Duration::from_secs(2)).await;
    let kernel_cwd_result = if kernel_info_result
        .as_ref()
        .and_then(|msg| match &msg.content {
            JupyterMessageContent::KernelInfoReply(reply) => Some(&reply.language_info.name),
            _ => None,
        })
        .map(|name| name == "python")
        .unwrap_or(false)
    {
        request_python_cwd_on_shell(&mut shell, Duration::from_secs(2)).await
    } else {
        None
    };

    // Now split the shell for async message passing
    let (mut shell_writer, mut shell_reader) = shell.split();

    let event_loop_proxy = event_loop.create_proxy();

    // Send half: forward messages from UI to kernel
    let (tx, mut rx) = futures::channel::mpsc::channel::<JupyterMessage>(100);
    tokio::spawn(async move {
        while let Some(message) = rx.next().await {
            if let Err(e) = shell_writer.send(message).await {
                error!("Failed to send message: {}", e);
            }
        }
    });

    // Recv half: read shell replies and forward to UI
    let shell_event_proxy = event_loop_proxy.clone();
    tokio::spawn(async move {
        while let Ok(message) = shell_reader.read().await {
            if let Err(e) =
                shell_event_proxy.send_event(SidecarEvent::JupyterMessage(Box::new(message)))
            {
                error!("Failed to forward shell reply: {:?}", e);
                break;
            }
        }
    });

    let ui_ready = Arc::new(AtomicBool::new(false));
    let pending_kernel_info: Arc<Mutex<Option<JupyterMessage>>> = Arc::new(Mutex::new(None));
    let pending_kernel_cwd: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let ui_ready_handler = ui_ready.clone();
    let pending_kernel_info_handler = pending_kernel_info.clone();
    let pending_kernel_cwd_handler = pending_kernel_cwd.clone();
    let kernel_info_proxy = event_loop_proxy.clone();
    let dump_file_for_shell = dump_file.clone();

    let webview = WebViewBuilder::new()
        .with_devtools(true)
        .with_asynchronous_custom_protocol("sidecar".into(), move |_webview_id, req, responder| {
            if let (&Method::POST, "/message") = (req.method(), req.uri().path()) {
                match serde_json::from_slice::<WryJupyterMessage>(req.body()) {
                    Ok(wry_message) => {
                        let message: JupyterMessage = wry_message.into();

                        info!(
                            "Sending message to shell: type={}, comm_id={:?}",
                            message.header.msg_type,
                            match &message.content {
                                JupyterMessageContent::CommMsg(c) => Some(c.comm_id.clone()),
                                _ => None,
                            }
                        );

                        // Dump outbound message to file if enabled
                        write_dump_entry(
                            &dump_file_for_shell,
                            DumpEntry::new("out", "shell", message.clone()),
                        );

                        let mut tx = tx.clone();

                        if let Err(e) = tx.try_send(message) {
                            error!("Failed to send message to shell channel: {}", e);
                        } else {
                            info!("Message sent to shell channel successfully");
                        }
                        responder.respond(Response::builder().status(200).body(&[]).unwrap());
                        return;
                    }
                    Err(e) => {
                        error!("Failed to deserialize message: {}", e);
                        responder.respond(
                            Response::builder()
                                .status(400)
                                .body("Bad Request".as_bytes().to_vec())
                                .unwrap(),
                        );
                        return;
                    }
                }
            };

            if let (&Method::POST, "/ready") = (req.method(), req.uri().path()) {
                ui_ready_handler.store(true, Ordering::SeqCst);
                if let Ok(mut pending) = pending_kernel_info_handler.lock() {
                    if let Some(message) = pending.take() {
                        let _ = kernel_info_proxy
                            .send_event(SidecarEvent::JupyterMessage(Box::new(message)));
                    }
                }
                if let Ok(mut pending) = pending_kernel_cwd_handler.lock() {
                    if let Some(cwd) = pending.take() {
                        let _ = kernel_info_proxy.send_event(SidecarEvent::KernelCwd { cwd });
                    }
                }
                responder.respond(Response::builder().status(204).body(Vec::new()).unwrap());
                return;
            }
            let response = get_response(req).map_err(|e| {
                error!("{:?}", e);
                e
            });
            match response {
                Ok(response) => responder.respond(response),
                Err(e) => {
                    error!("{:?}", e);
                    responder.respond(
                        Response::builder()
                            .status(500)
                            .body("Internal Server Error".as_bytes().to_vec())
                            .unwrap(),
                    )
                }
            }
        });

    let kernel_label = connection_file_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("kernel");
    let kernel_query = querystring::stringify(vec![("kernel", kernel_label)]);
    let ui_url = format!("sidecar://localhost/?{}", kernel_query);

    let webview = webview.with_url(&ui_url).build(&window)?;

    // Kernel was confirmed alive at startup via heartbeat check
    let _ = event_loop_proxy.send_event(SidecarEvent::KernelStatus {
        status: KernelConnectionStatus::Connected,
    });

    // Store pre-fetched kernel info in pending slots (will be sent when UI is ready)
    if let Some(message) = kernel_info_result {
        if let Ok(mut pending) = pending_kernel_info.lock() {
            *pending = Some(message);
        }
    }
    if let Some(cwd) = kernel_cwd_result {
        if let Ok(mut pending) = pending_kernel_cwd.lock() {
            *pending = Some(cwd);
        }
    }

    tokio::spawn(async move {
        while let Ok(message) = iopub.read().await {
            // Log ALL messages from iopub for debugging
            info!(
                "iopub message: type={}, comm_id={:?}",
                message.header.msg_type,
                match &message.content {
                    JupyterMessageContent::CommOpen(c) => Some(c.comm_id.clone()),
                    JupyterMessageContent::CommMsg(c) => Some(c.comm_id.clone()),
                    JupyterMessageContent::CommClose(c) => Some(c.comm_id.clone()),
                    _ => None,
                }
            );

            // Dump message to file if enabled
            write_dump_entry(&dump_file, DumpEntry::new("in", "iopub", message.clone()));

            match event_loop_proxy.send_event(SidecarEvent::JupyterMessage(Box::new(message))) {
                Ok(_) => {
                    debug!("Sent message to event loop");
                }
                Err(e) => {
                    error!("Failed to send message to event loop: {:?}", e);
                    break;
                }
            };
        }
    });

    // Track modifier keys for keyboard shortcuts
    let mut modifiers = ModifiersState::default();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        // Check for menu events
        if let Ok(menu_event) = MenuEvent::receiver().try_recv() {
            match menu_event.id().0.as_str() {
                MENU_QUIT_ID => {
                    *control_flow = ControlFlow::Exit;
                    return;
                }
                MENU_CLOSE_ID => {
                    *control_flow = ControlFlow::Exit;
                    return;
                }
                _ => {}
            }
        }

        match event {
            Event::WindowEvent {
                event: WindowEvent::ModifiersChanged(new_modifiers),
                ..
            } => {
                modifiers = new_modifiers;
            }
            Event::WindowEvent {
                event:
                    WindowEvent::KeyboardInput {
                        event: key_event, ..
                    },
                ..
            } => {
                // Cmd+Option+I to open devtools (macOS)
                // Ctrl+Shift+I on other platforms
                if key_event.state == ElementState::Pressed {
                    let is_devtools_shortcut = if cfg!(target_os = "macos") {
                        modifiers.super_key()
                            && modifiers.alt_key()
                            && key_event.logical_key == Key::Character("i")
                    } else {
                        modifiers.control_key()
                            && modifiers.shift_key()
                            && key_event.logical_key == Key::Character("I")
                    };

                    #[cfg(debug_assertions)]
                    if is_devtools_shortcut {
                        info!("Opening devtools");
                        webview.open_devtools();
                    }
                    #[cfg(not(debug_assertions))]
                    let _ = is_devtools_shortcut; // Silence unused variable warning
                }
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(data) => match data {
                SidecarEvent::JupyterMessage(message) => {
                    debug!("Received UserEvent message: {}", message.header.msg_type);
                    let serialized: WryJupyterMessage = (*message).into();
                    match serde_json::to_string(&serialized) {
                        Ok(serialized_message) => {
                            webview
                                .evaluate_script(&format!(
                                    r#"globalThis.onMessage({})"#,
                                    serialized_message
                                ))
                                .unwrap_or_else(|e| error!("Failed to evaluate script: {:?}", e));
                        }
                        Err(e) => error!("Failed to serialize message: {}", e),
                    }
                }
                SidecarEvent::KernelCwd { cwd } => {
                    let payload = serde_json::json!({
                        "type": "kernel_cwd",
                        "cwd": cwd,
                    });
                    if let Ok(serialized_payload) = serde_json::to_string(&payload) {
                        webview
                            .evaluate_script(&format!(
                                r#"globalThis.onSidecarInfo({})"#,
                                serialized_payload
                            ))
                            .unwrap_or_else(|e| error!("Failed to evaluate script: {:?}", e));
                    }
                }
                SidecarEvent::KernelStatus { status } => {
                    let payload = serde_json::json!({
                        "type": "kernel_status",
                        "status": status,
                    });
                    if let Ok(serialized_payload) = serde_json::to_string(&payload) {
                        webview
                            .evaluate_script(&format!(
                                r#"globalThis.onSidecarInfo({})"#,
                                serialized_payload
                            ))
                            .unwrap_or_else(|e| error!("Failed to evaluate script: {:?}", e));
                    }
                }
            },
            _ => {}
        }
    });
}

/// Request kernel info using an existing shell connection (before splitting)
async fn request_kernel_info_on_shell(
    shell: &mut runtimelib::ClientShellConnection,
    timeout: Duration,
) -> Option<JupyterMessage> {
    let request: JupyterMessage = KernelInfoRequest::default().into();
    if let Err(e) = shell.send(request).await {
        error!("Failed to send kernel_info_request: {}", e);
        return None;
    }

    let result = select(
        Box::pin(shell.read()),
        Box::pin(tokio::time::sleep(timeout)),
    )
    .await;

    match result {
        Either::Left((Ok(message), _)) => {
            if message.header.msg_type == "kernel_info_reply" {
                Some(message)
            } else {
                None
            }
        }
        Either::Left((Err(e), _)) => {
            error!("Failed to read kernel_info_reply: {}", e);
            None
        }
        Either::Right((_timeout, _)) => None,
    }
}

/// Request Python cwd using an existing shell connection (before splitting)
async fn request_python_cwd_on_shell(
    shell: &mut runtimelib::ClientShellConnection,
    timeout: Duration,
) -> Option<String> {
    let mut user_expressions = HashMap::new();
    user_expressions.insert("cwd".to_string(), "__import__('os').getcwd()".to_string());
    let request = ExecuteRequest {
        code: String::new(),
        silent: true,
        store_history: false,
        user_expressions: Some(user_expressions),
        allow_stdin: false,
        stop_on_error: false,
    };

    if let Err(e) = shell.send(request.into()).await {
        error!("Failed to send cwd execute_request: {}", e);
        return None;
    }

    let result = select(
        Box::pin(shell.read()),
        Box::pin(tokio::time::sleep(timeout)),
    )
    .await;

    match result {
        Either::Left((Ok(message), _)) => {
            if message.header.msg_type != "execute_reply" {
                return None;
            }
            let JupyterMessageContent::ExecuteReply(reply) = message.content else {
                return None;
            };
            let user_expressions = reply.user_expressions?;
            let expression = user_expressions.get("cwd")?;
            match expression {
                ExpressionResult::Ok { data, .. } => data.content.iter().find_map(|media| {
                    if let MediaType::Plain(text) = media {
                        Some(text.clone())
                    } else {
                        None
                    }
                }),
                ExpressionResult::Error { .. } => None,
            }
        }
        Either::Left((Err(e), _)) => {
            error!("Failed to read cwd execute_reply: {}", e);
            None
        }
        Either::Right((_timeout, _)) => None,
    }
}

/// Check if a kernel is alive by sending a heartbeat ping.
///
/// Returns true if the kernel responds within the timeout, false otherwise.
async fn check_kernel_heartbeat(connection_info: &ConnectionInfo, timeout: Duration) -> bool {
    let heartbeat_result = tokio::time::timeout(timeout, async {
        let mut hb = runtimelib::create_client_heartbeat_connection(connection_info).await?;
        hb.single_heartbeat().await
    })
    .await;

    matches!(heartbeat_result, Ok(Ok(())))
}

/// Launch the sidecar viewer for a Jupyter kernel.
///
/// This takes over the current thread to run the GUI event loop.
///
/// # Arguments
/// * `file` - Path to a Jupyter kernel connection file (JSON)
/// * `quiet` - If true, suppress log output
/// * `dump` - Optional path to dump all Jupyter messages as JSON
pub fn launch(file: &Path, quiet: bool, dump: Option<&Path>) -> Result<()> {
    if !quiet {
        env_logger::init();
    }
    info!("Starting sidecar application");
    let (width, height) = (960.0, 550.0);

    if !file.exists() {
        anyhow::bail!("Invalid file provided");
    }
    let connection_file = file.to_path_buf();

    let event_loop: EventLoop<SidecarEvent> = EventLoopBuilder::with_user_event().build();

    // Create window with icon
    let mut window_builder = WindowBuilder::new()
        .with_title("kernel sidecar")
        .with_inner_size(Size::Logical((width, height).into()));

    if let Some(icon) = load_icon() {
        window_builder = window_builder.with_window_icon(Some(icon));
    }

    let window = window_builder.build(&event_loop).unwrap();

    // Create menu bar
    let menu_bar = Menu::new();

    // App menu (macOS) with Quit
    let app_menu = Submenu::new("Sidecar", true);
    let quit_item = MenuItem::with_id(
        MENU_QUIT_ID,
        "Quit Sidecar",
        true,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::KeyQ)),
    );
    app_menu.append(&PredefinedMenuItem::about(None, None)).ok();
    app_menu.append(&PredefinedMenuItem::separator()).ok();
    app_menu.append(&quit_item).ok();
    menu_bar.append(&app_menu).ok();

    // Window menu with Close
    let window_menu = Submenu::new("Window", true);
    let close_item = MenuItem::with_id(
        MENU_CLOSE_ID,
        "Close Window",
        true,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::KeyW)),
    );
    window_menu.append(&PredefinedMenuItem::minimize(None)).ok();
    window_menu.append(&close_item).ok();
    menu_bar.append(&window_menu).ok();

    // Initialize the menu bar on the window
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::WindowExtMacOS;
        menu_bar.init_for_nsapp();
        window.set_is_document_edited(false);
    }
    #[cfg(target_os = "windows")]
    {
        use tao::platform::windows::WindowExtWindows;
        // SAFETY: window.hwnd() returns a valid HWND for the duration of the window's lifetime
        unsafe { menu_bar.init_for_hwnd(window.hwnd() as _) }.ok();
    }
    // Linux: Menu bar initialization skipped - requires GTK integration

    let dump_file = dump.map(|path| {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .expect("Failed to open dump file");
        info!("Dumping messages to {:?}", path);
        Arc::new(Mutex::new(file))
    });

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run(&connection_file, event_loop, window, dump_file))
}

fn get_response(request: Request<Vec<u8>>) -> Result<Response<Vec<u8>>> {
    if request.method() != Method::GET {
        return Ok(Response::builder()
            .status(405)
            .body("Method Not Allowed".as_bytes().to_vec())
            .unwrap());
    }

    let path = request.uri().path();

    // Normalize path: "/" -> "index.html", strip leading "/"
    let file_path = if path == "/" {
        "index.html"
    } else {
        path.trim_start_matches('/')
    };

    debug!("Serving asset: {}", file_path);

    match Asset::get(file_path) {
        Some(content) => {
            // Guess MIME type from file extension
            let mime_type = mime_guess::from_path(file_path)
                .first_or_octet_stream()
                .to_string();

            debug!("Found asset {} with mime type {}", file_path, mime_type);

            Ok(Response::builder()
                .header("Content-Type", mime_type)
                .status(200)
                .body(content.data.into_owned())
                .unwrap())
        }
        None => {
            debug!("Asset not found: {}", file_path);
            Ok(Response::builder()
                .header("Content-Type", "text/plain")
                .status(404)
                .body("Not Found".as_bytes().to_vec())
                .unwrap())
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_wry_jupyter_message_empty_parent_header() {
        let msg = r#"
        {
            "header": {
                "date": "2025-05-14T14:32:23.490Z",
                "msg_id": "44bd6b44-78a1-4892-87df-c0861a005d56",
                "msg_type": "kernel_info_request",
                "session": "b75bddaa-6d69-4340-ba13-81516192370e",
                "username": "",
                "version": "5.2"
            },
            "parent_header": {},
            "metadata": {},
            "content": {},
            "buffers": [],
            "channel": "shell"
        }
        "#;

        let message: WryJupyterMessage = serde_json::from_str(msg).unwrap();
        assert!(message.parent_header.is_none());
    }

    #[test]
    fn test_wry_jupyter_message_null_parent_header() {
        let msg = r#"
        {
            "header": {
                "date": "2025-05-14T14:32:23.490Z",
                "msg_id": "44bd6b44-78a1-4892-87df-c0861a005d56",
                "msg_type": "kernel_info_request",
                "session": "b75bddaa-6d69-4340-ba13-81516192370e",
                "username": "",
                "version": "5.2"
            },
            "parent_header": null,
            "metadata": {},
            "content": {},
            "buffers": [],
            "channel": "shell"
        }
        "#;

        let message: WryJupyterMessage = serde_json::from_str(msg).unwrap();
        assert!(message.parent_header.is_none());
    }
}
