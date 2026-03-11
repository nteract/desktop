//! Markdown image extraction for resolving relative image paths.
//!
//! This module provides utilities for extracting image references from markdown
//! content and determining which ones need to be resolved from disk.

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

/// Extract relative image paths from markdown source.
///
/// Returns a list of image paths that:
/// - Are relative (not absolute, not URLs, not data URIs)
/// - Are not already using Jupyter's attachment syntax
///
/// These paths need to be resolved against the notebook directory and stored
/// in the blob store.
pub fn extract_relative_images(source: &str) -> Vec<String> {
    let parser = Parser::new(source);
    let mut paths = Vec::new();
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
                        if is_relative_path(&dest) {
                            paths.push(dest);
                        }
                    }
                }
                in_image = false;
            }
            _ => {}
        }
    }

    paths
}

/// Check if a path is relative and needs resolution.
///
/// Returns false for:
/// - Absolute paths (starting with /)
/// - HTTP/HTTPS URLs
/// - Data URIs
/// - Blob URLs
/// - Jupyter attachment references (attachment:filename.png)
fn is_relative_path(path: &str) -> bool {
    let path = path.trim();

    // Empty paths
    if path.is_empty() {
        return false;
    }

    // Absolute URLs
    if path.starts_with("http://") || path.starts_with("https://") {
        return false;
    }

    // Data URIs
    if path.starts_with("data:") {
        return false;
    }

    // Blob URLs
    if path.starts_with("blob:") {
        return false;
    }

    // Absolute file paths
    if path.starts_with('/') {
        return false;
    }

    // Windows absolute paths
    if path.len() >= 2 && path.chars().nth(1) == Some(':') {
        return false;
    }

    // Jupyter attachment references
    if path.starts_with("attachment:") {
        return false;
    }

    true
}

/// Determine the media type from a file extension.
pub fn media_type_from_extension(path: &str) -> &'static str {
    let ext = path
        .rsplit('.')
        .next()
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "tiff" | "tif" => "image/tiff",
        "avif" => "image/avif",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_simple_image() {
        let md = "![alt](image.png)";
        let paths = extract_relative_images(md);
        assert_eq!(paths, vec!["image.png"]);
    }

    #[test]
    fn test_extract_relative_path() {
        let md = "![diagram](assets/diagram.png)";
        let paths = extract_relative_images(md);
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
        let paths = extract_relative_images(md);
        assert_eq!(paths, vec!["img1.png", "images/img2.jpg"]);
    }

    #[test]
    fn test_ignore_http_url() {
        let md = "![remote](https://example.com/image.png)";
        let paths = extract_relative_images(md);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_ignore_data_uri() {
        let md = "![inline](data:image/png;base64,iVBORw0KGgo=)";
        let paths = extract_relative_images(md);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_ignore_absolute_path() {
        let md = "![absolute](/usr/share/image.png)";
        let paths = extract_relative_images(md);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_ignore_attachment_syntax() {
        let md = "![attached](attachment:image.png)";
        let paths = extract_relative_images(md);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_ignore_blob_url() {
        let md = "![blob](blob:abc123)";
        let paths = extract_relative_images(md);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_media_type_png() {
        assert_eq!(media_type_from_extension("image.png"), "image/png");
        assert_eq!(media_type_from_extension("IMAGE.PNG"), "image/png");
    }

    #[test]
    fn test_media_type_jpeg() {
        assert_eq!(media_type_from_extension("photo.jpg"), "image/jpeg");
        assert_eq!(media_type_from_extension("photo.jpeg"), "image/jpeg");
    }

    #[test]
    fn test_media_type_svg() {
        assert_eq!(media_type_from_extension("icon.svg"), "image/svg+xml");
    }

    #[test]
    fn test_media_type_unknown() {
        assert_eq!(
            media_type_from_extension("file.xyz"),
            "application/octet-stream"
        );
    }
}
