//! Markdown asset extraction and resolution.
//!
//! Markdown rendered in isolated iframes cannot load notebook-relative files or
//! nbformat attachments directly. This module resolves those asset references
//! into blob-store hashes so the frontend can rewrite them to blob URLs.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

use base64::Engine;
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use regex::Regex;

use crate::blob_store::BlobStore;

/// Maximum size for a single markdown asset (relative image or attachment).
///
/// This is intentionally lower than the generic blob-store limit because
/// markdown assets are eagerly resolved for iframe rendering.
const MAX_MARKDOWN_ASSET_SIZE: usize = 25 * 1024 * 1024;

/// Extract markdown image refs that need daemon-side resolution.
///
/// Returned refs are unique and preserve first-seen order.
pub fn extract_markdown_asset_refs(source: &str) -> Vec<String> {
    let parser = Parser::new(source);
    let mut refs = Vec::new();
    let mut seen = HashSet::new();
    let mut in_image = false;
    let mut current_dest: Option<String> = None;

    for event in parser {
        match event {
            Event::Start(Tag::Image { dest_url, .. }) => {
                in_image = true;
                current_dest = Some(dest_url.to_string());
            }
            Event::End(TagEnd::Image) => {
                if in_image {
                    if let Some(dest) = current_dest.take() {
                        if is_resolvable_asset_ref(&dest) && seen.insert(dest.clone()) {
                            refs.push(dest);
                        }
                    }
                }
                in_image = false;
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                for asset_ref in extract_html_image_refs(&html) {
                    if is_resolvable_asset_ref(&asset_ref) && seen.insert(asset_ref.clone()) {
                        refs.push(asset_ref);
                    }
                }
            }
            _ => {}
        }
    }

    refs
}

/// Resolve markdown asset refs to blob hashes.
pub async fn resolve_markdown_assets(
    source: &str,
    notebook_path: Option<&Path>,
    nbformat_attachments: Option<&serde_json::Value>,
    blob_store: &BlobStore,
) -> HashMap<String, String> {
    let mut resolved = HashMap::new();

    for asset_ref in extract_markdown_asset_refs(source) {
        let hash = if let Some(name) = asset_ref.strip_prefix("attachment:") {
            resolve_nbformat_attachment(name, nbformat_attachments, blob_store).await
        } else {
            resolve_relative_asset(&asset_ref, notebook_path, blob_store).await
        };

        if let Some(hash) = hash {
            resolved.insert(asset_ref, hash);
        }
    }

    resolved
}

/// Determine the image media type from a file extension.
pub fn media_type_from_extension(path: &str) -> Option<&'static str> {
    let ext = path
        .rsplit('.')
        .next()
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "tiff" | "tif" => "image/tiff",
        "avif" => "image/avif",
        _ => return None,
    })
}

fn is_resolvable_asset_ref(asset_ref: &str) -> bool {
    let asset_ref = asset_ref.trim();

    if asset_ref.is_empty() {
        return false;
    }

    if asset_ref.starts_with("attachment:") {
        return true;
    }

    is_relative_path(asset_ref)
}

fn is_relative_path(path: &str) -> bool {
    let path = path.trim();

    if path.is_empty() {
        return false;
    }

    if path.starts_with("http://") || path.starts_with("https://") {
        return false;
    }

    if path.starts_with("data:") || path.starts_with("blob:") {
        return false;
    }

    if path.starts_with('/') {
        return false;
    }

    if path.len() >= 2 && path.chars().nth(1) == Some(':') {
        return false;
    }

    true
}

fn strip_query_and_fragment(path: &str) -> &str {
    path.split(['?', '#']).next().unwrap_or(path)
}

fn extract_html_image_refs(html: &str) -> Vec<String> {
    static IMG_SRC_RE: OnceLock<Result<Regex, regex::Error>> = OnceLock::new();
    let Ok(re) = IMG_SRC_RE.get_or_init(|| {
        Regex::new(r#"(?is)<img\b[^>]*\bsrc\s*=\s*(?:"([^"]+)"|'([^']+)'|([^\s"'=<>`]+))"#)
    }) else {
        return Vec::new();
    };

    re.captures_iter(html)
        .filter_map(|caps| {
            caps.get(1)
                .or_else(|| caps.get(2))
                .or_else(|| caps.get(3))
                .map(|m| m.as_str().to_string())
        })
        .collect()
}

async fn resolve_relative_asset(
    asset_ref: &str,
    notebook_path: Option<&Path>,
    blob_store: &BlobStore,
) -> Option<String> {
    let notebook_dir = notebook_path?.parent()?;
    let canonical_dir = notebook_dir.canonicalize().ok()?;
    let cleaned_ref = strip_query_and_fragment(asset_ref);
    let media_type = media_type_from_extension(cleaned_ref)?;

    let resolved = notebook_dir.join(cleaned_ref);
    let canonical = resolved.canonicalize().ok()?;
    if !canonical.starts_with(&canonical_dir) {
        return None;
    }

    let metadata = tokio::fs::metadata(&canonical).await.ok()?;
    if metadata.len() > MAX_MARKDOWN_ASSET_SIZE as u64 {
        return None;
    }

    let data = tokio::fs::read(&canonical).await.ok()?;
    blob_store.put(&data, media_type).await.ok()
}

async fn resolve_nbformat_attachment(
    name: &str,
    nbformat_attachments: Option<&serde_json::Value>,
    blob_store: &BlobStore,
) -> Option<String> {
    let attachment = nbformat_attachments?.get(strip_query_and_fragment(name))?;
    let (media_type, payload) = pick_attachment_payload(attachment)?;
    let data = decode_attachment_payload(media_type, payload)?;
    blob_store.put(&data, media_type).await.ok()
}

fn pick_attachment_payload(attachment: &serde_json::Value) -> Option<(&str, &str)> {
    let obj = attachment.as_object()?;

    obj.iter().find_map(|(media_type, payload)| {
        (media_type.starts_with("image/"))
            .then(|| payload.as_str().map(|s| (media_type.as_str(), s)))
            .flatten()
    })
}

fn decode_attachment_payload(media_type: &str, payload: &str) -> Option<Vec<u8>> {
    if !media_type.starts_with("image/") {
        return None;
    }

    if attachment_payload_too_large(media_type, payload, MAX_MARKDOWN_ASSET_SIZE) {
        return None;
    }

    if media_type == "image/svg+xml" {
        return Some(payload.as_bytes().to_vec());
    }

    base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()
}

fn attachment_payload_too_large(media_type: &str, payload: &str, max_size: usize) -> bool {
    if media_type == "image/svg+xml" {
        return payload.len() > max_size;
    }

    estimated_base64_decoded_len(payload) > max_size
}

fn estimated_base64_decoded_len(payload: &str) -> usize {
    let non_whitespace_len = payload
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .count();
    let trimmed = payload.trim_end_matches(|c: char| c.is_ascii_whitespace());
    let padding = trimmed
        .as_bytes()
        .iter()
        .rev()
        .take_while(|byte| **byte == b'=')
        .count();

    non_whitespace_len
        .div_ceil(4)
        .saturating_mul(3)
        .saturating_sub(padding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    use crate::blob_store::BlobStore;

    #[test]
    fn test_extract_simple_image() {
        let md = "![alt](image.png)";
        let paths = extract_markdown_asset_refs(md);
        assert_eq!(paths, vec!["image.png"]);
    }

    #[test]
    fn test_extract_relative_path() {
        let md = "![diagram](assets/diagram.png)";
        let paths = extract_markdown_asset_refs(md);
        assert_eq!(paths, vec!["assets/diagram.png"]);
    }

    #[test]
    fn test_extract_multiple_images() {
        let md = r#"
# Header

![first](img1.png)

Some text

![second](images/img2.jpg)
"#;
        let paths = extract_markdown_asset_refs(md);
        assert_eq!(paths, vec!["img1.png", "images/img2.jpg"]);
    }

    #[test]
    fn test_ignore_http_url() {
        let md = "![remote](https://example.com/image.png)";
        let paths = extract_markdown_asset_refs(md);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_ignore_data_uri() {
        let md = "![inline](data:image/png;base64,iVBORw0KGgo=)";
        let paths = extract_markdown_asset_refs(md);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_ignore_absolute_path() {
        let md = "![absolute](/usr/share/image.png)";
        let paths = extract_markdown_asset_refs(md);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_ignore_blob_url() {
        let md = "![blob](blob:abc123)";
        let paths = extract_markdown_asset_refs(md);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_extract_attachment_syntax() {
        let md = "![attached](attachment:image.png)";
        let refs = extract_markdown_asset_refs(md);
        assert_eq!(refs, vec!["attachment:image.png"]);
    }

    #[test]
    fn test_extract_unique_refs() {
        let md = "![a](image.png)\n![b](image.png)";
        let refs = extract_markdown_asset_refs(md);
        assert_eq!(refs, vec!["image.png"]);
    }

    #[test]
    fn test_extract_reference_style_image() {
        let md = "![logo][img]\n\n[img]: images/logo.png";
        let refs = extract_markdown_asset_refs(md);
        assert_eq!(refs, vec!["images/logo.png"]);
    }

    #[test]
    fn test_extract_inline_html_image() {
        let md = r#"<img src="images/logo.png" alt="logo">"#;
        let refs = extract_markdown_asset_refs(md);
        assert_eq!(refs, vec!["images/logo.png"]);
    }

    #[test]
    fn test_extract_inline_html_image_unquoted_src() {
        let md = "<img src=images/logo.png alt=logo>";
        let refs = extract_markdown_asset_refs(md);
        assert_eq!(refs, vec!["images/logo.png"]);
    }

    #[test]
    fn test_extract_inline_html_image_case_insensitive() {
        let md = r#"<IMG SRC="images/logo.png" ALT="logo">"#;
        let refs = extract_markdown_asset_refs(md);
        assert_eq!(refs, vec!["images/logo.png"]);
    }

    #[test]
    fn test_ignore_malformed_inline_html_image() {
        let md = r#"<img src="images/logo.png alt="logo">"#;
        let refs = extract_markdown_asset_refs(md);
        assert!(refs.is_empty());
    }

    #[test]
    fn test_ignore_malformed_markdown_image() {
        let md = "![alt](image.png";
        let refs = extract_markdown_asset_refs(md);
        assert!(refs.is_empty());
    }

    #[test]
    fn test_ignore_code_fence_html_image() {
        let md = "```html\n<img src=\"images/logo.png\">\n```";
        let refs = extract_markdown_asset_refs(md);
        assert!(refs.is_empty());
    }

    #[test]
    fn test_media_type_png() {
        assert_eq!(media_type_from_extension("image.png"), Some("image/png"));
        assert_eq!(media_type_from_extension("IMAGE.PNG"), Some("image/png"));
    }

    #[test]
    fn test_media_type_jpeg() {
        assert_eq!(media_type_from_extension("photo.jpg"), Some("image/jpeg"));
        assert_eq!(media_type_from_extension("photo.jpeg"), Some("image/jpeg"));
    }

    #[test]
    fn test_media_type_svg() {
        assert_eq!(media_type_from_extension("icon.svg"), Some("image/svg+xml"));
    }

    #[test]
    fn test_media_type_unknown() {
        assert_eq!(media_type_from_extension("file.xyz"), None);
    }

    #[test]
    fn test_decode_svg_attachment_as_text() {
        let decoded = decode_attachment_payload("image/svg+xml", "<svg/>").unwrap();
        assert_eq!(decoded, b"<svg/>");
    }

    #[test]
    fn test_decode_png_attachment_as_base64() {
        let decoded = decode_attachment_payload("image/png", "aGVsbG8=").unwrap();
        assert_eq!(decoded, b"hello");
    }

    #[test]
    fn test_decode_attachment_rejects_non_image_type() {
        assert!(decode_attachment_payload("text/plain", "hello").is_none());
    }

    #[test]
    fn test_attachment_payload_too_large_for_svg() {
        assert!(attachment_payload_too_large("image/svg+xml", "abcdef", 5));
        assert!(!attachment_payload_too_large("image/svg+xml", "abcde", 5));
    }

    #[test]
    fn test_attachment_payload_too_large_for_base64_image() {
        let payload = "YWFhYQ==";
        assert!(attachment_payload_too_large("image/png", payload, 3));
        assert!(!attachment_payload_too_large("image/png", payload, 4));
    }

    #[tokio::test]
    async fn test_resolve_markdown_assets_ignores_non_image_relative_file() {
        let dir = TempDir::new().unwrap();
        let notebook_path = dir.path().join("notebook.ipynb");
        tokio::fs::write(dir.path().join("notes.txt"), b"hello")
            .await
            .unwrap();
        let blob_store = BlobStore::new(dir.path().join("blobs"));

        let resolved =
            resolve_markdown_assets("![alt](notes.txt)", Some(&notebook_path), None, &blob_store)
                .await;

        assert!(resolved.is_empty());
    }

    #[tokio::test]
    async fn test_resolve_markdown_assets_ignores_oversized_relative_file() {
        let dir = TempDir::new().unwrap();
        let notebook_path = dir.path().join("notebook.ipynb");
        let large_path = dir.path().join("large.png");
        let file = tokio::fs::File::create(&large_path).await.unwrap();
        file.set_len((MAX_MARKDOWN_ASSET_SIZE as u64) + 1)
            .await
            .unwrap();
        let blob_store = BlobStore::new(dir.path().join("blobs"));

        let resolved =
            resolve_markdown_assets("![alt](large.png)", Some(&notebook_path), None, &blob_store)
                .await;

        assert!(resolved.is_empty());
    }

    #[tokio::test]
    async fn test_resolve_markdown_assets_ignores_non_image_attachment() {
        let dir = TempDir::new().unwrap();
        let blob_store = BlobStore::new(dir.path().join("blobs"));
        let attachments = serde_json::json!({
            "notes.txt": {
                "text/plain": "hello"
            }
        });

        let resolved = resolve_markdown_assets(
            "![alt](attachment:notes.txt)",
            None,
            Some(&attachments),
            &blob_store,
        )
        .await;

        assert!(resolved.is_empty());
    }

    #[tokio::test]
    async fn test_resolve_markdown_assets_strips_attachment_query_and_fragment() {
        let dir = TempDir::new().unwrap();
        let blob_store = BlobStore::new(dir.path().join("blobs"));
        let attachments = serde_json::json!({
            "image.png": {
                "image/png": "aGVsbG8="
            }
        });

        let resolved = resolve_markdown_assets(
            "![alt](attachment:image.png?raw=1#fragment)",
            None,
            Some(&attachments),
            &blob_store,
        )
        .await;

        assert!(resolved.contains_key("attachment:image.png?raw=1#fragment"));
    }
}
