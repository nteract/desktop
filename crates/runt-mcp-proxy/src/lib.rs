//! Resilient MCP proxy for `runt mcp`.
//!
//! Provides a reusable proxy core that spawns `runt mcp` as a child process,
//! forwards MCP tools/resources, and handles transparent restart on child death.
//!
//! Used by:
//! - `runt-proxy` — shipped as a sidecar in the nteract app and inside the `.mcpb` Claude Desktop extension
//! - `mcp-supervisor` — dev environment MCP proxy with file watching and build management

// Tests are allowed to use unwrap()/expect()—they're how you assert
// preconditions and keep test failures informative. Workspace-wide
// `clippy::unwrap_used = "warn"` applies to non-test code.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod child;
pub mod circuit_breaker;
pub mod proxy;
pub mod session;
pub mod tools;
pub mod version;

pub use proxy::{McpProxy, ProxyConfig};
