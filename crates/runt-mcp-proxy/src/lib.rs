//! Resilient MCP proxy for `runt mcp`.
//!
//! Provides a reusable proxy core that spawns `runt mcp` as a child process,
//! forwards MCP tools/resources, and handles transparent restart on child death.
//!
//! Used by:
//! - `nteract-mcp` — shipped as a sidecar in the nteract app, inside the `.mcpb` Claude Desktop extension, and in the Claude Code plugin
//! - `mcp-supervisor` — dev environment MCP proxy with file watching and build management

// Allow `expect()` and `unwrap()` in tests
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod child;
pub mod circuit_breaker;
pub mod proxy;
pub mod session;
pub mod tools;
pub mod version;

pub use proxy::{McpProxy, ProxyConfig};
