//! MCP resource serving (output.html, notebook cells, status).

use rmcp::model::{
    Annotated, ListResourcesResult, RawResource, ReadResourceRequestParams, ReadResourceResult,
    ResourceContents,
};
use rmcp::ErrorData as McpError;

use crate::NteractMcp;

const OUTPUT_RESOURCE_URI: &str = "ui://nteract/output.html";
const OUTPUT_MIME_TYPE: &str = "text/html;profile=mcp-app";

/// The compiled output renderer HTML, built by `apps/mcp-app/build-html.js`.
/// Build with: `cd apps/mcp-app && pnpm build`
/// The build script copies the file to `crates/runt-mcp/assets/_output.html`.
const OUTPUT_HTML: &str = include_str!("../assets/_output.html");

/// List available MCP resources.
pub async fn list_resources(_server: &NteractMcp) -> Result<ListResourcesResult, McpError> {
    let mut raw = RawResource::new(OUTPUT_RESOURCE_URI, "nteract output");
    raw.description = Some("Interactive output renderer for notebook cells".into());
    raw.mime_type = Some(OUTPUT_MIME_TYPE.into());

    let resources = vec![Annotated {
        raw,
        annotations: None,
    }];

    Ok(ListResourcesResult {
        resources,
        next_cursor: None,
        meta: None,
    })
}

/// Read an MCP resource by URI.
pub async fn read_resource(
    _server: &NteractMcp,
    request: &ReadResourceRequestParams,
) -> Result<ReadResourceResult, McpError> {
    let uri = request.uri.as_str();

    if uri == OUTPUT_RESOURCE_URI {
        return Ok(ReadResourceResult::new(vec![
            ResourceContents::TextResourceContents {
                uri: OUTPUT_RESOURCE_URI.into(),
                mime_type: Some(OUTPUT_MIME_TYPE.into()),
                text: OUTPUT_HTML.to_string(),
                meta: None,
            },
        ]));
    }

    Err(McpError::resource_not_found(
        format!("Unknown resource: {}", uri),
        None,
    ))
}
