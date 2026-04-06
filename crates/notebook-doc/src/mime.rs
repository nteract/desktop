//! Canonical MIME type classification for notebook outputs.
//!
//! This is the single source of truth for MIME classification in the Rust
//! codebase. The three-way [`MimeKind`] enum and [`mime_kind`] classifier
//! determine how output data is stored, transferred, and displayed.
//!
//! ## Keeping copies in sync
//!
//! A TypeScript copy of this logic lives in
//! `apps/notebook/src/lib/manifest-resolution.ts` (`isBinaryMime`).
//! Any changes here **must** be mirrored there — see the `is_binary_mime`
//! contract in `AGENTS.md` for the full list of locations.

use serde::{Deserialize, Serialize};

/// Three-way classification of a MIME type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MimeKind {
    /// UTF-8 text: `text/*`, `image/svg+xml`, `application/javascript`, etc.
    Text,
    /// Raw binary bytes: `image/png`, `audio/*`, `video/*`, etc.
    Binary,
    /// JSON data: `application/json`, `*+json`, `*.json`.
    Json,
}

/// Classify a MIME type into [`MimeKind::Text`], [`MimeKind::Binary`], or
/// [`MimeKind::Json`].
///
/// The rules, in evaluation order:
///
/// 1. `application/json` → Json
/// 2. `application/*+json` or `application/*.json` → Json
/// 3. `image/*` → Binary, **except** `image/*+xml` (e.g. SVG) → Text
/// 4. `audio/*`, `video/*` → Binary
/// 5. `application/*` → Binary by default, with carve-outs for text-like
///    subtypes (`javascript`, `ecmascript`, `xml`, `xhtml+xml`, `mathml+xml`,
///    `sql`, `graphql`, `x-latex`, `x-tex`, and any `+xml` suffix)
/// 6. Everything else (`text/*`, unknown) → Text
pub fn mime_kind(mime: &str) -> MimeKind {
    // JSON types
    if mime == "application/json" {
        return MimeKind::Json;
    }
    if let Some(subtype) = mime.strip_prefix("application/") {
        if subtype.ends_with("+json") || subtype.ends_with(".json") {
            return MimeKind::Json;
        }
    }

    // Binary images (but NOT SVG — that's XML text)
    if mime.starts_with("image/") {
        return if mime.ends_with("+xml") {
            MimeKind::Text
        } else {
            MimeKind::Binary
        };
    }

    // Audio/video are always binary
    if mime.starts_with("audio/") || mime.starts_with("video/") {
        return MimeKind::Binary;
    }

    // application/* is binary by default, with carve-outs for text-like formats
    if let Some(subtype) = mime.strip_prefix("application/") {
        let is_text = subtype == "javascript"
            || subtype == "ecmascript"
            || subtype == "xml"
            || subtype == "xhtml+xml"
            || subtype == "mathml+xml"
            || subtype == "sql"
            || subtype == "graphql"
            || subtype == "x-latex"
            || subtype == "x-tex"
            || subtype.ends_with("+xml");
        return if is_text {
            MimeKind::Text
        } else {
            MimeKind::Binary
        };
    }

    // Everything else (text/*, unknown) is text
    MimeKind::Text
}

/// Returns `true` when the MIME type represents raw binary data.
///
/// This is a convenience wrapper around [`mime_kind`].
#[inline]
pub fn is_binary_mime(mime: &str) -> bool {
    matches!(mime_kind(mime), MimeKind::Binary)
}

/// A content reference resolved for frontend consumption.
///
/// WASM resolves binary blob refs to URLs (the browser fetches raw bytes
/// directly) and passes through inline content and text blob refs that
/// need JS-side fetching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResolvedContentRef {
    /// Ready to use — inline text content (< 1KB threshold).
    #[serde(rename_all = "camelCase")]
    Inline { inline: String },
    /// Ready to use — browser fetches raw bytes from this URL.
    /// Used for binary MIME types (images, audio, video).
    #[serde(rename_all = "camelCase")]
    Url { url: String },
    /// Needs JS-side fetch — text blob ref that WASM couldn't resolve
    /// (requires HTTP fetch to blob server for text content).
    #[serde(rename_all = "camelCase")]
    Blob { blob: String, size: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_png_is_binary() {
        assert_eq!(mime_kind("image/png"), MimeKind::Binary);
    }

    #[test]
    fn svg_is_text() {
        assert_eq!(mime_kind("image/svg+xml"), MimeKind::Text);
    }

    #[test]
    fn audio_wav_is_binary() {
        assert_eq!(mime_kind("audio/wav"), MimeKind::Binary);
    }

    #[test]
    fn video_mp4_is_binary() {
        assert_eq!(mime_kind("video/mp4"), MimeKind::Binary);
    }

    #[test]
    fn text_plain_is_text() {
        assert_eq!(mime_kind("text/plain"), MimeKind::Text);
    }

    #[test]
    fn text_html_is_text() {
        assert_eq!(mime_kind("text/html"), MimeKind::Text);
    }

    #[test]
    fn application_json_is_json() {
        assert_eq!(mime_kind("application/json"), MimeKind::Json);
    }

    #[test]
    fn plotly_json_is_json() {
        assert_eq!(mime_kind("application/vnd.plotly.v1+json"), MimeKind::Json);
    }

    #[test]
    fn vegalite_json_is_json() {
        assert_eq!(
            mime_kind("application/vnd.vegalite.v5+json"),
            MimeKind::Json
        );
    }

    #[test]
    fn application_javascript_is_text() {
        assert_eq!(mime_kind("application/javascript"), MimeKind::Text);
    }

    #[test]
    fn application_pdf_is_binary() {
        assert_eq!(mime_kind("application/pdf"), MimeKind::Binary);
    }

    #[test]
    fn geo_json_is_json() {
        assert_eq!(mime_kind("application/geo+json"), MimeKind::Json);
    }

    #[test]
    fn application_xml_is_text() {
        assert_eq!(mime_kind("application/xml"), MimeKind::Text);
    }

    #[test]
    fn application_octet_stream_is_binary() {
        assert_eq!(mime_kind("application/octet-stream"), MimeKind::Binary);
    }

    #[test]
    fn is_binary_mime_convenience() {
        assert!(is_binary_mime("image/png"));
        assert!(!is_binary_mime("text/plain"));
        assert!(!is_binary_mime("application/json"));
    }

    #[test]
    fn application_text_like_carveouts() {
        // All the text-like application/* subtypes
        assert_eq!(mime_kind("application/ecmascript"), MimeKind::Text);
        assert_eq!(mime_kind("application/xhtml+xml"), MimeKind::Text);
        assert_eq!(mime_kind("application/mathml+xml"), MimeKind::Text);
        assert_eq!(mime_kind("application/sql"), MimeKind::Text);
        assert_eq!(mime_kind("application/graphql"), MimeKind::Text);
        assert_eq!(mime_kind("application/x-latex"), MimeKind::Text);
        assert_eq!(mime_kind("application/x-tex"), MimeKind::Text);
    }

    #[test]
    fn dot_json_suffix_is_json() {
        assert_eq!(
            mime_kind("application/vnd.dataresource.json"),
            MimeKind::Json
        );
    }

    #[test]
    fn resolved_content_ref_inline_json() {
        let r = ResolvedContentRef::Inline {
            inline: "hello".to_string(),
        };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json, serde_json::json!({"inline": "hello"}));
    }

    #[test]
    fn resolved_content_ref_url_json() {
        let r = ResolvedContentRef::Url {
            url: "http://127.0.0.1:8765/blob/abc".to_string(),
        };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"url": "http://127.0.0.1:8765/blob/abc"})
        );
    }

    #[test]
    fn resolved_content_ref_blob_json() {
        let r = ResolvedContentRef::Blob {
            blob: "abc123".to_string(),
            size: 4200,
        };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json, serde_json::json!({"blob": "abc123", "size": 4200}));
    }

    #[test]
    fn resolved_content_ref_roundtrip() {
        let inline: ResolvedContentRef =
            serde_json::from_value(serde_json::json!({"inline": "hi"})).unwrap();
        assert!(matches!(inline, ResolvedContentRef::Inline { .. }));
        let url: ResolvedContentRef =
            serde_json::from_value(serde_json::json!({"url": "http://x"})).unwrap();
        assert!(matches!(url, ResolvedContentRef::Url { .. }));
        let blob: ResolvedContentRef =
            serde_json::from_value(serde_json::json!({"blob": "h", "size": 1})).unwrap();
        assert!(matches!(blob, ResolvedContentRef::Blob { .. }));
    }
}
