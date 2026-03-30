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
pub mod presence;
mod resources;
mod session;
mod structured;
pub mod tools;

use session::NotebookSession;

/// The nteract MCP server.
pub struct NteractMcp {
    socket_path: PathBuf,
    blob_base_url: Option<String>,
    blob_store_path: Option<PathBuf>,
    session: Arc<RwLock<Option<NotebookSession>>>,
}

impl NteractMcp {
    /// Create a new MCP server instance.
    pub fn new(
        socket_path: PathBuf,
        blob_base_url: Option<String>,
        blob_store_path: Option<PathBuf>,
    ) -> Self {
        Self {
            socket_path,
            blob_base_url,
            blob_store_path,
            session: Arc::new(RwLock::new(None)),
        }
    }
}

impl ServerHandler for NteractMcp {
    fn get_info(&self) -> ServerInfo {
        // Advertise MCP Apps extension for output rendering
        let mut extensions = rmcp::model::ExtensionCapabilities::new();
        #[allow(clippy::unwrap_used)] // static JSON, always valid
        extensions.insert(
            "io.modelcontextprotocol/apps".to_string(),
            serde_json::from_value(serde_json::json!({})).unwrap(),
        );

        ServerInfo {
            protocol_version: Default::default(),
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_extensions_with(extensions)
                .build(),
            server_info: Implementation {
                name: "nteract".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                ..Default::default()
            },
            instructions: Some(
                "nteract MCP server for AI-powered Jupyter notebooks. \
                 Use list_active_notebooks to discover open notebooks, \
                 then join_notebook or open_notebook to start working."
                    .into(),
            ),
        }
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            tools: tools::all_tools(),
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
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
