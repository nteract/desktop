use super::*;
use base64::Engine;
use std::fmt;

pub(crate) type AttachmentRefs = HashMap<String, HashMap<String, AttachmentRef>>;

#[derive(Debug)]
pub(crate) enum AttachmentIngestError {
    InvalidPayload(String),
    StoreFailed(String),
}

impl fmt::Display for AttachmentIngestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AttachmentIngestError::InvalidPayload(msg)
            | AttachmentIngestError::StoreFailed(msg) => f.write_str(msg),
        }
    }
}

#[derive(Debug)]
pub(crate) enum AttachmentResolveError {
    MissingBlob(String),
    BlobReadFailed(String),
    InvalidPayload(String),
}

impl fmt::Display for AttachmentResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AttachmentResolveError::MissingBlob(msg)
            | AttachmentResolveError::BlobReadFailed(msg)
            | AttachmentResolveError::InvalidPayload(msg) => f.write_str(msg),
        }
    }
}

pub(crate) async fn nbformat_attachments_to_blob_refs(
    attachments: Option<&serde_json::Value>,
    blob_store: &BlobStore,
) -> Result<AttachmentRefs, AttachmentIngestError> {
    let Some(attachments) = attachments.and_then(|v| v.as_object()) else {
        return Ok(HashMap::new());
    };

    let mut refs = HashMap::new();
    for (name, bundle_value) in attachments {
        let bundle = bundle_value.as_object().ok_or_else(|| {
            AttachmentIngestError::InvalidPayload(format!(
                "attachment {name} must be a MIME bundle object"
            ))
        })?;
        let mut media_refs = HashMap::new();
        for (media_type, payload) in bundle {
            let (data, encoding) = decode_attachment_payload(media_type, payload)?;
            let hash = blob_store.put(&data, media_type).await.map_err(|e| {
                AttachmentIngestError::StoreFailed(format!(
                    "failed to store attachment {name} ({media_type}): {e}"
                ))
            })?;
            media_refs.insert(
                media_type.clone(),
                AttachmentRef {
                    blob_hash: hash,
                    encoding,
                },
            );
        }
        if !media_refs.is_empty() {
            refs.insert(name.clone(), media_refs);
        }
    }
    Ok(refs)
}

pub(crate) async fn attachment_refs_to_nbformat_value(
    refs: &AttachmentRefs,
    blob_store: &BlobStore,
) -> Result<serde_json::Value, AttachmentResolveError> {
    let mut attachments = serde_json::Map::new();
    for (name, bundle) in refs {
        let mut media_bundle = serde_json::Map::new();
        for (media_type, attachment_ref) in bundle {
            let data = blob_store
                .get(&attachment_ref.blob_hash)
                .await
                .map_err(|e| {
                    AttachmentResolveError::BlobReadFailed(format!(
                        "failed to read attachment blob {} for {name} ({media_type}): {e}",
                        attachment_ref.blob_hash
                    ))
                })?
                .ok_or_else(|| {
                    AttachmentResolveError::MissingBlob(format!(
                        "missing attachment blob {} for {name} ({media_type})",
                        attachment_ref.blob_hash
                    ))
                })?;
            media_bundle.insert(
                media_type.clone(),
                encode_attachment_payload(media_type, &attachment_ref.encoding, &data)?,
            );
        }
        if !media_bundle.is_empty() {
            attachments.insert(name.clone(), serde_json::Value::Object(media_bundle));
        }
    }
    Ok(serde_json::Value::Object(attachments))
}

pub(crate) fn image_attachment_hash(refs: &AttachmentRefs, name: &str) -> Option<String> {
    let attachment = refs
        .get(name)
        .or_else(|| refs.get(strip_query_and_fragment(name)))?;
    const PREFERRED_IMAGE_MEDIA_TYPES: &[&str] = &[
        "image/png",
        "image/jpeg",
        "image/gif",
        "image/webp",
        "image/svg+xml",
        "image/avif",
        "image/bmp",
        "image/x-icon",
        "image/tiff",
    ];

    for media_type in PREFERRED_IMAGE_MEDIA_TYPES {
        if let Some(attachment_ref) = attachment.get(*media_type) {
            return Some(attachment_ref.blob_hash.clone());
        }
    }

    let mut image_refs: Vec<_> = attachment
        .iter()
        .filter(|(media_type, _)| media_type.starts_with("image/"))
        .collect();
    image_refs.sort_by(|(left, _), (right, _)| left.cmp(right));
    image_refs
        .into_iter()
        .next()
        .map(|(_, attachment_ref)| attachment_ref.blob_hash.clone())
}

pub(crate) fn resolved_attachment_assets(
    source: &str,
    refs: &AttachmentRefs,
) -> HashMap<String, String> {
    let mut resolved = HashMap::new();
    for asset_ref in extract_markdown_asset_refs(source) {
        let Some(name) = asset_ref.strip_prefix("attachment:") else {
            continue;
        };
        if let Some(hash) = image_attachment_hash(refs, name) {
            resolved.insert(asset_ref, hash);
        }
    }
    resolved
}

fn decode_attachment_payload(
    media_type: &str,
    payload: &serde_json::Value,
) -> Result<(Vec<u8>, AttachmentEncoding), AttachmentIngestError> {
    if attachment_payload_is_json(media_type) {
        return serde_json::to_vec(payload)
            .map(|data| (data, AttachmentEncoding::Json))
            .map_err(|e| {
                AttachmentIngestError::InvalidPayload(format!(
                    "attachment {media_type} JSON payload is invalid: {e}"
                ))
            });
    }

    if let Some(payload) = payload.as_str() {
        if attachment_payload_is_text(media_type) {
            return Ok((payload.as_bytes().to_vec(), AttachmentEncoding::Text));
        }
        return base64::engine::general_purpose::STANDARD
            .decode(payload)
            .map(|data| (data, AttachmentEncoding::Base64))
            .map_err(|e| {
                AttachmentIngestError::InvalidPayload(format!(
                    "attachment {media_type} base64 payload is invalid: {e}"
                ))
            });
    }

    serde_json::to_vec(payload)
        .map(|data| (data, AttachmentEncoding::Json))
        .map_err(|e| {
            AttachmentIngestError::InvalidPayload(format!(
                "attachment {media_type} JSON payload is invalid: {e}"
            ))
        })
}

fn encode_attachment_payload(
    media_type: &str,
    encoding: &AttachmentEncoding,
    data: &[u8],
) -> Result<serde_json::Value, AttachmentResolveError> {
    match encoding {
        AttachmentEncoding::Json => serde_json::from_slice(data).map_err(|e| {
            AttachmentResolveError::InvalidPayload(format!(
                "attachment {media_type} JSON payload is invalid: {e}"
            ))
        }),
        AttachmentEncoding::Text => encode_text_attachment(media_type, data),
        AttachmentEncoding::Base64 if attachment_payload_is_text(media_type) => {
            encode_text_attachment(media_type, data)
        }
        AttachmentEncoding::Base64 => Ok(serde_json::Value::String(
            base64::engine::general_purpose::STANDARD.encode(data),
        )),
        AttachmentEncoding::Unknown(value) => Err(AttachmentResolveError::InvalidPayload(format!(
            "attachment {media_type} has unknown encoding {value}"
        ))),
    }
}

fn encode_text_attachment(
    media_type: &str,
    data: &[u8],
) -> Result<serde_json::Value, AttachmentResolveError> {
    let text = std::str::from_utf8(data).map_err(|e| {
        AttachmentResolveError::InvalidPayload(format!(
            "attachment {media_type} is not valid UTF-8: {e}"
        ))
    })?;
    Ok(serde_json::Value::String(text.to_string()))
}

fn attachment_payload_is_json(media_type: &str) -> bool {
    media_type == "application/json" || media_type.ends_with("+json")
}

fn attachment_payload_is_text(media_type: &str) -> bool {
    media_type.starts_with("text/") || media_type == "image/svg+xml"
}

fn strip_query_and_fragment(path: &str) -> &str {
    path.split(['?', '#']).next().unwrap_or(path)
}
