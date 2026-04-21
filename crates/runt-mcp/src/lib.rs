//! Rust-native MCP server for nteract notebook interaction.
//!
//! Implements the MCP protocol using `rmcp`, backed by `runtimed-client`
//! for daemon IPC and `notebook-sync` for Automerge document operations.

// Allow `expect()` and `unwrap()` in tests
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, CallToolResult, Implementation, ListResourceTemplatesResult,
    ListResourcesResult, ListToolsResult, ReadResourceRequestParams, ReadResourceResult,
    ServerCapabilities, ServerInfo,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler};
use tokio::sync::RwLock;

pub mod editing;
pub mod execution;
pub mod formatting;
pub mod health;
pub mod presence;
pub mod project_file;
mod resources;
mod session;
mod structured;
pub mod tools;

use health::DaemonState;
use session::NotebookSession;

/// The nteract MCP server.
pub struct NteractMcp {
    socket_path: PathBuf,
    blob_base_url: Option<String>,
    blob_store_path: Option<PathBuf>,
    session: Arc<RwLock<Option<NotebookSession>>>,
    /// The MCP client's display name, sniffed from the initialize handshake.
    /// Used as the peer label in notebook sessions so the notebook app shows
    /// "Claude Desktop" or "Claude Code" instead of the default "Inkwell".
    peer_label: Arc<RwLock<String>>,
    /// When true, the `launch_app` tool is not registered (headless environments).
    no_show: bool,
    /// Current daemon connection state, updated by the health monitor.
    daemon_state: Arc<RwLock<DaemonState>>,
}

impl NteractMcp {
    /// Create a new MCP server instance.
    pub fn new(
        socket_path: PathBuf,
        blob_base_url: Option<String>,
        blob_store_path: Option<PathBuf>,
        initial_daemon_state: DaemonState,
    ) -> Self {
        Self {
            socket_path,
            blob_base_url,
            blob_store_path,
            session: Arc::new(RwLock::new(None)),
            peer_label: Arc::new(RwLock::new("Inkwell".to_string())),
            no_show: false,
            daemon_state: Arc::new(RwLock::new(initial_daemon_state)),
        }
    }

    /// Create a new MCP server with `launch_app` disabled.
    pub fn new_no_show(
        socket_path: PathBuf,
        blob_base_url: Option<String>,
        blob_store_path: Option<PathBuf>,
        initial_daemon_state: DaemonState,
    ) -> Self {
        let mut server = Self::new(
            socket_path,
            blob_base_url,
            blob_store_path,
            initial_daemon_state,
        );
        server.no_show = true;
        server
    }

    /// Get the peer label for notebook connections.
    pub async fn get_peer_label(&self) -> String {
        self.peer_label.read().await.clone()
    }

    /// Get the shared daemon state (for the health monitor).
    pub fn daemon_state(&self) -> &Arc<RwLock<DaemonState>> {
        &self.daemon_state
    }

    /// Get the shared session (for the health monitor).
    pub fn session(&self) -> &Arc<RwLock<Option<NotebookSession>>> {
        &self.session
    }

    /// Get the shared peer label (for the health monitor).
    pub fn peer_label_shared(&self) -> &Arc<RwLock<String>> {
        &self.peer_label
    }
}

impl ServerHandler for NteractMcp {
    fn get_info(&self) -> ServerInfo {
        // Advertise MCP Apps extension for output rendering
        let mut extensions = rmcp::model::ExtensionCapabilities::new();
        #[allow(clippy::unwrap_used)] // static JSON, always valid
        extensions.insert(
            "io.modelcontextprotocol/ui".to_string(),
            serde_json::from_value(serde_json::json!({})).unwrap(),
        );

        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_extensions_with(extensions)
                .build(),
        )
        .with_server_info(Implementation::new("nteract", env!("CARGO_PKG_VERSION")))
        .with_instructions(
            "nteract MCP server for Jupyter notebooks. \
             Each connection has one active notebook session. \
             Use list_active_notebooks to discover open notebooks, \
             then open_notebook or create_notebook to set your active session. \
             Calling these again switches your active session.",
        )
    }

    /// Accept logging/setLevel requests (no-op — we don't change log level dynamically).
    /// Without this, MCP Jam gets an error on connect.
    async fn set_level(
        &self,
        _request: rmcp::model::SetLevelRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        Ok(())
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let mut tools = tools::all_tools();
        if self.no_show {
            tools.retain(|t| t.name.as_ref() != "launch_app");
        }
        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        // Sniff client name on first call for use as the notebook peer label.
        // The title (e.g., "Claude Desktop") is preferred over the name ("claude-desktop").
        {
            let current = self.peer_label.read().await;
            if *current == "Inkwell" {
                drop(current);
                if let Some(info) = context.peer.peer_info() {
                    let label = info
                        .client_info
                        .title
                        .as_deref()
                        .unwrap_or(&info.client_info.name);
                    if !label.is_empty() {
                        *self.peer_label.write().await = label.to_string();
                    }
                }
            }
        }
        let start = std::time::Instant::now();
        let result = tools::dispatch(self, &request).await;
        if tracing::enabled!(tracing::Level::DEBUG) {
            let elapsed = start.elapsed();
            log_mcp_response(&request.name, elapsed, &result);
        }
        result
    }

    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        resources::list_resources(self).await
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        resources::read_resource(self, &request).await
    }

    async fn list_resource_templates(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult {
            resource_templates: vec![],
            next_cursor: None,
            meta: None,
        })
    }
}

/// Analyze and log an MCP tool response for SDK crash investigation.
///
/// Logs payload size, content summary, problematic bytes, and timing at debug
/// level for every response. Escalates to warn for payloads that are likely to
/// trigger SDK message reader crashes (large payloads, null bytes, malformed content).
fn log_mcp_response(tool_name: &str, elapsed: Duration, result: &Result<CallToolResult, McpError>) {
    match result {
        Ok(ref call_result) => {
            let serialized = serde_json::to_string(call_result).unwrap_or_default();
            let payload_bytes = serialized.len();
            let elapsed_ms = elapsed.as_millis();

            let content_count = call_result.content.len();
            let has_structured = call_result.structured_content.is_some();
            let is_error = call_result.is_error.unwrap_or(false);

            // Scan for problematic bytes
            let null_bytes = serialized.bytes().filter(|&b| b == 0).count();
            let control_chars = serialized
                .bytes()
                .filter(|&b| b < 0x20 && b != b'\n' && b != b'\r' && b != b'\t')
                .count();

            // Content item summaries
            let mut item_summaries: Vec<String> = Vec::new();
            for (i, item) in call_result.content.iter().enumerate() {
                let serialized_item = serde_json::to_string(item).unwrap_or_default();
                let item_bytes = serialized_item.len();
                let preview = if serialized_item.len() > 200 {
                    let end = serialized_item.floor_char_boundary(200);
                    format!("{}...", &serialized_item[..end])
                } else {
                    serialized_item
                };
                item_summaries.push(format!("  [{i}] {item_bytes}B: {preview}"));
            }

            let structured_bytes = call_result
                .structured_content
                .as_ref()
                .map(|sc| serde_json::to_string(sc).unwrap_or_default().len())
                .unwrap_or(0);

            if null_bytes > 0 || control_chars > 0 || payload_bytes > 512 * 1024 {
                tracing::warn!(
                    "[mcp-response] tool={tool_name} PROBLEMATIC \
                     payload={payload_bytes}B elapsed={elapsed_ms}ms \
                     items={content_count} structured={has_structured} \
                     structured_bytes={structured_bytes} error={is_error} \
                     null_bytes={null_bytes} control_chars={control_chars}"
                );
                for summary in &item_summaries {
                    tracing::warn!("[mcp-response] {tool_name} {summary}");
                }
                if has_structured {
                    let sc_preview = call_result
                        .structured_content
                        .as_ref()
                        .and_then(|sc| serde_json::to_string(sc).ok())
                        .unwrap_or_default();
                    let sc_preview = if sc_preview.len() > 500 {
                        let end = sc_preview.floor_char_boundary(500);
                        format!("{}...", &sc_preview[..end])
                    } else {
                        sc_preview
                    };
                    tracing::warn!("[mcp-response] {tool_name} structured_content: {sc_preview}");
                }
            } else {
                tracing::debug!(
                    "[mcp-response] tool={tool_name} \
                     payload={payload_bytes}B elapsed={elapsed_ms}ms \
                     items={content_count} structured={has_structured} \
                     structured_bytes={structured_bytes} error={is_error}"
                );
                for summary in &item_summaries {
                    tracing::debug!("[mcp-response] {tool_name} {summary}");
                }
            }
        }
        Err(ref err) => {
            let elapsed_ms = elapsed.as_millis();
            tracing::debug!(
                "[mcp-response] tool={tool_name} ERROR elapsed={elapsed_ms}ms code={:?} message={}",
                err.code,
                err.message,
            );
        }
    }
}
