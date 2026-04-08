//! Rust-native MCP server for nteract notebook interaction.
//!
//! Implements the MCP protocol using `rmcp`, backed by `runtimed-client`
//! for daemon IPC and `notebook-sync` for Automerge document operations.

use std::path::PathBuf;
use std::sync::Arc;

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
        tools::dispatch(self, &request).await
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
