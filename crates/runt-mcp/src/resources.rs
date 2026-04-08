//! MCP resource serving (output.html, notebook cells, status).

use rmcp::model::{
    Annotated, ListResourcesResult, Meta, RawResource, ReadResourceRequestParams,
    ReadResourceResult, ResourceContents,
};
use rmcp::ErrorData as McpError;

use crate::NteractMcp;

const OUTPUT_RESOURCE_URI: &str = "ui://nteract/output.html";
const OUTPUT_MIME_TYPE: &str = "text/html;profile=mcp-app";

/// The compiled output renderer HTML, built by `apps/mcp-app/build-html.js`.
/// Build with: `cd apps/mcp-app && pnpm build`
/// The build script copies the file to `crates/runt-mcp/assets/_output.html`.
const OUTPUT_HTML: &str = include_str!("../assets/_output.html");

/// Build `_meta` for the output widget resource with CSP domains.
///
/// MCP Apps spec CSP fields (from ext-apps specification):
/// - `resourceDomains` → `img-src`, `script-src`, `style-src`, `font-src`, `media-src`
/// - `connectDomains`  → `connect-src` (fetch/XHR/WebSocket)
///
/// The daemon's blob HTTP server URL is needed in both: `resourceDomains` for
/// loading plugin JS/CSS via `<script>`/`<link>` tags, and `connectDomains` for
/// `fetch()` calls to resolve blob-stored output data (plotly JSON, geojson, etc.).
///
/// Claude Desktop requires `localhost` (not `127.0.0.1`) for domain allowlists.
fn resource_ui_meta(blob_base_url: &Option<String>) -> Option<Meta> {
    let url = blob_base_url.as_ref()?;
    let mut meta = serde_json::Map::new();
    meta.insert(
        "ui".to_string(),
        serde_json::json!({
            "csp": {
                "resourceDomains": [
                    url,
                    // CartoDB basemap tiles used by the Leaflet renderer plugin
                    "https://*.basemaps.cartocdn.com",
                ],
                "connectDomains": [url]
            }
        }),
    );
    Some(Meta(meta))
}

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
    server: &NteractMcp,
    request: &ReadResourceRequestParams,
) -> Result<ReadResourceResult, McpError> {
    let uri = request.uri.as_str();

    if uri == OUTPUT_RESOURCE_URI {
        return Ok(ReadResourceResult::new(vec![
            ResourceContents::TextResourceContents {
                uri: OUTPUT_RESOURCE_URI.into(),
                mime_type: Some(OUTPUT_MIME_TYPE.into()),
                text: OUTPUT_HTML.to_string(),
                meta: resource_ui_meta(&server.blob_base_url),
            },
        ]));
    }

    Err(McpError::resource_not_found(
        format!("Unknown resource: {}", uri),
        None,
    ))
}
