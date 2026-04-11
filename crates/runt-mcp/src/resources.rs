//! MCP resource serving (output.html, notebook cells, status).

use rmcp::model::{
    Annotated, ListResourceTemplatesResult, ListResourcesResult, Meta, RawResource,
    RawResourceTemplate, ReadResourceRequestParams, ReadResourceResult, ResourceContents,
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

/// List available MCP resource templates.
pub async fn list_resource_templates(
    server: &NteractMcp,
) -> Result<ListResourceTemplatesResult, McpError> {
    let mut templates = vec![
        // Output renderer (static resource, not a template)
        Annotated {
            raw: RawResourceTemplate {
                uri_template: OUTPUT_RESOURCE_URI.to_string(),
                name: "nteract output".to_string(),
                title: None,
                description: Some("Interactive output renderer for notebook cells".into()),
                mime_type: Some(OUTPUT_MIME_TYPE.into()),
                icons: None,
            },
            annotations: None,
        },
    ];

    // If a session is active, add notebook resource templates
    if let Some(session) = server.session().read().await.as_ref() {
        let notebook_id = &session.notebook_id;

        templates.extend(vec![
            Annotated {
                raw: RawResourceTemplate {
                    uri_template: format!("notebook://{}/cells", notebook_id),
                    name: "All cells".to_string(),
                    title: None,
                    description: Some("List of all cells in the notebook".into()),
                    mime_type: Some("application/json".into()),
                    icons: None,
                },
                annotations: None,
            },
            Annotated {
                raw: RawResourceTemplate {
                    uri_template: format!("notebook://{}/cell/{{cell_id}}", notebook_id),
                    name: "Single cell".to_string(),
                    title: None,
                    description: Some("Cell with source, metadata, and outputs".into()),
                    mime_type: Some("application/json".into()),
                    icons: None,
                },
                annotations: None,
            },
            Annotated {
                raw: RawResourceTemplate {
                    uri_template: format!("notebook://{}/cell/{{cell_id}}/source", notebook_id),
                    name: "Cell source".to_string(),
                    title: None,
                    description: Some("Cell source code as plain text".into()),
                    mime_type: Some("text/plain".into()),
                    icons: None,
                },
                annotations: None,
            },
            Annotated {
                raw: RawResourceTemplate {
                    uri_template: format!("notebook://{}/cell/{{cell_id}}/outputs", notebook_id),
                    name: "Cell outputs".to_string(),
                    title: None,
                    description: Some("Cell execution outputs".into()),
                    mime_type: Some("application/json".into()),
                    icons: None,
                },
                annotations: None,
            },
            Annotated {
                raw: RawResourceTemplate {
                    uri_template: format!("notebook://{}/status", notebook_id),
                    name: "Runtime status".to_string(),
                    title: None,
                    description: Some(
                        "Kernel status, execution queue, and environment info".into(),
                    ),
                    mime_type: Some("application/json".into()),
                    icons: None,
                },
                annotations: None,
            },
            Annotated {
                raw: RawResourceTemplate {
                    uri_template: format!("notebook://{}/dependencies", notebook_id),
                    name: "Dependencies".to_string(),
                    title: None,
                    description: Some("PEP 723 inline script dependencies".into()),
                    mime_type: Some("application/json".into()),
                    icons: None,
                },
                annotations: None,
            },
        ]);
    }

    Ok(ListResourceTemplatesResult {
        resource_templates: templates,
        next_cursor: None,
        meta: None,
    })
}

/// List available MCP resources (for backwards compatibility).
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

    // Try notebook:// URIs
    if uri.starts_with("notebook://") {
        return read_notebook_resource(server, uri).await;
    }

    Err(McpError::resource_not_found(
        format!("Unknown resource: {}", uri),
        None,
    ))
}

/// Parse and read notebook:// resources.
async fn read_notebook_resource(
    server: &NteractMcp,
    uri: &str,
) -> Result<ReadResourceResult, McpError> {
    let session_guard = server.session().read().await;
    let session = session_guard.as_ref().ok_or_else(|| {
        McpError::resource_not_found(
            "No active notebook session. Use open_notebook or join_notebook first.",
            None,
        )
    })?;

    // Parse URI: notebook://{notebook_id}/{resource}
    let uri_path = uri
        .strip_prefix("notebook://")
        .ok_or_else(|| McpError::invalid_params("URI must start with notebook://", None))?;
    let parts: Vec<&str> = uri_path.split('/').collect();

    if parts.len() < 2 {
        return Err(McpError::invalid_params(
            "Invalid notebook URI format. Expected: notebook://{id}/cells, notebook://{id}/cell/{cell_id}, etc.",
            None,
        ));
    }

    let notebook_id = parts[0];
    let resource_type = parts[1];

    // Verify notebook ID matches current session
    if notebook_id != session.notebook_id {
        return Err(McpError::resource_not_found(
            format!(
                "Notebook ID mismatch. Current session: {}, requested: {}",
                session.notebook_id, notebook_id
            ),
            None,
        ));
    }

    match resource_type {
        "cells" => {
            // Return all cells as JSON
            let cells = session.handle.get_cells();
            let json = serde_json::to_string_pretty(&cells).map_err(|e| {
                McpError::internal_error(format!("Failed to serialize cells: {}", e), None)
            })?;

            Ok(ReadResourceResult::new(vec![
                ResourceContents::TextResourceContents {
                    uri: uri.to_string(),
                    mime_type: Some("application/json".into()),
                    text: json,
                    meta: None,
                },
            ]))
        }
        "cell" => {
            // Get specific cell: notebook://{id}/cell/{cell_id} or notebook://{id}/cell/{cell_id}/source or /outputs
            if parts.len() < 3 {
                return Err(McpError::invalid_params(
                    "Cell ID required. Use: notebook://{id}/cell/{cell_id}",
                    None,
                ));
            }

            let cell_id = parts[2];
            let snapshot = session.handle.snapshot();
            let cell = snapshot.get_cell(cell_id).ok_or_else(|| {
                McpError::resource_not_found(format!("Cell not found: {}", cell_id), None)
            })?;

            // Check for sub-resource (/source or /outputs)
            if parts.len() >= 4 {
                match parts[3] {
                    "source" => {
                        // Return just the source as plain text
                        Ok(ReadResourceResult::new(vec![
                            ResourceContents::TextResourceContents {
                                uri: uri.to_string(),
                                mime_type: Some("text/plain".into()),
                                text: cell.source.clone(),
                                meta: None,
                            },
                        ]))
                    }
                    "outputs" => {
                        // Return outputs as JSON
                        let json = serde_json::to_string_pretty(&cell.outputs).map_err(|e| {
                            McpError::internal_error(
                                format!("Failed to serialize outputs: {}", e),
                                None,
                            )
                        })?;

                        Ok(ReadResourceResult::new(vec![
                            ResourceContents::TextResourceContents {
                                uri: uri.to_string(),
                                mime_type: Some("application/json".into()),
                                text: json,
                                meta: None,
                            },
                        ]))
                    }
                    _ => Err(McpError::resource_not_found(
                        format!("Unknown cell sub-resource: {}", parts[3]),
                        None,
                    )),
                }
            } else {
                // Return full cell as JSON
                let json = serde_json::to_string_pretty(&cell).map_err(|e| {
                    McpError::internal_error(format!("Failed to serialize cell: {}", e), None)
                })?;

                Ok(ReadResourceResult::new(vec![
                    ResourceContents::TextResourceContents {
                        uri: uri.to_string(),
                        mime_type: Some("application/json".into()),
                        text: json,
                        meta: None,
                    },
                ]))
            }
        }
        "status" => {
            // Return runtime status (would need to get from RuntimeStateDoc)
            // For now, return a placeholder
            let status = serde_json::json!({
                "kernel_status": "unknown",
                "message": "Runtime status not yet implemented"
            });

            let json = serde_json::to_string_pretty(&status).map_err(|e| {
                McpError::internal_error(format!("Failed to serialize status: {}", e), None)
            })?;

            Ok(ReadResourceResult::new(vec![
                ResourceContents::TextResourceContents {
                    uri: uri.to_string(),
                    mime_type: Some("application/json".into()),
                    text: json,
                    meta: None,
                },
            ]))
        }
        "dependencies" => {
            // Return dependencies from notebook metadata (runt.uv.dependencies)
            let snapshot = session.handle.snapshot();
            let deps = snapshot
                .notebook_metadata()
                .and_then(|m| m.runt.uv.as_ref())
                .map(|uv| uv.dependencies.clone())
                .unwrap_or_default();

            let json = serde_json::to_string_pretty(&deps).map_err(|e| {
                McpError::internal_error(format!("Failed to serialize dependencies: {}", e), None)
            })?;

            Ok(ReadResourceResult::new(vec![
                ResourceContents::TextResourceContents {
                    uri: uri.to_string(),
                    mime_type: Some("application/json".into()),
                    text: json,
                    meta: None,
                },
            ]))
        }
        _ => Err(McpError::resource_not_found(
            format!("Unknown notebook resource type: {}", resource_type),
            None,
        )),
    }
}
