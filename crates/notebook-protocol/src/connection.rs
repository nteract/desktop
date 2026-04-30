//! Unified connection protocol surface for the runtimed socket.
//!
//! This module preserves the historical `notebook_protocol::connection::*` API
//! while keeping framing, handshake, and environment helpers in focused files.

mod env;
mod framing;
mod handshake;

pub use env::{EnvSource, LaunchSpec, PackageManager};

pub use framing::{
    recv_control_frame, recv_frame, recv_json_frame, recv_preamble, recv_typed_frame, send_frame,
    send_json_frame, send_preamble, send_typed_frame, send_typed_json_frame, FramedReader,
    NotebookFrameType, TypedNotebookFrame, MAGIC, MIN_PROTOCOL_VERSION, PREAMBLE_LEN,
};

pub use handshake::{
    Handshake, NotebookConnectionInfo, ProtocolCapabilities, PROTOCOL_V4, PROTOCOL_VERSION,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::framing::{
        frame_size_limits, recv_control_frame, MAX_CONTROL_FRAME_SIZE, MAX_FRAME_SIZE,
    };
    use serde::{Deserialize, Serialize};
    use std::str::FromStr;

    #[tokio::test]
    async fn test_frame_roundtrip() {
        let data = b"hello world";

        let mut buf = Vec::new();
        send_frame(&mut buf, data).await.unwrap();
        assert_eq!(buf.len(), 4 + data.len());

        let mut cursor = std::io::Cursor::new(buf);
        let received = recv_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(received, data);
    }

    #[tokio::test]
    async fn test_frame_eof() {
        let buf: &[u8] = &[];
        let mut cursor = std::io::Cursor::new(buf);
        let result = recv_frame(&mut cursor).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_frame_too_large_recv() {
        let len_bytes = (MAX_FRAME_SIZE as u32 + 1).to_be_bytes();
        let mut cursor = std::io::Cursor::new(len_bytes.to_vec());
        let result = recv_frame(&mut cursor).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_frame_too_large_send() {
        let data = vec![0u8; MAX_FRAME_SIZE + 1];
        let mut buf = Vec::new();
        let result = send_frame(&mut buf, &data).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn test_control_frame_rejects_oversized() {
        // A frame larger than 64 KiB should be rejected by recv_control_frame
        let oversized_len = (MAX_CONTROL_FRAME_SIZE as u32 + 1).to_be_bytes();
        let mut cursor = std::io::Cursor::new(oversized_len.to_vec());
        let result = recv_control_frame(&mut cursor).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_control_frame_accepts_small() {
        let data = b"small control payload";
        let mut buf = Vec::new();
        send_frame(&mut buf, data).await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let received = recv_control_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(received, data);
    }

    #[tokio::test]
    async fn test_json_frame_roundtrip() {
        let handshake = Handshake::Pool;

        let mut buf = Vec::new();
        send_json_frame(&mut buf, &handshake).await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let received: Handshake = recv_json_frame(&mut cursor).await.unwrap().unwrap();
        assert!(matches!(received, Handshake::Pool));
    }

    #[tokio::test]
    async fn test_preamble_roundtrip() {
        let mut buf = Vec::new();
        send_preamble(&mut buf).await.unwrap();
        assert_eq!(buf.len(), PREAMBLE_LEN);
        assert_eq!(&buf[..4], &MAGIC);
        assert_eq!(buf[4], PROTOCOL_VERSION as u8);

        let mut cursor = std::io::Cursor::new(buf);
        let version = recv_preamble(&mut cursor).await.unwrap();
        assert_eq!(version, PROTOCOL_VERSION as u8);
    }

    #[tokio::test]
    async fn test_preamble_bad_magic() {
        let buf = [0xFF, 0xFF, 0xFF, 0xFF, PROTOCOL_VERSION as u8];
        let mut cursor = std::io::Cursor::new(buf.to_vec());
        let result = recv_preamble(&mut cursor).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn test_preamble_version_mismatch() {
        let mut buf = [0u8; PREAMBLE_LEN];
        buf[..4].copy_from_slice(&MAGIC);
        buf[4] = 99; // wrong version
        let mut cursor = std::io::Cursor::new(buf.to_vec());
        let result = recv_preamble(&mut cursor).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn test_preamble_eof() {
        let buf: &[u8] = &[0xC0, 0xDE]; // incomplete
        let mut cursor = std::io::Cursor::new(buf);
        let result = recv_preamble(&mut cursor).await;
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }

    #[tokio::test]
    async fn test_handshake_serialization() {
        // Pool
        let json = serde_json::to_string(&Handshake::Pool).unwrap();
        assert_eq!(json, r#"{"channel":"pool"}"#);

        // SettingsSync
        let json = serde_json::to_string(&Handshake::SettingsSync).unwrap();
        assert_eq!(json, r#"{"channel":"settings_sync"}"#);

        // NotebookSync (without protocol - should omit the field)
        let json = serde_json::to_string(&Handshake::NotebookSync {
            notebook_id: "abc".into(),
            protocol: None,
            working_dir: None,
            initial_metadata: None,
        })
        .unwrap();
        assert_eq!(json, r#"{"channel":"notebook_sync","notebook_id":"abc"}"#);

        // NotebookSync with v4 protocol
        let json = serde_json::to_string(&Handshake::NotebookSync {
            notebook_id: "abc".into(),
            protocol: Some(PROTOCOL_V4.into()),
            working_dir: None,
            initial_metadata: None,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"channel":"notebook_sync","notebook_id":"abc","protocol":"v4"}"#
        );

        // NotebookSync with working_dir for untitled notebook
        let json = serde_json::to_string(&Handshake::NotebookSync {
            notebook_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            protocol: Some(PROTOCOL_V4.into()),
            working_dir: Some("/home/user/project".into()),
            initial_metadata: None,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"channel":"notebook_sync","notebook_id":"550e8400-e29b-41d4-a716-446655440000","protocol":"v4","working_dir":"/home/user/project"}"#
        );

        // Blob
        let json = serde_json::to_string(&Handshake::Blob).unwrap();
        assert_eq!(json, r#"{"channel":"blob"}"#);

        // OpenNotebook
        let json = serde_json::to_string(&Handshake::OpenNotebook {
            path: "/home/user/notebook.ipynb".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"channel":"open_notebook","path":"/home/user/notebook.ipynb"}"#
        );

        // CreateNotebook without working_dir
        let json = serde_json::to_string(&Handshake::CreateNotebook {
            runtime: "python".into(),
            working_dir: None,
            notebook_id: None,
            ephemeral: None,
            package_manager: None,
            dependencies: vec![],
        })
        .unwrap();
        assert_eq!(json, r#"{"channel":"create_notebook","runtime":"python"}"#);

        // CreateNotebook with working_dir
        let json = serde_json::to_string(&Handshake::CreateNotebook {
            runtime: "deno".into(),
            working_dir: Some("/home/user/project".into()),
            notebook_id: None,
            ephemeral: None,
            package_manager: None,
            dependencies: vec![],
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"channel":"create_notebook","runtime":"deno","working_dir":"/home/user/project"}"#
        );

        // CreateNotebook with notebook_id hint (session restore)
        let json = serde_json::to_string(&Handshake::CreateNotebook {
            runtime: "python".into(),
            working_dir: None,
            notebook_id: Some("550e8400-e29b-41d4-a716-446655440000".into()),
            ephemeral: None,
            package_manager: None,
            dependencies: vec![],
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"channel":"create_notebook","runtime":"python","notebook_id":"550e8400-e29b-41d4-a716-446655440000"}"#
        );
    }

    #[test]
    fn test_notebook_connection_info_serialization() {
        // Success case (minimal - no optional fields)
        let info = NotebookConnectionInfo {
            protocol: PROTOCOL_V4.into(),
            protocol_version: None,
            daemon_version: None,
            notebook_id: "/home/user/notebook.ipynb".into(),
            cell_count: 5,
            needs_trust_approval: false,
            error: None,
            ephemeral: false,
            notebook_path: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert_eq!(
            json,
            r#"{"protocol":"v4","notebook_id":"/home/user/notebook.ipynb","cell_count":5,"needs_trust_approval":false,"ephemeral":false}"#
        );

        // With version info
        let info = NotebookConnectionInfo {
            protocol: PROTOCOL_V4.into(),
            protocol_version: Some(PROTOCOL_VERSION),
            daemon_version: Some("0.1.0+abc123".into()),
            notebook_id: "/home/user/notebook.ipynb".into(),
            cell_count: 5,
            needs_trust_approval: false,
            error: None,
            ephemeral: false,
            notebook_path: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains(&format!(r#""protocol_version":{}"#, PROTOCOL_VERSION)));
        assert!(json.contains(r#""daemon_version":"0.1.0+abc123""#));

        // With trust approval needed
        let info = NotebookConnectionInfo {
            protocol: PROTOCOL_V4.into(),
            protocol_version: None,
            daemon_version: None,
            notebook_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            cell_count: 1,
            needs_trust_approval: true,
            error: None,
            ephemeral: false,
            notebook_path: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains(r#""needs_trust_approval":true"#));

        // Error case
        let info = NotebookConnectionInfo {
            protocol: PROTOCOL_V4.into(),
            protocol_version: None,
            daemon_version: None,
            notebook_id: String::new(),
            cell_count: 0,
            needs_trust_approval: false,
            error: Some("File not found".into()),
            ephemeral: false,
            notebook_path: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains(r#""error":"File not found""#));

        // With notebook_path
        let info = NotebookConnectionInfo {
            protocol: PROTOCOL_V4.into(),
            protocol_version: None,
            daemon_version: None,
            notebook_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            cell_count: 5,
            needs_trust_approval: false,
            error: None,
            ephemeral: false,
            notebook_path: Some("/home/user/notebook.ipynb".into()),
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains(r#""notebook_path":"/home/user/notebook.ipynb""#));

        // Backward compat: deserialize without notebook_path
        let old_json = r#"{"protocol":"v2","notebook_id":"abc","cell_count":1,"needs_trust_approval":false,"ephemeral":false}"#;
        let info: NotebookConnectionInfo = serde_json::from_str(old_json).unwrap();
        assert!(info.notebook_path.is_none());
    }

    #[tokio::test]
    async fn typed_frame_rejects_oversized_presence() {
        // Presence frames cap at 1 MiB. A desync that happens to land
        // on the Presence channel with a multi-MiB length header is
        // caught here instead of trying to allocate it.
        let cap = frame_size_limits(notebook_wire::frame_types::PRESENCE).cap;
        let body_len: u32 = (cap as u32) + 1;
        let total_len: u32 = body_len + 1;
        let mut buf = Vec::new();
        buf.extend_from_slice(&total_len.to_be_bytes());
        buf.push(notebook_wire::frame_types::PRESENCE);
        let mut cursor = std::io::Cursor::new(buf);
        let err = recv_typed_frame(&mut cursor).await.unwrap_err();
        assert!(err.to_string().contains("too large for type 0x04"));
    }

    #[tokio::test]
    async fn typed_frame_allows_big_broadcast_for_widget_comm() {
        // `NotebookBroadcast::Comm` carries widget envelopes with inline
        // binary buffers. The Broadcast cap (16 MiB) leaves room for
        // legitimate widget messages while still being far below the
        // outer ceiling.
        let big_payload = vec![0x42u8; 2 * 1024 * 1024]; // 2 MiB
        let mut buf = Vec::new();
        send_typed_frame(&mut buf, NotebookFrameType::Broadcast, &big_payload)
            .await
            .unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let frame = recv_typed_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(frame.frame_type, NotebookFrameType::Broadcast);
        assert_eq!(frame.payload.len(), big_payload.len());
    }

    #[tokio::test]
    async fn typed_frame_rejects_oversized_request() {
        // The Request cap rejects payloads that exceed the channel's
        // legitimate worst case (today: a SendComm envelope with widget
        // buffers that JSON-expand from binary).
        let cap = frame_size_limits(notebook_wire::frame_types::REQUEST).cap;
        let body_len: u32 = (cap as u32) + 1;
        let total_len: u32 = body_len + 1;
        let mut buf = Vec::new();
        buf.extend_from_slice(&total_len.to_be_bytes());
        buf.push(notebook_wire::frame_types::REQUEST);
        let mut cursor = std::io::Cursor::new(buf);
        let err = recv_typed_frame(&mut cursor).await.unwrap_err();
        assert!(err.to_string().contains("too large for type 0x01"));
    }

    #[tokio::test]
    async fn typed_frame_allows_sendcomm_with_widget_buffers() {
        // Custom comm messages from `model.send(content, callbacks, buffers)`
        // ride `NotebookRequest::SendComm`. JSON-encoding `Vec<Vec<u8>>`
        // expands binary by ~4×, so a 256 KiB widget buffer becomes
        // ~1 MiB on the wire. The Request cap must accommodate this.
        // 4 MiB simulates a buffer roughly equivalent to a 1 MiB binary
        // payload after JSON expansion — a realistic moderate-size
        // custom widget message.
        let big_payload = vec![0x42u8; 4 * 1024 * 1024];
        let mut buf = Vec::new();
        send_typed_frame(&mut buf, NotebookFrameType::Request, &big_payload)
            .await
            .unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let frame = recv_typed_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(frame.frame_type, NotebookFrameType::Request);
        assert_eq!(frame.payload.len(), big_payload.len());
    }

    #[tokio::test]
    async fn typed_frame_send_rejects_outbound_oversize() {
        // The send path mirrors the receive cap so an outbound oversize
        // surfaces as a clear local error rather than as a generic peer
        // rejection.
        let cap = frame_size_limits(notebook_wire::frame_types::REQUEST).cap;
        let oversized = vec![0u8; cap + 1];
        let mut buf = Vec::new();
        let err = send_typed_frame(&mut buf, NotebookFrameType::Request, &oversized)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("outbound frame too large"));
        // No bytes should have been written for an over-cap frame.
        assert!(
            buf.is_empty(),
            "frame body must not be written when over cap"
        );
    }

    #[test]
    fn frame_size_limits_cover_every_known_frame_type() {
        // Pin the per-type cap table so a new frame type can't slip in
        // without an explicit limit decision. Compares against the
        // outer ceiling so the test fails when an unknown type ends up
        // on the 100 MiB fallback.
        use notebook_wire::frame_types as ft;
        for &ty in &[
            ft::AUTOMERGE_SYNC,
            ft::REQUEST,
            ft::RESPONSE,
            ft::BROADCAST,
            ft::PRESENCE,
            ft::RUNTIME_STATE_SYNC,
            ft::POOL_STATE_SYNC,
            ft::SESSION_CONTROL,
        ] {
            let limits = frame_size_limits(ty);
            assert!(
                limits.cap < MAX_FRAME_SIZE,
                "type 0x{ty:02x} has no tighter cap than the outer ceiling",
            );
            assert!(
                limits.warn < limits.cap,
                "type 0x{ty:02x} warn ({}) >= cap ({})",
                limits.warn,
                limits.cap,
            );
            assert!(limits.warn > 0, "type 0x{ty:02x} warn must be > 0");
        }
    }

    #[tokio::test]
    async fn typed_frame_allows_big_automerge_sync() {
        let big_payload = vec![0x42u8; 2 * 1024 * 1024]; // 2 MiB
        let mut buf = Vec::new();
        send_typed_frame(&mut buf, NotebookFrameType::AutomergeSync, &big_payload)
            .await
            .unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let frame = recv_typed_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(frame.frame_type, NotebookFrameType::AutomergeSync);
        assert_eq!(frame.payload.len(), big_payload.len());
    }

    #[tokio::test]
    async fn typed_frame_rejects_1819243560_byte_length() {
        // The specific desync value observed in the field: 0x6C6F6761
        // ("loga"). Interpreted as a u32 big-endian length this is
        // 1,819,243,560 bytes — well above the 100 MiB outer cap.
        // This must be rejected at the outer check before we ever read
        // the type byte.
        let loga_bytes: [u8; 4] = [0x6C, 0x6F, 0x67, 0x61];
        let mut cursor = std::io::Cursor::new(loga_bytes.to_vec());
        let err = recv_typed_frame(&mut cursor).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("frame too large"), "unexpected error: {msg}");
    }

    #[tokio::test]
    async fn test_multiple_frames_on_same_stream() {
        let mut buf = Vec::new();
        send_frame(&mut buf, b"first").await.unwrap();
        send_frame(&mut buf, b"second").await.unwrap();
        send_frame(&mut buf, b"third").await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        assert_eq!(recv_frame(&mut cursor).await.unwrap().unwrap(), b"first");
        assert_eq!(recv_frame(&mut cursor).await.unwrap().unwrap(), b"second");
        assert_eq!(recv_frame(&mut cursor).await.unwrap().unwrap(), b"third");
        // EOF
        assert!(recv_frame(&mut cursor).await.unwrap().is_none());
    }

    #[test]
    fn test_notebook_frame_type_conversion() {
        assert_eq!(
            NotebookFrameType::try_from(0x00).unwrap(),
            NotebookFrameType::AutomergeSync
        );
        assert_eq!(
            NotebookFrameType::try_from(0x01).unwrap(),
            NotebookFrameType::Request
        );
        assert_eq!(
            NotebookFrameType::try_from(0x02).unwrap(),
            NotebookFrameType::Response
        );
        assert_eq!(
            NotebookFrameType::try_from(0x03).unwrap(),
            NotebookFrameType::Broadcast
        );
        assert!(NotebookFrameType::try_from(0xFF).is_err());
    }

    #[tokio::test]
    async fn test_typed_frame_roundtrip() {
        let payload = b"test payload";

        let mut buf = Vec::new();
        send_typed_frame(&mut buf, NotebookFrameType::Request, payload)
            .await
            .unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let frame = recv_typed_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(frame.frame_type, NotebookFrameType::Request);
        assert_eq!(frame.payload, payload);
    }

    #[tokio::test]
    async fn test_typed_frame_automerge_sync() {
        let sync_data = b"\x00binary automerge data";

        let mut buf = Vec::new();
        send_typed_frame(&mut buf, NotebookFrameType::AutomergeSync, sync_data)
            .await
            .unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let frame = recv_typed_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(frame.frame_type, NotebookFrameType::AutomergeSync);
        assert_eq!(frame.payload, sync_data);
    }

    #[tokio::test]
    async fn test_typed_json_frame() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct TestMsg {
            value: i32,
        }

        let msg = TestMsg { value: 42 };

        let mut buf = Vec::new();
        send_typed_json_frame(&mut buf, NotebookFrameType::Request, &msg)
            .await
            .unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let frame = recv_typed_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(frame.frame_type, NotebookFrameType::Request);

        let parsed: TestMsg = serde_json::from_slice(&frame.payload).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn package_manager_as_str_round_trips() {
        assert_eq!(PackageManager::Uv.as_str(), "uv");
        assert_eq!(PackageManager::Conda.as_str(), "conda");
        assert_eq!(PackageManager::Pixi.as_str(), "pixi");
    }

    #[test]
    fn package_manager_parse_valid() {
        assert_eq!(PackageManager::parse("uv").unwrap(), PackageManager::Uv);
        assert_eq!(
            PackageManager::parse("conda").unwrap(),
            PackageManager::Conda
        );
        assert_eq!(PackageManager::parse("pixi").unwrap(), PackageManager::Pixi);
    }

    #[test]
    fn package_manager_parse_aliases() {
        assert_eq!(PackageManager::parse("pip").unwrap(), PackageManager::Uv);
        assert_eq!(
            PackageManager::parse("mamba").unwrap(),
            PackageManager::Conda
        );
    }

    #[test]
    fn package_manager_parse_rejects_unknown() {
        let err = PackageManager::parse("npm").unwrap_err();
        assert!(err.contains("Unsupported package manager 'npm'"));
        assert!(err.contains("Supported: uv, conda, pixi"));
    }

    #[test]
    fn package_manager_fromstr_works() {
        let pm: PackageManager = PackageManager::from_str("conda").unwrap();
        assert_eq!(pm, PackageManager::Conda);
        assert!(PackageManager::from_str("bogus").is_err());
    }

    #[test]
    fn package_manager_display_matches_as_str() {
        assert_eq!(format!("{}", PackageManager::Uv), "uv");
        assert_eq!(format!("{}", PackageManager::Conda), "conda");
        assert_eq!(format!("{}", PackageManager::Pixi), "pixi");
    }

    #[test]
    fn package_manager_serde_is_lowercase() {
        let json = serde_json::to_string(&PackageManager::Conda).unwrap();
        assert_eq!(json, "\"conda\"");
        let pm: PackageManager = serde_json::from_str("\"pixi\"").unwrap();
        assert_eq!(pm, PackageManager::Pixi);
    }

    #[test]
    fn package_manager_deserialize_captures_unknown() {
        // Aliases must decode (wire compatibility for legacy clients).
        let pm: PackageManager = serde_json::from_str("\"pip\"").unwrap();
        assert_eq!(pm, PackageManager::Unknown("pip".to_string()));
        let pm: PackageManager = serde_json::from_str("\"mamba\"").unwrap();
        assert_eq!(pm, PackageManager::Unknown("mamba".to_string()));
        // Genuinely unknown values decode to Unknown, not an error.
        let pm: PackageManager = serde_json::from_str("\"poetry\"").unwrap();
        assert_eq!(pm, PackageManager::Unknown("poetry".to_string()));
    }

    #[test]
    fn package_manager_unknown_round_trips_verbatim() {
        let pm = PackageManager::Unknown("mamba".to_string());
        let json = serde_json::to_string(&pm).unwrap();
        assert_eq!(json, "\"mamba\"");
        let decoded: PackageManager = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, pm);
    }

    #[test]
    fn package_manager_resolve_folds_aliases() {
        assert_eq!(PackageManager::Uv.resolve().unwrap(), PackageManager::Uv);
        assert_eq!(
            PackageManager::Unknown("pip".to_string())
                .resolve()
                .unwrap(),
            PackageManager::Uv
        );
        assert_eq!(
            PackageManager::Unknown("mamba".to_string())
                .resolve()
                .unwrap(),
            PackageManager::Conda
        );
        assert!(PackageManager::Unknown("poetry".to_string())
            .resolve()
            .is_err());
    }

    // -----------------------------------------------------------
    // EnvSource tests
    // -----------------------------------------------------------

    #[test]
    fn env_source_as_str_round_trips_all_variants() {
        assert_eq!(
            EnvSource::Prewarmed(PackageManager::Uv).as_str(),
            "uv:prewarmed"
        );
        assert_eq!(
            EnvSource::Prewarmed(PackageManager::Conda).as_str(),
            "conda:prewarmed"
        );
        assert_eq!(
            EnvSource::Prewarmed(PackageManager::Pixi).as_str(),
            "pixi:prewarmed"
        );
        assert_eq!(EnvSource::Inline(PackageManager::Uv).as_str(), "uv:inline");
        assert_eq!(
            EnvSource::Inline(PackageManager::Conda).as_str(),
            "conda:inline"
        );
        assert_eq!(
            EnvSource::Inline(PackageManager::Pixi).as_str(),
            "pixi:inline"
        );
        assert_eq!(EnvSource::Pyproject.as_str(), "uv:pyproject");
        assert_eq!(EnvSource::PixiToml.as_str(), "pixi:toml");
        assert_eq!(EnvSource::EnvYml.as_str(), "conda:env_yml");
        assert_eq!(EnvSource::Pep723(PackageManager::Uv).as_str(), "uv:pep723");
        assert_eq!(
            EnvSource::Pep723(PackageManager::Pixi).as_str(),
            "pixi:pep723"
        );
        assert_eq!(EnvSource::Deno.as_str(), "deno");
    }

    #[test]
    fn env_source_parse_valid_round_trips() {
        for s in [
            "uv:prewarmed",
            "conda:prewarmed",
            "pixi:prewarmed",
            "uv:inline",
            "conda:inline",
            "pixi:inline",
            "uv:pyproject",
            "pixi:toml",
            "conda:env_yml",
            "uv:pep723",
            "pixi:pep723",
            "deno",
        ] {
            let parsed = EnvSource::parse(s);
            assert_eq!(parsed.as_str(), s, "round-trip failed for {s}");
            assert!(!matches!(parsed, EnvSource::Unknown(_)));
        }
    }

    #[test]
    fn env_source_parse_unknown_captures_string() {
        let pm = EnvSource::parse("weird:future-variant");
        assert_eq!(pm, EnvSource::Unknown("weird:future-variant".to_string()));
        assert_eq!(pm.as_str(), "weird:future-variant");
    }

    #[test]
    fn env_source_parse_empty_is_unknown() {
        assert_eq!(EnvSource::parse(""), EnvSource::Unknown(String::new()));
    }

    #[test]
    fn env_source_serde_is_string() {
        let src = EnvSource::Inline(PackageManager::Conda);
        let json = serde_json::to_string(&src).unwrap();
        assert_eq!(json, "\"conda:inline\"");
        let decoded: EnvSource = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, src);
    }

    #[test]
    fn env_source_serde_unknown_round_trips_verbatim() {
        let src = EnvSource::Unknown("something:new".to_string());
        let json = serde_json::to_string(&src).unwrap();
        assert_eq!(json, "\"something:new\"");
        let decoded: EnvSource = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, src);
    }

    #[test]
    fn env_source_conda_pep723_round_trips() {
        let src = EnvSource::Pep723(PackageManager::Conda);
        assert_eq!(src.as_str(), "conda:pep723");
        assert_eq!(EnvSource::parse("conda:pep723"), src);
    }

    #[test]
    fn env_source_unknown_prefix_preserves_family() {
        // Forward-compat: newer peers may send env_source strings we haven't
        // taught the enum about. Family classification must still route
        // correctly to the originating package manager's pool / helpers.
        assert_eq!(
            EnvSource::parse("conda:foo").package_manager(),
            Some(PackageManager::Conda)
        );
        assert_eq!(
            EnvSource::parse("uv:new-source").package_manager(),
            Some(PackageManager::Uv)
        );
        assert_eq!(
            EnvSource::parse("pixi:bar").package_manager(),
            Some(PackageManager::Pixi)
        );
        // No recognized prefix → no manager.
        assert_eq!(EnvSource::parse("mystery").package_manager(), None);
    }

    #[test]
    fn env_source_package_manager_for_all() {
        assert_eq!(
            EnvSource::Prewarmed(PackageManager::Uv).package_manager(),
            Some(PackageManager::Uv)
        );
        assert_eq!(
            EnvSource::Inline(PackageManager::Conda).package_manager(),
            Some(PackageManager::Conda)
        );
        assert_eq!(
            EnvSource::Pyproject.package_manager(),
            Some(PackageManager::Uv)
        );
        assert_eq!(
            EnvSource::PixiToml.package_manager(),
            Some(PackageManager::Pixi)
        );
        assert_eq!(
            EnvSource::EnvYml.package_manager(),
            Some(PackageManager::Conda)
        );
        assert_eq!(
            EnvSource::Pep723(PackageManager::Pixi).package_manager(),
            Some(PackageManager::Pixi)
        );
        assert_eq!(EnvSource::Deno.package_manager(), None);
        assert_eq!(EnvSource::Unknown("junk".into()).package_manager(), None);
    }

    #[test]
    fn env_source_prepares_own_env() {
        // Inline / project-file / pep723 sources prepare their own env.
        assert!(EnvSource::Inline(PackageManager::Uv).prepares_own_env());
        assert!(EnvSource::Inline(PackageManager::Conda).prepares_own_env());
        assert!(EnvSource::Inline(PackageManager::Pixi).prepares_own_env());
        assert!(EnvSource::Pyproject.prepares_own_env());
        assert!(EnvSource::PixiToml.prepares_own_env());
        assert!(EnvSource::EnvYml.prepares_own_env());
        assert!(EnvSource::Pep723(PackageManager::Uv).prepares_own_env());
        assert!(EnvSource::Pep723(PackageManager::Pixi).prepares_own_env());

        // Prewarmed variants do not prepare their own env.
        assert!(!EnvSource::Prewarmed(PackageManager::Uv).prepares_own_env());
        assert!(!EnvSource::Prewarmed(PackageManager::Conda).prepares_own_env());
        assert!(!EnvSource::Prewarmed(PackageManager::Pixi).prepares_own_env());

        // Deno and Unknown take the "no pool" path.
        assert!(!EnvSource::Deno.prepares_own_env());
        assert!(!EnvSource::Unknown("nope".into()).prepares_own_env());
    }

    // -----------------------------------------------------------
    // LaunchSpec tests
    // -----------------------------------------------------------

    #[test]
    fn launch_spec_parse_auto_variants() {
        assert_eq!(LaunchSpec::parse(""), LaunchSpec::Auto);
        assert_eq!(LaunchSpec::parse("auto"), LaunchSpec::Auto);
        assert_eq!(LaunchSpec::parse("prewarmed"), LaunchSpec::Auto);
        assert_eq!(
            LaunchSpec::parse("auto:uv"),
            LaunchSpec::AutoScoped(PackageManager::Uv)
        );
        assert_eq!(
            LaunchSpec::parse("auto:conda"),
            LaunchSpec::AutoScoped(PackageManager::Conda)
        );
        assert_eq!(
            LaunchSpec::parse("auto:pixi"),
            LaunchSpec::AutoScoped(PackageManager::Pixi)
        );
    }

    #[test]
    fn launch_spec_parse_concrete_delegates_to_env_source() {
        assert_eq!(
            LaunchSpec::parse("uv:inline"),
            LaunchSpec::Concrete(EnvSource::Inline(PackageManager::Uv))
        );
        assert_eq!(
            LaunchSpec::parse("deno"),
            LaunchSpec::Concrete(EnvSource::Deno)
        );
    }

    #[test]
    fn launch_spec_parse_future_value_is_concrete_unknown() {
        assert_eq!(
            LaunchSpec::parse("something:new"),
            LaunchSpec::Concrete(EnvSource::Unknown("something:new".to_string()))
        );
    }

    #[test]
    fn launch_spec_auto_scope_returns_manager() {
        assert_eq!(LaunchSpec::Auto.auto_scope(), None);
        assert_eq!(
            LaunchSpec::AutoScoped(PackageManager::Conda).auto_scope(),
            Some(PackageManager::Conda)
        );
        assert_eq!(LaunchSpec::Concrete(EnvSource::Deno).auto_scope(), None);
    }

    #[test]
    fn launch_spec_serde_is_string() {
        assert_eq!(
            serde_json::to_value(LaunchSpec::AutoScoped(PackageManager::Pixi)).unwrap(),
            serde_json::json!("auto:pixi")
        );
        assert_eq!(
            serde_json::to_value(LaunchSpec::Concrete(EnvSource::Inline(PackageManager::Uv)))
                .unwrap(),
            serde_json::json!("uv:inline")
        );

        let auto: LaunchSpec = serde_json::from_value(serde_json::json!("auto")).unwrap();
        assert_eq!(auto, LaunchSpec::Auto);
        let concrete: LaunchSpec =
            serde_json::from_value(serde_json::json!("conda:inline")).unwrap();
        assert_eq!(
            concrete,
            LaunchSpec::Concrete(EnvSource::Inline(PackageManager::Conda))
        );
    }

    /// `recv_typed_frame` is built on `read_exact`, which is NOT cancel-
    /// safe: dropping the future mid-read silently discards bytes
    /// already pulled off the underlying reader.
    ///
    /// This test exercises the exact misuse pattern that desynced the
    /// runtime-agent ↔ daemon channel under heavy stream output: a peer
    /// loop puts `recv_typed_frame` in a `tokio::select!` arm next to a
    /// high-frequency cancel-safe arm. When the cancel-safe arm wins
    /// while a body read is in flight, the next iteration's
    /// `recv_typed_frame` reads its 4-byte length prefix from the middle
    /// of the previous payload and flags `frame too large` (production
    /// repros saw 0x20202020 from indented kernel stdout and 0x6C6C6F6E
    /// from "...loaded kernel_..." text).
    ///
    /// `FramedReader` is the structural fix: a dedicated reader task
    /// owns the read half and publishes frames through an mpsc, whose
    /// `recv()` is cancel-safe.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn framed_reader_does_not_desync_in_select_under_pressure() {
        use tokio::io::duplex;
        use tokio::sync::mpsc;
        use tokio::time::{timeout, Duration};

        // Tiny duplex buffer forces multi-poll reads, mirroring the real
        // socket pressure that triggered the production desync.
        let (server_side, client_side) = duplex(64);
        let (_server_read, mut writer) = tokio::io::split(server_side);
        let (reader, _client_write) = tokio::io::split(client_side);

        const NUM_FRAMES: usize = 32;
        const PAYLOAD_SIZE: usize = 4096;

        let writer_task = tokio::spawn(async move {
            for i in 0u64..NUM_FRAMES as u64 {
                let mut payload = vec![0u8; PAYLOAD_SIZE];
                payload[..8].copy_from_slice(&i.to_be_bytes());
                for (j, b) in payload[8..].iter_mut().enumerate() {
                    *b = (j & 0xFF) as u8;
                }
                send_typed_frame(&mut writer, NotebookFrameType::AutomergeSync, &payload)
                    .await
                    .expect("writer should not fail");
            }
        });

        // Always-ready cancel-safe interrupter, simulating the real
        // daemon pressure (state_changed_rx fires per kernel output).
        let (interrupter_tx, mut interrupter_rx) = mpsc::unbounded_channel::<()>();
        let pump_token = interrupter_tx.clone();
        let pump_task = tokio::spawn(async move {
            loop {
                if pump_token.send(()).is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        });

        let mut framed = FramedReader::spawn(reader, 16);

        let mut received = 0u64;
        let receive_loop = async {
            while received < NUM_FRAMES as u64 {
                tokio::select! {
                    biased;
                    Some(_) = interrupter_rx.recv() => {
                        while interrupter_rx.try_recv().is_ok() {}
                    }
                    maybe = framed.recv() => {
                        let frame = maybe
                            .expect("channel should not close before NUM_FRAMES delivered")
                            .expect("frame decode should not error mid-stream");
                        assert_eq!(frame.frame_type, NotebookFrameType::AutomergeSync);
                        assert_eq!(frame.payload.len(), PAYLOAD_SIZE);
                        let seq = u64::from_be_bytes(frame.payload[..8].try_into().unwrap());
                        assert_eq!(
                            seq, received,
                            "frame sequence desynced (expected {}, got {})",
                            received, seq,
                        );
                        for (j, b) in frame.payload[8..].iter().enumerate() {
                            assert_eq!(
                                *b,
                                (j & 0xFF) as u8,
                                "payload corruption at offset {} of frame {}",
                                j + 8,
                                received,
                            );
                        }
                        received += 1;
                    }
                }
            }
        };

        timeout(Duration::from_secs(10), receive_loop)
            .await
            .expect("receive loop should complete within 10s");

        pump_task.abort();
        let _ = pump_task.await;
        writer_task.await.expect("writer task panicked");
        assert_eq!(received, NUM_FRAMES as u64);
    }
}
