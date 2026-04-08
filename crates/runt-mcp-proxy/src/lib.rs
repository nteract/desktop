//! Resilient MCP proxy for `runt mcp`.
//!
//! Provides a reusable proxy core that spawns `runt mcp` as a child process,
//! forwards MCP tools/resources, and handles transparent restart on child death.
//!
//! Used by:
//! - `mcpb-runt` — lightweight MCPB binary for Claude Desktop
//! - `mcp-supervisor` — dev environment MCP proxy with file watching and build management

pub mod child;
pub mod circuit_breaker;
pub mod proxy;
pub mod session;
pub mod tools;
pub mod version;

pub use proxy::{McpProxy, ProxyConfig};
