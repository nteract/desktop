use super::*;
use serial_test::serial;
use uuid::Uuid;

#[test]
fn fallback_output_stamps_id_when_missing() {
    let raw = serde_json::json!({
        "output_type": "stream",
        "name": "stdout",
        "text": "hi\n",
    });
    let out = fallback_output_with_id(&raw);
    let id = out
        .get("output_id")
        .and_then(|v| v.as_str())
        .expect("output_id set");
    assert!(!id.is_empty(), "fallback must stamp a non-empty id");
    // Rest of the payload passes through untouched.
    assert_eq!(out["output_type"], "stream");
    assert_eq!(out["name"], "stdout");
}

#[test]
fn fallback_output_preserves_existing_id() {
    let raw = serde_json::json!({
        "output_type": "stream",
        "output_id": "existing-id",
    });
    let out = fallback_output_with_id(&raw);
    assert_eq!(out["output_id"], "existing-id");
}

#[test]
fn fallback_output_replaces_empty_id() {
    let raw = serde_json::json!({
        "output_type": "stream",
        "output_id": "",
    });
    let out = fallback_output_with_id(&raw);
    let id = out
        .get("output_id")
        .and_then(|v| v.as_str())
        .expect("output_id set");
    assert!(!id.is_empty());
    assert_ne!(id, "");
}

#[test]
fn test_sanitize_peer_label_basic() {
    assert_eq!(sanitize_peer_label(None, "fb"), "fb");
    assert_eq!(sanitize_peer_label(Some(""), "fb"), "fb");
    assert_eq!(sanitize_peer_label(Some("  "), "fb"), "fb");
    assert_eq!(sanitize_peer_label(Some("Codex"), "fb"), "Codex");
    assert_eq!(sanitize_peer_label(Some("  Claude  "), "fb"), "Claude");
}

#[test]
fn test_sanitize_peer_label_clamps_length() {
    let long = "a".repeat(100);
    assert_eq!(sanitize_peer_label(Some(&long), "fb").len(), 64);
}

#[test]
fn test_sanitize_peer_label_clamps_unicode() {
    // 70 emoji = 70 chars but 280 bytes
    let emoji_label: String = "🦾".repeat(70);
    let result = sanitize_peer_label(Some(&emoji_label), "fb");
    assert_eq!(result.chars().count(), 64);
}

#[test]
fn test_sanitize_peer_label_strips_zero_width() {
    // ZWJ, ZWSP, ZWNJ scattered in a label
    assert_eq!(
        sanitize_peer_label(Some("Co\u{200B}d\u{200D}ex"), "fb"),
        "Codex"
    );
    // Only zero-width chars → falls back to fallback
    assert_eq!(
        sanitize_peer_label(Some("\u{200B}\u{200C}\u{200D}"), "fb"),
        "fb"
    );
}

#[test]
fn test_sanitize_peer_label_strips_control_chars() {
    assert_eq!(sanitize_peer_label(Some("Claude\x00\x1F"), "fb"), "Claude");
    assert_eq!(sanitize_peer_label(Some("\x07"), "fb"), "fb");
}

#[test]
fn test_sanitize_peer_label_strips_bidi_overrides() {
    // RTL override + LTR override
    assert_eq!(
        sanitize_peer_label(Some("\u{202E}Agent\u{202C}"), "fb"),
        "Agent"
    );
}

#[test]
fn test_sanitize_peer_label_strips_bidi_marks() {
    // LRM and RLM
    assert_eq!(
        sanitize_peer_label(Some("\u{200E}Agent\u{200F}"), "fb"),
        "Agent"
    );
    assert_eq!(sanitize_peer_label(Some("\u{200E}\u{200F}"), "fb"), "fb");
}

/// Create a test blob store in the given temp directory.
fn test_blob_store(tmp: &tempfile::TempDir) -> Arc<BlobStore> {
    Arc::new(BlobStore::new(tmp.path().join("blobs")))
}

#[tokio::test]
async fn notebook_room_has_uuid_id_populated() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);
    let uuid = uuid::Uuid::new_v4();
    let room = NotebookRoom::new_fresh(
        uuid,
        None, // untitled
        tmp.path(),
        blob_store,
        false, // ephemeral
    );
    assert_eq!(room.id, uuid);
}

#[tokio::test]
async fn untitled_room_has_path_none() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);
    let room = NotebookRoom::new_fresh(Uuid::new_v4(), None, tmp.path(), blob_store, false);
    assert!(room.path.read().await.is_none());
}

#[tokio::test]
async fn file_backed_room_has_path_some() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);
    let fake_path = tmp.path().join("note.ipynb");
    let room = NotebookRoom::new_fresh(
        Uuid::new_v4(),
        Some(fake_path.clone()),
        tmp.path(),
        blob_store,
        false,
    );
    let guard = room.path.read().await;
    assert_eq!(guard.as_deref(), Some(fake_path.as_path()));
}

#[tokio::test]
async fn test_room_load_or_create_new() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);
    let room = NotebookRoom::load_or_create("test-nb", tmp.path(), blob_store);

    let doc = room.doc.try_read().unwrap();
    assert_eq!(doc.notebook_id(), Some("test-nb".to_string()));
    assert_eq!(doc.cell_count(), 0);
    assert_eq!(room.active_peers.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn test_room_persists_and_reloads() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);

    // Create room and add a cell
    {
        let room = NotebookRoom::load_or_create("persist-test", tmp.path(), blob_store.clone());
        let mut doc = room.doc.try_write().unwrap();
        doc.add_cell(0, "c1", "code").unwrap();
        doc.update_source("c1", "hello").unwrap();
        let bytes = doc.save();
        persist_notebook_bytes(&bytes, &room.persist_path);
    }

    // Load again — should have the cell
    {
        let room = NotebookRoom::load_or_create("persist-test", tmp.path(), blob_store);
        let doc = room.doc.try_read().unwrap();
        assert_eq!(doc.cell_count(), 1);
        let cell = doc.get_cell("c1").unwrap();
        assert_eq!(cell.source, "hello");
    }
}

#[tokio::test]
async fn test_get_or_create_room_reuses_existing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);
    let rooms: NotebookRooms = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let path_index = Arc::new(tokio::sync::Mutex::new(PathIndex::new()));
    let uuid1 = Uuid::new_v4();

    let room1 = get_or_create_room(
        &rooms,
        &path_index,
        uuid1,
        None,
        tmp.path(),
        blob_store.clone(),
        false,
    )
    .await;
    let room2 = get_or_create_room(
        &rooms,
        &path_index,
        uuid1,
        None,
        tmp.path(),
        blob_store,
        false,
    )
    .await;

    // Should be the same Arc (same room)
    assert!(Arc::ptr_eq(&room1, &room2));
}

#[tokio::test]
async fn test_get_or_create_room_different_notebooks() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);
    let rooms: NotebookRooms = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let path_index = Arc::new(tokio::sync::Mutex::new(PathIndex::new()));
    let uuid1 = Uuid::new_v4();
    let uuid2 = Uuid::new_v4();

    let room1 = get_or_create_room(
        &rooms,
        &path_index,
        uuid1,
        None,
        tmp.path(),
        blob_store.clone(),
        false,
    )
    .await;
    let room2 = get_or_create_room(
        &rooms,
        &path_index,
        uuid2,
        None,
        tmp.path(),
        blob_store,
        false,
    )
    .await;

    // Should be different rooms
    assert!(!Arc::ptr_eq(&room1, &room2));
    assert_eq!(rooms.lock().await.len(), 2);
}

#[tokio::test]
async fn test_room_peer_counting() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);
    let room = NotebookRoom::load_or_create("peer-test", tmp.path(), blob_store);

    assert_eq!(room.active_peers.load(Ordering::Relaxed), 0);

    room.active_peers.fetch_add(1, Ordering::Relaxed);
    room.active_peers.fetch_add(1, Ordering::Relaxed);
    assert_eq!(room.active_peers.load(Ordering::Relaxed), 2);

    room.active_peers.fetch_sub(1, Ordering::Relaxed);
    assert_eq!(room.active_peers.load(Ordering::Relaxed), 1);

    room.active_peers.fetch_sub(1, Ordering::Relaxed);
    assert_eq!(room.active_peers.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn test_new_fresh_creates_empty_doc() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);
    let uuid = Uuid::new_v4();
    let room = NotebookRoom::new_fresh(uuid, None, tmp.path(), blob_store, false);

    let doc = room.doc.try_read().unwrap();
    assert_eq!(doc.notebook_id(), Some(uuid.to_string()));
    assert_eq!(doc.cell_count(), 0);
}

#[tokio::test]
async fn test_new_fresh_deletes_stale_persisted_doc_for_file_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);

    // Use a fixed UUID so we can find the persist file again.
    let uuid = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-111111111111").unwrap();

    // Create and persist a room with content using load_or_create (uses the UUID string)
    {
        let room = NotebookRoom::load_or_create(&uuid.to_string(), tmp.path(), blob_store.clone());
        let mut doc = room.doc.try_write().unwrap();
        doc.add_cell(0, "c1", "code").unwrap();
        doc.update_source("c1", "old content").unwrap();
        let bytes = doc.save();
        persist_notebook_bytes(&bytes, &room.persist_path);
    }

    // Verify persisted file exists
    let filename = notebook_doc_filename(&uuid.to_string());
    let persist_path = tmp.path().join(&filename);
    assert!(persist_path.exists(), "Persisted file should exist");

    // Create fresh room for a file-backed path — should delete persisted doc and start empty.
    // path=Some means this is file-backed, so the persisted .automerge doc should be deleted.
    let fake_ipynb = tmp.path().join("stale-test.ipynb");
    let room = NotebookRoom::new_fresh(uuid, Some(fake_ipynb), tmp.path(), blob_store, false);

    // Persisted file should be deleted
    assert!(
        !persist_path.exists(),
        "Persisted file should be deleted by new_fresh"
    );

    // Room should be empty (no cells from persisted doc)
    let doc = room.doc.try_read().unwrap();
    assert_eq!(doc.cell_count(), 0, "new_fresh should start with empty doc");
}

#[tokio::test]
async fn test_new_fresh_loads_persisted_doc_for_untitled_notebook() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);

    // Use a fixed UUID (untitled notebook — path=None)
    let uuid = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();

    // Create and persist a room with content using load_or_create
    {
        let room = NotebookRoom::load_or_create(&uuid.to_string(), tmp.path(), blob_store.clone());
        let mut doc = room.doc.try_write().unwrap();
        doc.add_cell(0, "c1", "code").unwrap();
        doc.update_source("c1", "restored content").unwrap();
        let bytes = doc.save();
        persist_notebook_bytes(&bytes, &room.persist_path);
    }

    // Verify persisted file exists
    let filename = notebook_doc_filename(&uuid.to_string());
    let persist_path = tmp.path().join(&filename);
    assert!(persist_path.exists(), "Persisted file should exist");

    // Create fresh room for untitled notebook (path=None) — should load persisted doc
    let room = NotebookRoom::new_fresh(uuid, None, tmp.path(), blob_store, false);

    // Persisted file should still exist (not deleted)
    assert!(
        persist_path.exists(),
        "Persisted file should NOT be deleted for untitled notebooks"
    );

    // Room should have the persisted content
    let doc = room.doc.try_read().unwrap();
    assert_eq!(
        doc.cell_count(),
        1,
        "new_fresh should load persisted doc for untitled notebooks"
    );
    let cells = doc.get_cells();
    assert_eq!(cells[0].source, "restored content");
}

/// Regression test for #1646: untitled notebooks must read trust from
/// the persisted Automerge doc, not from a non-existent .ipynb file.
#[tokio::test]
#[serial]
async fn test_new_fresh_untitled_trust_from_doc() {
    let temp_dir = tempfile::tempdir().unwrap();
    let key_path = temp_dir.path().join("trust-key");
    std::env::set_var("RUNT_TRUST_KEY_PATH", key_path.to_str().unwrap());

    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);

    let notebook_id = "550e8400-e29b-41d4-a716-446655440000";

    // Build a snapshot with UV deps and a valid trust signature.
    let mut snapshot = snapshot_with_uv(vec!["numpy".to_string()]);
    let mut metadata = std::collections::HashMap::new();
    if let Ok(runt_value) = serde_json::to_value(&snapshot.runt) {
        metadata.insert("runt".to_string(), runt_value);
    }
    let signature = runt_trust::sign_notebook_dependencies(&metadata).unwrap();
    snapshot.runt.trust_signature = Some(signature);

    // Create a room, write the signed metadata, and persist to disk.
    {
        let room = NotebookRoom::load_or_create(notebook_id, tmp.path(), blob_store.clone());
        {
            let mut doc = room.doc.try_write().unwrap();
            doc.set_metadata_snapshot(&snapshot).unwrap();
            let bytes = doc.save();
            persist_notebook_bytes(&bytes, &room.persist_path);
        }
    }

    // Simulate daemon restart: create a fresh room with the same UUID.
    // new_fresh should load the persisted doc and read trust from it.
    let notebook_uuid = Uuid::parse_str(notebook_id).unwrap();
    let room = NotebookRoom::new_fresh(notebook_uuid, None, tmp.path(), blob_store, false);

    let ts = room.trust_state.try_read().unwrap();
    assert_eq!(
        ts.status,
        runt_trust::TrustStatus::Trusted,
        "Untitled notebook trust should survive daemon restart"
    );

    std::env::remove_var("RUNT_TRUST_KEY_PATH");
}

#[tokio::test(start_paused = true)]
async fn test_ephemeral_room_skips_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let blob_store = Arc::new(BlobStore::new(dir.path().join("blobs")));
    let notebook_uuid = uuid::Uuid::new_v4();
    let room = NotebookRoom::new_fresh(notebook_uuid, None, dir.path(), blob_store, true);

    assert!(room.persist_tx.is_none());
    assert!(room.is_ephemeral.load(std::sync::atomic::Ordering::Relaxed));

    // No .automerge file should exist
    let filename = notebook_doc_filename(&notebook_uuid.to_string());
    assert!(!dir.path().join(&filename).exists());
}

#[tokio::test(start_paused = true)]
async fn test_session_room_persists() {
    let dir = tempfile::tempdir().unwrap();
    let blob_store = Arc::new(BlobStore::new(dir.path().join("blobs")));
    let notebook_uuid = uuid::Uuid::new_v4();
    let room = NotebookRoom::new_fresh(notebook_uuid, None, dir.path(), blob_store, false);

    assert!(room.persist_tx.is_some());
    assert!(!room.is_ephemeral.load(std::sync::atomic::Ordering::Relaxed));
}

#[tokio::test(start_paused = true)]
async fn test_ephemeral_room_has_metadata_flag() {
    let dir = tempfile::tempdir().unwrap();
    let blob_store = Arc::new(BlobStore::new(dir.path().join("blobs")));
    let notebook_uuid = uuid::Uuid::new_v4();
    let room = NotebookRoom::new_fresh(notebook_uuid, None, dir.path(), blob_store, true);

    let doc = room.doc.read().await;
    assert_eq!(doc.get_metadata("ephemeral"), Some("true".to_string()));
}

/// Helper to build a snapshot with UV inline deps.
fn snapshot_with_uv(deps: Vec<String>) -> NotebookMetadataSnapshot {
    NotebookMetadataSnapshot {
        kernelspec: None,
        language_info: None,
        runt: crate::notebook_metadata::RuntMetadata {
            schema_version: "1".to_string(),
            env_id: None,
            uv: Some(crate::notebook_metadata::UvInlineMetadata {
                dependencies: deps,
                requires_python: None,
                prerelease: None,
            }),
            conda: None,
            pixi: None,
            deno: None,
            trust_signature: None,
            trust_timestamp: None,
            extra: std::collections::BTreeMap::new(),
        },
    }
}

/// Helper to build a snapshot with conda inline deps.
fn snapshot_with_conda(deps: Vec<String>) -> NotebookMetadataSnapshot {
    NotebookMetadataSnapshot {
        kernelspec: None,
        language_info: None,
        runt: crate::notebook_metadata::RuntMetadata {
            schema_version: "1".to_string(),
            env_id: None,
            uv: None,
            conda: Some(crate::notebook_metadata::CondaInlineMetadata {
                dependencies: deps,
                channels: vec!["conda-forge".to_string()],
                python: None,
            }),
            pixi: None,
            deno: None,
            trust_signature: None,
            trust_timestamp: None,
            extra: std::collections::BTreeMap::new(),
        },
    }
}

/// Helper to build an empty snapshot (no deps).
fn snapshot_empty() -> NotebookMetadataSnapshot {
    NotebookMetadataSnapshot {
        kernelspec: None,
        language_info: None,
        runt: crate::notebook_metadata::RuntMetadata {
            schema_version: "1".to_string(),
            env_id: None,
            uv: None,
            conda: None,
            pixi: None,
            deno: None,
            trust_signature: None,
            trust_timestamp: None,
            extra: std::collections::BTreeMap::new(),
        },
    }
}

#[test]
fn test_check_inline_deps_uv() {
    use notebook_protocol::connection::{EnvSource, PackageManager};
    let snapshot = snapshot_with_uv(vec!["numpy".to_string()]);
    assert_eq!(
        check_inline_deps(&snapshot),
        Some(EnvSource::Inline(PackageManager::Uv))
    );
}

#[test]
fn test_check_inline_deps_conda() {
    use notebook_protocol::connection::{EnvSource, PackageManager};
    let snapshot = snapshot_with_conda(vec!["pandas".to_string()]);
    assert_eq!(
        check_inline_deps(&snapshot),
        Some(EnvSource::Inline(PackageManager::Conda))
    );
}

#[test]
fn test_check_inline_deps_empty() {
    let snapshot = snapshot_empty();
    assert_eq!(check_inline_deps(&snapshot), None);
}

#[test]
fn test_check_inline_deps_empty_array() {
    // Snapshot with empty deps array - should return None
    let snapshot = snapshot_with_uv(vec![]);
    assert_eq!(check_inline_deps(&snapshot), None);
}

#[test]
fn test_check_inline_deps_uv_priority() {
    // Snapshot with both UV and conda deps - UV takes priority
    let snapshot = NotebookMetadataSnapshot {
        kernelspec: None,
        language_info: None,
        runt: crate::notebook_metadata::RuntMetadata {
            schema_version: "1".to_string(),
            env_id: None,
            uv: Some(crate::notebook_metadata::UvInlineMetadata {
                dependencies: vec!["numpy".to_string()],
                requires_python: None,
                prerelease: None,
            }),
            conda: Some(crate::notebook_metadata::CondaInlineMetadata {
                dependencies: vec!["pandas".to_string()],
                channels: vec!["conda-forge".to_string()],
                python: None,
            }),
            pixi: None,
            deno: None,
            trust_signature: None,
            trust_timestamp: None,
            extra: std::collections::BTreeMap::new(),
        },
    };
    use notebook_protocol::connection::{EnvSource, PackageManager};
    assert_eq!(
        check_inline_deps(&snapshot),
        Some(EnvSource::Inline(PackageManager::Uv))
    );
}

#[test]
fn test_check_inline_deps_deno() {
    // Snapshot with deno config - deno takes priority over everything
    let snapshot = NotebookMetadataSnapshot {
        kernelspec: None,
        language_info: None,
        runt: crate::notebook_metadata::RuntMetadata {
            schema_version: "1".to_string(),
            env_id: None,
            uv: Some(crate::notebook_metadata::UvInlineMetadata {
                dependencies: vec!["numpy".to_string()],
                requires_python: None,
                prerelease: None,
            }),
            conda: None,
            pixi: None,
            deno: Some(crate::notebook_metadata::DenoMetadata {
                permissions: vec![],
                import_map: None,
                config: None,
                flexible_npm_imports: None,
            }),
            trust_signature: None,
            trust_timestamp: None,
            extra: std::collections::BTreeMap::new(),
        },
    };
    use notebook_protocol::connection::EnvSource;
    assert_eq!(check_inline_deps(&snapshot), Some(EnvSource::Deno));
}

// Runtime detection tests now live in notebook-doc/src/metadata.rs
// (NotebookMetadataSnapshot::detect_runtime) with comprehensive coverage.

// ── Integration tests for save_notebook_to_disk ────────────────────────

/// Create a test room with a path pointing to a file in temp dir.
fn test_room_with_path(
    tmp: &tempfile::TempDir,
    notebook_filename: &str,
) -> (NotebookRoom, PathBuf) {
    let notebook_path = tmp.path().join(notebook_filename);
    let blob_store = test_blob_store(tmp);
    let notebook_id = notebook_path.to_string_lossy().to_string();

    let doc = crate::notebook_doc::NotebookDoc::new(&notebook_id);
    let (changed_tx, _) = broadcast::channel(16);
    let (kernel_broadcast_tx, _) = broadcast::channel(KERNEL_BROADCAST_CAPACITY);
    let persist_path = tmp.path().join("doc.automerge");
    let (persist_tx, persist_rx) = watch::channel::<Option<Vec<u8>>>(None);
    let (flush_request_tx, flush_rx) = mpsc::unbounded_channel::<FlushRequest>();
    spawn_persist_debouncer(persist_rx, flush_rx, persist_path.clone());

    let (presence_tx, _) = broadcast::channel(64);

    let (state_changed_tx, _) = broadcast::channel(16);
    let state = runtime_doc::RuntimeStateHandle::new(RuntimeStateDoc::new(), state_changed_tx);
    let room = NotebookRoom {
        id: uuid::Uuid::new_v4(),
        doc: Arc::new(RwLock::new(doc)),
        changed_tx,
        kernel_broadcast_tx,
        presence_tx,
        presence: Arc::new(RwLock::new(PresenceState::new())),
        persist_tx: Some(persist_tx),
        flush_request_tx: Some(flush_request_tx),
        persist_path,
        active_peers: AtomicUsize::new(0),
        had_peers: AtomicBool::new(false),
        is_ephemeral: AtomicBool::new(false),
        blob_store,
        trust_state: Arc::new(RwLock::new(TrustState {
            status: runt_trust::TrustStatus::Untrusted,
            info: runt_trust::TrustInfo {
                status: runt_trust::TrustStatus::Untrusted,
                uv_dependencies: vec![],
                conda_dependencies: vec![],
                conda_channels: vec![],
            },
            pending_launch: false,
        })),
        path: RwLock::new(Some(notebook_path.clone())),
        nbformat_attachments: Arc::new(RwLock::new(HashMap::new())),
        working_dir: Arc::new(RwLock::new(None)),

        is_loading: AtomicBool::new(false),
        last_self_write: AtomicU64::new(0),
        last_save_sources: Arc::new(RwLock::new(HashMap::new())),
        watcher_shutdown_tx: Mutex::new(None),
        state,
        runtime_agent_handle: Arc::new(Mutex::new(None)),
        runtime_agent_env_path: Arc::new(RwLock::new(None)),
        runtime_agent_launched_config: Arc::new(RwLock::new(None)),
        runtime_agent_request_tx: Arc::new(Mutex::new(None)),
        pending_runtime_agent_connect_tx: Arc::new(Mutex::new(None)),
        runtime_agent_generation: Arc::new(AtomicU64::new(0)),
        next_queue_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        current_runtime_agent_id: Arc::new(RwLock::new(None)),
    };

    (room, notebook_path)
}

#[tokio::test]
async fn test_save_notebook_to_disk_creates_valid_nbformat() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, notebook_path) = test_room_with_path(&tmp, "test.ipynb");

    // Add cells to the doc
    {
        let mut doc = room.doc.write().await;
        doc.add_cell(0, "cell1", "code").unwrap();
        doc.update_source("cell1", "print('hello')").unwrap();
        doc.add_cell(1, "cell2", "markdown").unwrap();
        doc.update_source("cell2", "# Title").unwrap();
    }

    // Save to disk
    save_notebook_to_disk(&room, None).await.unwrap();

    // Read and validate with nbformat
    let content = std::fs::read_to_string(&notebook_path).unwrap();
    let notebook: nbformat::v4::Notebook =
        serde_json::from_str(&content).expect("Saved notebook should be valid nbformat");

    assert_eq!(notebook.cells.len(), 2);
    assert_eq!(notebook.nbformat, 4);
    assert!(
        notebook.nbformat_minor >= 5,
        "Cell IDs require nbformat_minor >= 5"
    );
}

#[tokio::test]
async fn test_save_notebook_to_disk_preserves_unknown_metadata() {
    use std::io::Write;
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, notebook_path) = test_room_with_path(&tmp, "metadata.ipynb");

    // Create existing file with unknown metadata fields
    {
        let mut f = std::fs::File::create(&notebook_path).unwrap();
        writeln!(
            f,
            r#"{{
                "nbformat": 4,
                "nbformat_minor": 5,
                "metadata": {{
                    "custom_extension": {{"key": "value"}},
                    "jupyter": {{"source_hidden": true}},
                    "runt": {{"trust_signature": "abc123", "schema_version": "1"}}
                }},
                "cells": []
            }}"#
        )
        .unwrap();
    }

    // Add a cell and save
    {
        let mut doc = room.doc.write().await;
        doc.add_cell(0, "cell1", "code").unwrap();
        doc.update_source("cell1", "x = 1").unwrap();
    }

    save_notebook_to_disk(&room, None).await.unwrap();

    // Verify unknown metadata is preserved
    let content = std::fs::read_to_string(&notebook_path).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&content).unwrap();
    let metadata = saved.get("metadata").unwrap();

    // custom_extension should be preserved
    assert!(
        metadata.get("custom_extension").is_some(),
        "custom_extension should be preserved"
    );
    assert_eq!(
        metadata.get("custom_extension").unwrap().get("key"),
        Some(&serde_json::json!("value"))
    );

    // jupyter should be preserved
    assert!(
        metadata.get("jupyter").is_some(),
        "jupyter metadata should be preserved"
    );

    // trust_signature in runt should be preserved (deep-merge)
    let runt = metadata.get("runt").unwrap();
    assert_eq!(
        runt.get("trust_signature"),
        Some(&serde_json::json!("abc123")),
        "trust_signature should be preserved via deep-merge"
    );
}

#[tokio::test]
async fn test_save_notebook_to_disk_enforces_nbformat_minor_5() {
    use std::io::Write;
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, notebook_path) = test_room_with_path(&tmp, "old_minor.ipynb");

    // Create existing file with old nbformat_minor
    {
        let mut f = std::fs::File::create(&notebook_path).unwrap();
        writeln!(
            f,
            r#"{{
                "nbformat": 4,
                "nbformat_minor": 2,
                "metadata": {{}},
                "cells": []
            }}"#
        )
        .unwrap();
    }

    // Add a cell with an id and save
    {
        let mut doc = room.doc.write().await;
        doc.add_cell(0, "cell-with-id", "code").unwrap();
    }

    save_notebook_to_disk(&room, None).await.unwrap();

    // Verify nbformat_minor is upgraded to 5
    let content = std::fs::read_to_string(&notebook_path).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(
        saved.get("nbformat_minor"),
        Some(&serde_json::json!(5)),
        "nbformat_minor should be upgraded to 5 when writing cell IDs"
    );
}

#[tokio::test]
async fn test_save_notebook_to_disk_with_outputs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, notebook_path) = test_room_with_path(&tmp, "outputs.ipynb");

    // Add a cell with a raw output stored in RuntimeStateDoc
    let eid = "test-exec-1";
    {
        let mut doc = room.doc.write().await;
        doc.add_cell(0, "cell1", "code").unwrap();
        doc.update_source("cell1", "print('hello')").unwrap();
        doc.set_execution_id("cell1", Some(eid)).unwrap();
    }
    room.state
        .with_doc(|sd| {
            let output: serde_json::Value =
                serde_json::json!({"output_type": "stream", "name": "stdout", "text": ["hello\n"]});
            sd.create_execution(eid, "cell1")?;
            sd.set_execution_count(eid, 1)?;
            sd.set_outputs(eid, &[output])?;
            sd.set_execution_done(eid, true)?;
            Ok(())
        })
        .unwrap();

    save_notebook_to_disk(&room, None).await.unwrap();

    // Read and validate
    let content = std::fs::read_to_string(&notebook_path).unwrap();
    let notebook: nbformat::v4::Notebook =
        serde_json::from_str(&content).expect("Should be valid nbformat with outputs");

    assert_eq!(notebook.cells.len(), 1);
    if let nbformat::v4::Cell::Code { outputs, .. } = &notebook.cells[0] {
        assert_eq!(outputs.len(), 1, "Should have one output");
        // Verify it's a stream output (nbformat types may vary)
        match &outputs[0] {
            nbformat::v4::Output::Stream { name, .. } => {
                assert_eq!(name, "stdout");
            }
            _ => panic!("Expected stream output"),
        }
    } else {
        panic!("Expected code cell");
    }
}

#[test]
fn test_is_untitled_notebook_with_uuid() {
    assert!(is_untitled_notebook("550e8400-e29b-41d4-a716-446655440000"));
    assert!(is_untitled_notebook("a1b2c3d4-e5f6-7890-abcd-ef1234567890"));
}

#[test]
fn test_is_untitled_notebook_with_path() {
    assert!(!is_untitled_notebook("/home/user/notebook.ipynb"));
    assert!(!is_untitled_notebook("./relative/path.ipynb"));
    assert!(!is_untitled_notebook("notebook.ipynb"));
}

/// Test that the debouncer flushes at max interval even during continuous updates.
///
/// Uses short intervals (50ms debounce, 200ms max) for fast testing.
#[tokio::test]
async fn test_persist_debouncer_max_interval_flush() {
    use std::time::Duration;

    let tmp = tempfile::TempDir::new().unwrap();
    let persist_path = tmp.path().join("test.automerge");

    // Create watch channel and spawn debouncer with short intervals for testing
    let (tx, rx) = watch::channel::<Option<Vec<u8>>>(None);
    let (_flush_tx, flush_rx) = mpsc::unbounded_channel::<FlushRequest>();
    let config = PersistDebouncerConfig {
        debounce_ms: 50,       // 50ms debounce window
        max_interval_ms: 200,  // 200ms max between flushes
        check_interval_ms: 10, // Check every 10ms
    };
    spawn_persist_debouncer_with_config(rx, flush_rx, persist_path.clone(), config);

    // Send updates every 20ms (faster than 50ms debounce, so debounce never triggers)
    // The 200ms max interval should force a flush even without a quiet period.
    for i in 0..20 {
        let data = format!("update-{}", i).into_bytes();
        tx.send(Some(data)).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Total time: 20 * 20ms = 400ms, which is > 200ms max interval

    // Give debouncer time to flush
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        persist_path.exists(),
        "File should exist after max interval even with continuous updates"
    );

    // Verify content is from an update
    let content = std::fs::read(&persist_path).unwrap();
    let content_str = String::from_utf8_lossy(&content);
    assert!(
        content_str.starts_with("update-"),
        "Content should be from an update"
    );
}

/// Regression test for the eviction/debouncer race.
///
/// The bug: room eviction used to remove the room from the HashMap before
/// the persist debouncer's debounce window elapsed, so a fast reconnect
/// would load stale/empty bytes. The fix: eviction sends a flush request
/// on `flush_request_tx` and awaits an ack on the oneshot *before* the
/// HashMap mutation. This test pins the contract: the ack must arrive
/// after the latest watch value has been written to disk, well inside
/// the debounce window.
#[tokio::test]
async fn test_persist_debouncer_flush_request_is_synchronous() {
    use std::time::Duration;

    let tmp = tempfile::TempDir::new().unwrap();
    let persist_path = tmp.path().join("race.automerge");

    // Use production defaults for debounce (500ms) so the timed flush
    // can't mask the flush-request ack timing.
    let (tx, rx) = watch::channel::<Option<Vec<u8>>>(None);
    let (flush_tx, flush_rx) = mpsc::unbounded_channel::<FlushRequest>();
    spawn_persist_debouncer(rx, flush_rx, persist_path.clone());

    // Push latest bytes and request a flush immediately. No sleeps — the
    // debounce timer must not be the thing that persists this write.
    let payload = b"eviction-latest-bytes".to_vec();
    tx.send(Some(payload.clone())).unwrap();

    let (ack_tx, ack_rx) = oneshot::channel::<bool>();
    flush_tx.send(ack_tx).unwrap();

    // The ack must come back fast (success=true). 500ms is 10x margin over
    // local disk I/O.
    let ack_result = tokio::time::timeout(Duration::from_millis(500), ack_rx).await;
    assert!(
        matches!(ack_result, Ok(Ok(true))),
        "flush ack did not arrive synchronously with success=true: {:?}",
        ack_result
    );

    // And the file on disk must hold the latest payload, not stale bytes.
    assert!(persist_path.exists(), "file must exist after flush ack");
    let on_disk = std::fs::read(&persist_path).unwrap();
    assert_eq!(
        on_disk, payload,
        "file contents must match latest payload after flush ack"
    );
}

/// The flush-and-ack must report I/O failures so the eviction task can
/// retry (rather than remove the room and leave stale bytes on disk).
/// Force a write failure by pointing persist_path at a non-writable
/// location, then confirm the ack carries `false`.
#[tokio::test]
async fn test_persist_debouncer_flush_request_reports_write_failure() {
    use std::time::Duration;

    let tmp = tempfile::TempDir::new().unwrap();
    // Write target is a file *inside* a path that includes a non-directory
    // component — `std::fs::create_dir_all` on parent will succeed, but
    // `std::fs::write` on the final path will fail because it conflicts
    // with a regular file we planted there. This simulates ENOSPC-class
    // failures without needing OS-specific tricks.
    let blocker = tmp.path().join("blocker");
    std::fs::write(&blocker, b"regular file").unwrap();
    let persist_path = blocker.join("race.automerge");

    let (tx, rx) = watch::channel::<Option<Vec<u8>>>(None);
    let (flush_tx, flush_rx) = mpsc::unbounded_channel::<FlushRequest>();
    spawn_persist_debouncer(rx, flush_rx, persist_path.clone());

    let payload = b"write-will-fail".to_vec();
    tx.send(Some(payload)).unwrap();

    let (ack_tx, ack_rx) = oneshot::channel::<bool>();
    flush_tx.send(ack_tx).unwrap();

    let ack_result = tokio::time::timeout(Duration::from_millis(500), ack_rx).await;
    assert!(
        matches!(ack_result, Ok(Ok(false))),
        "flush ack must report write failure: {:?}",
        ack_result
    );
    // The file should not exist, since the write errored before any bytes hit disk.
    assert!(
        !persist_path.exists(),
        "persist_path must not exist after failed write"
    );
}

// ==========================================================================
// File watcher tests
// ==========================================================================

#[test]
fn test_parse_cells_from_ipynb_with_ids() {
    let json = serde_json::json!({
        "cells": [
            {
                "id": "cell-1",
                "cell_type": "code",
                "source": "print('hello')",
                "execution_count": 5,
                "outputs": []
            },
            {
                "id": "cell-2",
                "cell_type": "markdown",
                "source": ["# Title\n", "Body"],
                "execution_count": null,
                "outputs": []
            }
        ]
    });

    let parsed = parse_cells_from_ipynb(&json).expect("Should parse valid notebook");
    let cells = &parsed.cells;
    assert_eq!(cells.len(), 2);
    assert_eq!(cells[0].id, "cell-1");
    assert_eq!(cells[0].cell_type, "code");
    assert_eq!(cells[0].source, "print('hello')");
    assert_eq!(cells[0].execution_count, "5");
    assert_eq!(cells[1].id, "cell-2");
    assert_eq!(cells[1].cell_type, "markdown");
    assert_eq!(cells[1].source, "# Title\nBody");
    assert_eq!(cells[1].execution_count, "null");
    // Empty `outputs` arrays on disk produce no entries in the outputs map.
    assert!(parsed.outputs_by_cell.is_empty());
}

#[test]
fn test_parse_cells_from_ipynb_missing_ids() {
    // Older notebooks (pre-nbformat 4.5) don't have cell IDs
    let json = serde_json::json!({
        "cells": [
            {
                "cell_type": "code",
                "source": "x = 1",
                "execution_count": null,
                "outputs": []
            },
            {
                "cell_type": "code",
                "source": "y = 2",
                "execution_count": null,
                "outputs": []
            }
        ]
    });

    let parsed = parse_cells_from_ipynb(&json).expect("Should parse valid notebook");
    let cells = &parsed.cells;
    assert_eq!(cells.len(), 2);
    // Should generate fallback IDs based on index
    assert_eq!(cells[0].id, "__external_cell_0");
    assert_eq!(cells[1].id, "__external_cell_1");
    assert_eq!(cells[0].source, "x = 1");
    assert_eq!(cells[1].source, "y = 2");
}

#[test]
fn test_parse_cells_from_ipynb_empty() {
    // Valid notebook with empty cells array - should return Some([])
    let json = serde_json::json!({
        "cells": []
    });
    let parsed = parse_cells_from_ipynb(&json).expect("Should parse valid empty notebook");
    assert!(parsed.cells.is_empty());
    assert!(parsed.outputs_by_cell.is_empty());
}

#[test]
fn test_parse_cells_from_ipynb_no_cells_key() {
    // Invalid notebook (missing cells key) - should return None
    let json = serde_json::json!({
        "metadata": {}
    });
    assert!(
        parse_cells_from_ipynb(&json).is_none(),
        "Should return None for invalid notebook"
    );
}

#[tokio::test]
async fn test_apply_ipynb_changes_clears_all_cells() {
    // Valid "delete all cells" case — empty cells array from external
    // file should clear the doc, but ONLY when we have a save baseline
    // (last_save_sources populated). Without a save snapshot, deletions
    // are skipped to prevent the Run 38 cell-loss bug.
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _) = test_room_with_path(&tmp, "test.ipynb");

    // Add cells to the doc
    {
        let mut doc = room.doc.write().await;
        doc.add_cell(0, "cell-1", "code").unwrap();
        doc.update_source("cell-1", "x = 1").unwrap();
    }

    // Populate last_save_sources — simulates a save that included the cell
    {
        let mut saved = room.last_save_sources.write().await;
        saved.insert("cell-1".to_string(), "x = 1".to_string());
    }

    // Apply empty external cells - should delete all cells (we have
    // a save baseline confirming cell-1 was on disk before)
    let external_cells = vec![];
    let changed = apply_ipynb_changes(
        &room,
        &external_cells,
        &HashMap::new(),
        &HashMap::new(),
        false,
    )
    .await;
    assert!(changed, "Should apply changes to clear all cells");

    // Verify all cells were deleted
    let cells = {
        let doc = room.doc.read().await;
        doc.get_cells()
    };
    assert!(cells.is_empty(), "All cells should be deleted");
}

#[tokio::test]
async fn test_apply_ipynb_changes_updates_execution_count() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _) = test_room_with_path(&tmp, "test.ipynb");

    // Add cells to the doc
    {
        let mut doc = room.doc.write().await;
        doc.add_cell(0, "cell-1", "code").unwrap();
    }

    // Apply external changes with execution_count
    let external_cells = vec![CellSnapshot {
        id: "cell-1".to_string(),
        cell_type: "code".to_string(),
        position: "80".to_string(),
        source: String::new(),
        execution_count: "42".to_string(),
        metadata: serde_json::json!({}),
        resolved_assets: std::collections::HashMap::new(),
    }];

    let changed = apply_ipynb_changes(
        &room,
        &external_cells,
        &HashMap::new(),
        &HashMap::new(),
        false,
    )
    .await;
    assert!(changed, "Should detect execution_count change");

    // execution_count is now in RuntimeStateDoc via synthetic execution_id
    let doc = room.doc.read().await;
    let eid = doc.get_execution_id("cell-1");
    drop(doc);
    assert!(eid.is_some(), "Should have execution_id set");
    let ec = room
        .state
        .read(|sd| {
            sd.get_execution(eid.as_ref().unwrap())
                .unwrap()
                .execution_count
        })
        .unwrap();
    assert_eq!(ec, Some(42));
}

#[tokio::test]
async fn test_apply_ipynb_changes_preserves_execution_count_when_kernel_running() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _) = test_room_with_path(&tmp, "test.ipynb");

    // Add cell with execution_count in RuntimeStateDoc via synthetic eid
    let eid = "existing-exec-1";
    {
        let mut doc = room.doc.write().await;
        doc.add_cell(0, "cell-1", "code").unwrap();
        doc.set_execution_id("cell-1", Some(eid)).unwrap();
    }
    room.state
        .with_doc(|sd| {
            sd.create_execution(eid, "cell-1")?;
            sd.set_execution_count(eid, 10)?;
            sd.set_execution_done(eid, true)?;
            Ok(())
        })
        .unwrap();

    // Apply external changes while kernel is "running"
    let external_cells = vec![CellSnapshot {
        id: "cell-1".to_string(),
        cell_type: "code".to_string(),
        position: "80".to_string(),
        source: "new source".to_string(),
        execution_count: "5".to_string(),
        metadata: serde_json::json!({}),
        resolved_assets: std::collections::HashMap::new(),
    }];

    let changed = apply_ipynb_changes(
        &room,
        &external_cells,
        &HashMap::new(),
        &HashMap::new(),
        true,
    )
    .await;
    assert!(changed, "Should apply source change");

    let cells = {
        let doc = room.doc.read().await;
        doc.get_cells()
    };
    // Source should be updated
    assert_eq!(cells[0].source, "new source");
    // execution_count should be preserved in RuntimeStateDoc (kernel running)
    let exec = room.state.read(|sd| sd.get_execution(eid)).unwrap();
    assert_eq!(exec.unwrap().execution_count, Some(10));
}

#[tokio::test]
async fn test_apply_ipynb_changes_new_cell_with_outputs_while_kernel_running() {
    // New external cells should get their external outputs even when kernel is running
    // (they don't have any in-progress state to preserve)
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _) = test_room_with_path(&tmp, "test.ipynb");

    // Start with one cell
    {
        let mut doc = room.doc.write().await;
        doc.add_cell(0, "existing-cell", "code").unwrap();
    }

    // Add a new external cell with outputs while kernel is "running"
    let external_cells = vec![
        CellSnapshot {
            id: "existing-cell".to_string(),
            cell_type: "code".to_string(),
            position: "80".to_string(),
            source: String::new(),
            execution_count: "null".to_string(),
            metadata: serde_json::json!({}),
            resolved_assets: std::collections::HashMap::new(),
        },
        CellSnapshot {
            id: "new-cell".to_string(),
            cell_type: "code".to_string(),
            position: "81".to_string(),
            source: "print('new')".to_string(),
            execution_count: "42".to_string(),
            metadata: serde_json::json!({}),
            resolved_assets: std::collections::HashMap::new(),
        },
    ];
    let mut external_outputs: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    external_outputs.insert(
        "new-cell".to_string(),
        vec![serde_json::json!({"output_type":"execute_result"})],
    );

    let changed = apply_ipynb_changes(
        &room,
        &external_cells,
        &external_outputs,
        &HashMap::new(),
        true,
    )
    .await;
    assert!(changed, "Should add new cell");

    let cells = {
        let doc = room.doc.read().await;
        doc.get_cells()
    };
    assert_eq!(cells.len(), 2);

    // New cell should have external outputs and execution_count in RuntimeStateDoc
    let new_cell = cells.iter().find(|c| c.id == "new-cell").unwrap();
    assert_eq!(new_cell.source, "print('new')");

    // Outputs and execution_count are in RuntimeStateDoc keyed by synthetic execution_id
    let eid = {
        let doc = room.doc.read().await;
        doc.get_execution_id("new-cell")
            .expect("new-cell should have execution_id")
    };
    let exec = room.state.read(|sd| sd.get_execution(&eid)).unwrap();
    assert_eq!(exec.unwrap().execution_count, Some(42));
    let outputs = room.state.read(|sd| sd.get_outputs(&eid)).unwrap();
    assert_eq!(outputs.len(), 1);
    let manifest = &outputs[0];
    assert!(
        manifest.is_object(),
        "Output should be a manifest object, got: {}",
        manifest
    );
    // Verify the manifest resolves back to the original output
    let parsed_manifest: crate::output_store::OutputManifest =
        serde_json::from_value(manifest.clone()).unwrap();
    let resolved = crate::output_store::resolve_manifest(&parsed_manifest, &room.blob_store)
        .await
        .unwrap();
    assert_eq!(resolved["output_type"], "execute_result");
}

#[tokio::test]
async fn test_apply_ipynb_changes_wholesale_replacement() {
    // When external file has entirely different cell IDs (zero overlap),
    // the rebuild path should replace all cells correctly (issue #1310).
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _) = test_room_with_path(&tmp, "test.ipynb");

    // Add original cells
    {
        let mut doc = room.doc.write().await;
        doc.add_cell(0, "old-a", "code").unwrap();
        doc.update_source("old-a", "x = 1").unwrap();
        doc.add_cell(1, "old-b", "code").unwrap();
        doc.update_source("old-b", "y = 2").unwrap();
        doc.add_cell(2, "old-c", "markdown").unwrap();
        doc.update_source("old-c", "# Hello").unwrap();
    }

    // Completely replace with different cells (zero common IDs)
    let external_cells = vec![
        CellSnapshot {
            id: "new-1".to_string(),
            cell_type: "code".to_string(),
            position: "80".to_string(),
            source: "a = 10".to_string(),
            execution_count: "1".to_string(),
            metadata: serde_json::json!({}),
            resolved_assets: std::collections::HashMap::new(),
        },
        CellSnapshot {
            id: "new-2".to_string(),
            cell_type: "code".to_string(),
            position: "81".to_string(),
            source: "b = 20".to_string(),
            execution_count: "2".to_string(),
            metadata: serde_json::json!({}),
            resolved_assets: std::collections::HashMap::new(),
        },
    ];

    let changed = apply_ipynb_changes(
        &room,
        &external_cells,
        &HashMap::new(),
        &HashMap::new(),
        false,
    )
    .await;
    assert!(changed, "Should detect wholesale replacement");

    let cells = {
        let doc = room.doc.read().await;
        doc.get_cells()
    };
    assert_eq!(cells.len(), 2, "Should have exactly 2 new cells");
    assert_eq!(cells[0].id, "new-1");
    assert_eq!(cells[0].source, "a = 10");
    assert_eq!(cells[1].id, "new-2");
    assert_eq!(cells[1].source, "b = 20");
    // Old cells should be gone
    assert!(cells.iter().all(|c| !c.id.starts_with("old-")));
}

#[tokio::test]
async fn test_apply_ipynb_changes_partial_overlap_preserves_unsaved() {
    // When there IS overlap between current and external cells, the
    // incremental path should preserve user-added cells not in
    // last_save_sources.
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _) = test_room_with_path(&tmp, "test.ipynb");

    // Add cells and populate last_save_sources to simulate a save
    {
        let mut doc = room.doc.write().await;
        doc.add_cell(0, "keep", "code").unwrap();
        doc.update_source("keep", "x = 1").unwrap();
        doc.add_cell(1, "remove", "code").unwrap();
        doc.update_source("remove", "y = 2").unwrap();
    }
    {
        let mut saved = room.last_save_sources.write().await;
        saved.insert("keep".to_string(), "x = 1".to_string());
        saved.insert("remove".to_string(), "y = 2".to_string());
    }

    // Add a cell NOT in last_save_sources (user just added it)
    {
        let mut doc = room.doc.write().await;
        doc.add_cell(2, "user-added", "code").unwrap();
        doc.update_source("user-added", "z = 3").unwrap();
    }

    // External file has "keep" (overlap) but not "remove" or "user-added"
    let external_cells = vec![CellSnapshot {
        id: "keep".to_string(),
        cell_type: "code".to_string(),
        position: "80".to_string(),
        source: "x = 1".to_string(),
        execution_count: "null".to_string(),
        metadata: serde_json::json!({}),
        resolved_assets: std::collections::HashMap::new(),
    }];

    let changed = apply_ipynb_changes(
        &room,
        &external_cells,
        &HashMap::new(),
        &HashMap::new(),
        false,
    )
    .await;
    assert!(changed);

    let cells = {
        let doc = room.doc.read().await;
        doc.get_cells()
    };
    let ids: Vec<&str> = cells.iter().map(|c| c.id.as_str()).collect();
    assert!(
        ids.contains(&"keep"),
        "Overlapping cell should remain: {:?}",
        ids
    );
    assert!(
        !ids.contains(&"remove"),
        "Saved cell removed externally should be deleted: {:?}",
        ids
    );
    assert!(
        ids.contains(&"user-added"),
        "User-added cell not in save snapshot should be preserved: {:?}",
        ids
    );
}

#[tokio::test]
async fn test_apply_ipynb_changes_no_save_snapshot_preserves_crdt_cells() {
    // Regression test for Run 38 cell-loss: when last_save_sources is
    // empty (initial autosave with 0 cells), the file watcher must NOT
    // delete CRDT cells that aren't on disk. Without a save baseline we
    // can't distinguish "externally deleted" from "just created in CRDT."
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _) = test_room_with_path(&tmp, "test.ipynb");

    // Add cells to the CRDT (simulates MCP client creating cells)
    {
        let mut doc = room.doc.write().await;
        doc.add_cell(0, "cell-a", "code").unwrap();
        doc.update_source("cell-a", "x = 1").unwrap();
        doc.add_cell(1, "cell-b", "code").unwrap();
        doc.update_source("cell-b", "y = 2").unwrap();
    }

    // Do NOT populate last_save_sources — simulates the case where
    // the only save was with 0 cells (empty HashMap is the default).
    assert!(room.last_save_sources.read().await.is_empty());

    // External file has 0 cells (the autosave wrote an empty notebook)
    let external_cells: Vec<CellSnapshot> = vec![];

    let changed = apply_ipynb_changes(
        &room,
        &external_cells,
        &HashMap::new(),
        &HashMap::new(),
        false,
    )
    .await;
    // No changes should be applied — cells preserved
    assert!(
        !changed,
        "Should not delete cells when no save snapshot exists"
    );

    let cells = {
        let doc = room.doc.read().await;
        doc.get_cells()
    };
    let ids: Vec<&str> = cells.iter().map(|c| c.id.as_str()).collect();
    assert!(
        ids.contains(&"cell-a"),
        "CRDT cell should be preserved when no save snapshot: {:?}",
        ids
    );
    assert!(
        ids.contains(&"cell-b"),
        "CRDT cell should be preserved when no save snapshot: {:?}",
        ids
    );
}

#[tokio::test]
async fn test_load_notebook_from_disk_routes_outputs_through_blob_store() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);

    // Create a .ipynb file with outputs including a large base64 image
    let large_image = "x".repeat(16 * 1024); // 16KB, above 8KB inline threshold
    let notebook_json = serde_json::json!({
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": {},
        "cells": [
            {
                "id": "cell-1",
                "cell_type": "code",
                "source": "1 + 1",
                "execution_count": 1,
                "metadata": {},
                "outputs": [
                    {
                        "output_type": "execute_result",
                        "execution_count": 1,
                        "data": { "text/plain": "2" },
                        "metadata": {}
                    }
                ]
            },
            {
                "id": "cell-2",
                "cell_type": "code",
                "source": "display(img)",
                "execution_count": 2,
                "metadata": {},
                "outputs": [
                    {
                        "output_type": "display_data",
                        "data": {
                            "text/plain": "<Image>",
                            "image/png": large_image
                        },
                        "metadata": {}
                    }
                ]
            },
            {
                "id": "cell-3",
                "cell_type": "code",
                "source": "print('hi')",
                "execution_count": 3,
                "metadata": {},
                "outputs": [
                    {
                        "output_type": "stream",
                        "name": "stdout",
                        "text": "hi\n"
                    }
                ]
            }
        ]
    });

    let ipynb_path = tmp.path().join("test.ipynb");
    std::fs::write(
        &ipynb_path,
        serde_json::to_string_pretty(&notebook_json).unwrap(),
    )
    .unwrap();

    let notebook_id = ipynb_path.to_string_lossy().to_string();
    let mut doc = crate::notebook_doc::NotebookDoc::new(&notebook_id);
    let mut state_doc = RuntimeStateDoc::new();

    let count = load_notebook_from_disk_with_state_doc(
        &mut doc,
        Some(&mut state_doc),
        &ipynb_path,
        &blob_store,
    )
    .await
    .unwrap();
    assert_eq!(count, 3);

    let cells = doc.get_cells();
    assert_eq!(cells.len(), 3);

    // Each code cell with outputs should have an execution_id pointing to state_doc
    for cell in &cells {
        if let Some(eid) = doc.get_execution_id(&cell.id) {
            let outputs = state_doc.get_outputs(&eid);
            assert!(
                !outputs.is_empty(),
                "Cell {} should have outputs in state doc",
                cell.id
            );
            for output_ref in &outputs {
                assert!(
                    output_ref.is_object(),
                    "Cell {} output should be a manifest object, got: {}",
                    cell.id,
                    output_ref
                );
                assert!(
                    output_ref.get("output_type").is_some(),
                    "Cell {} output manifest should have output_type",
                    cell.id
                );
            }
        }
    }

    // Resolve cell-1's execute_result and verify round-trip
    let eid1 = doc
        .get_execution_id("cell-1")
        .expect("cell-1 should have execution_id");
    let outputs1 = state_doc.get_outputs(&eid1);
    let manifest = &outputs1[0];
    let parsed_manifest: crate::output_store::OutputManifest =
        serde_json::from_value(manifest.clone()).unwrap();
    let resolved = crate::output_store::resolve_manifest(&parsed_manifest, &blob_store)
        .await
        .unwrap();
    assert_eq!(resolved["output_type"], "execute_result");
    assert_eq!(resolved["data"]["text/plain"], "2");
    assert_eq!(resolved["execution_count"], 1);

    // Resolve cell-2's display_data with the large image
    let eid2 = doc
        .get_execution_id("cell-2")
        .expect("cell-2 should have execution_id");
    let outputs2 = state_doc.get_outputs(&eid2);
    let manifest = &outputs2[0];
    let parsed_manifest2: crate::output_store::OutputManifest =
        serde_json::from_value(manifest.clone()).unwrap();
    // The manifest should contain a blob ref for the large image, not inline
    let image_ref = &manifest["data"]["image/png"];
    assert!(
        image_ref.get("blob").is_some(),
        "Large image should be stored as blob ref, not inlined: {}",
        image_ref
    );
    // Full round-trip should reconstruct original data
    let resolved = crate::output_store::resolve_manifest(&parsed_manifest2, &blob_store)
        .await
        .unwrap();
    assert_eq!(resolved["output_type"], "display_data");
    assert_eq!(resolved["data"]["image/png"], large_image);

    // Resolve cell-3's stream output
    let eid3 = doc
        .get_execution_id("cell-3")
        .expect("cell-3 should have execution_id");
    let outputs3 = state_doc.get_outputs(&eid3);
    let manifest = &outputs3[0];
    let parsed_manifest: crate::output_store::OutputManifest =
        serde_json::from_value(manifest.clone()).unwrap();
    let resolved = crate::output_store::resolve_manifest(&parsed_manifest, &blob_store)
        .await
        .unwrap();
    assert_eq!(resolved["output_type"], "stream");
    assert_eq!(resolved["name"], "stdout");
    assert_eq!(resolved["text"], "hi\n");
}

#[tokio::test]
async fn test_load_notebook_from_disk_resolves_nbformat_attachments() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);

    let notebook_json = serde_json::json!({
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": {},
        "cells": [
            {
                "id": "markdown-1",
                "cell_type": "markdown",
                "source": ["![inline](attachment:image.png)"],
                "metadata": {},
                "attachments": {
                    "image.png": {
                        "image/png": "aGVsbG8="
                    }
                }
            }
        ]
    });

    let ipynb_path = tmp.path().join("attachments.ipynb");
    std::fs::write(
        &ipynb_path,
        serde_json::to_string_pretty(&notebook_json).unwrap(),
    )
    .unwrap();

    let notebook_id = ipynb_path.to_string_lossy().to_string();
    let mut doc = crate::notebook_doc::NotebookDoc::new(&notebook_id);

    let count = load_notebook_from_disk(&mut doc, &ipynb_path, &blob_store)
        .await
        .unwrap();
    assert_eq!(count, 1);

    let cells = doc.get_cells();
    assert_eq!(cells.len(), 1);

    let hash = cells[0]
        .resolved_assets
        .get("attachment:image.png")
        .expect("attachment should resolve into render assets");

    let bytes = blob_store.get(hash).await.unwrap().unwrap();
    assert_eq!(bytes, b"hello");
}

#[tokio::test]
async fn test_load_notebook_from_disk_skips_code_cell_asset_resolution() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);
    std::fs::write(tmp.path().join("image.png"), b"hello").unwrap();

    let notebook_json = serde_json::json!({
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": {},
        "cells": [
            {
                "id": "code-1",
                "cell_type": "code",
                "source": ["![inline](image.png)"],
                "metadata": {},
                "outputs": [],
                "execution_count": null
            }
        ]
    });

    let ipynb_path = tmp.path().join("code-assets.ipynb");
    std::fs::write(
        &ipynb_path,
        serde_json::to_string_pretty(&notebook_json).unwrap(),
    )
    .unwrap();

    let notebook_id = ipynb_path.to_string_lossy().to_string();
    let mut doc = crate::notebook_doc::NotebookDoc::new(&notebook_id);

    let count = load_notebook_from_disk(&mut doc, &ipynb_path, &blob_store)
        .await
        .unwrap();
    assert_eq!(count, 1);

    let cells = doc.get_cells();
    assert_eq!(cells.len(), 1);
    assert!(cells[0].resolved_assets.is_empty());
}

#[tokio::test]
async fn test_process_markdown_assets_rebuilds_stale_refs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, notebook_path) = test_room_with_path(&tmp, "assets.ipynb");
    std::fs::write(&notebook_path, "{}").unwrap();
    std::fs::write(tmp.path().join("img1.png"), b"one").unwrap();
    std::fs::write(tmp.path().join("img2.png"), b"two").unwrap();

    {
        let mut doc = room.doc.write().await;
        doc.add_cell(0, "markdown-1", "markdown").unwrap();
        doc.update_source("markdown-1", "![one](img1.png)").unwrap();
    }

    process_markdown_assets(&room).await;

    {
        let cells = room.doc.read().await.get_cells();
        let assets = &cells[0].resolved_assets;
        assert!(assets.contains_key("img1.png"));
        assert_eq!(assets.len(), 1);
    }

    {
        let mut doc = room.doc.write().await;
        doc.update_source("markdown-1", "![two](img2.png)").unwrap();
    }

    process_markdown_assets(&room).await;

    let cells = room.doc.read().await.get_cells();
    let assets = &cells[0].resolved_assets;
    assert!(assets.contains_key("img2.png"));
    assert!(!assets.contains_key("img1.png"));
    assert_eq!(assets.len(), 1);
}

#[tokio::test]
async fn test_save_notebook_to_disk_with_target_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _original_path) = test_room_with_path(&tmp, "original.ipynb");

    // Add a cell
    {
        let mut doc = room.doc.write().await;
        doc.add_cell(0, "cell1", "code").unwrap();
        doc.update_source("cell1", "x = 1").unwrap();
    }

    // Save to a different absolute path
    let new_path = tmp.path().join("new_location.ipynb");
    let result = save_notebook_to_disk(&room, Some(new_path.to_str().unwrap())).await;

    assert!(result.is_ok());
    let saved_path = result.unwrap();
    assert_eq!(saved_path, new_path.to_string_lossy());
    assert!(new_path.exists(), "File should be created at new path");

    // Verify content
    let content = std::fs::read_to_string(&new_path).unwrap();
    let notebook: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(notebook["cells"][0]["source"], serde_json::json!(["x = 1"]));
}

#[tokio::test]
async fn test_save_notebook_to_disk_preserves_nbformat_attachments_from_cache() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, original_path) = test_room_with_path(&tmp, "original.ipynb");

    {
        let mut doc = room.doc.write().await;
        doc.add_cell(0, "markdown-1", "markdown").unwrap();
        doc.update_source("markdown-1", "![inline](attachment:image.png)")
            .unwrap();
    }
    {
        let mut attachments = room.nbformat_attachments.write().await;
        attachments.insert(
            "markdown-1".to_string(),
            serde_json::json!({
                "image.png": {
                    "image/png": "aGVsbG8="
                }
            }),
        );
    }

    save_notebook_to_disk(&room, None).await.unwrap();

    let content = std::fs::read_to_string(&original_path).unwrap();
    let notebook: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(
        notebook["cells"][0]["attachments"],
        serde_json::json!({
            "image.png": {
                "image/png": "aGVsbG8="
            }
        })
    );
}

#[tokio::test]
async fn test_save_notebook_to_disk_preserves_raw_cell_attachments_from_cache() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, original_path) = test_room_with_path(&tmp, "raw.ipynb");

    {
        let mut doc = room.doc.write().await;
        doc.add_cell(0, "raw-1", "raw").unwrap();
        doc.update_source("raw-1", "attachment ref").unwrap();
    }
    {
        let mut attachments = room.nbformat_attachments.write().await;
        attachments.insert(
            "raw-1".to_string(),
            serde_json::json!({
                "snippet.txt": {
                    "text/plain": "hello"
                }
            }),
        );
    }

    save_notebook_to_disk(&room, None).await.unwrap();

    let content = std::fs::read_to_string(&original_path).unwrap();
    let notebook: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(
        notebook["cells"][0]["attachments"],
        serde_json::json!({
            "snippet.txt": {
                "text/plain": "hello"
            }
        })
    );
}

#[tokio::test]
async fn test_save_notebook_to_disk_appends_ipynb_extension() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _original_path) = test_room_with_path(&tmp, "original.ipynb");

    // Add a cell
    {
        let mut doc = room.doc.write().await;
        doc.add_cell(0, "cell1", "code").unwrap();
    }

    // Save to path without .ipynb extension
    let base_path = tmp.path().join("no_extension");
    let result = save_notebook_to_disk(&room, Some(base_path.to_str().unwrap())).await;

    assert!(result.is_ok());
    let saved_path = result.unwrap();
    assert!(
        saved_path.ends_with(".ipynb"),
        "Saved path should have .ipynb extension"
    );

    let expected_path = tmp.path().join("no_extension.ipynb");
    assert!(
        expected_path.exists(),
        "File should exist with .ipynb extension"
    );
}

#[tokio::test]
async fn test_save_notebook_to_disk_rejects_relative_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _original_path) = test_room_with_path(&tmp, "original.ipynb");

    // Try to save with a relative path
    let result = save_notebook_to_disk(&room, Some("relative/path.ipynb")).await;

    assert!(result.is_err());
    let error = result.unwrap_err();
    assert!(
        matches!(error, SaveError::Unrecoverable(_)),
        "Error should be unrecoverable: {}",
        error
    );
    assert!(
        error
            .to_string()
            .contains("Relative paths are not supported"),
        "Error should mention relative paths: {}",
        error
    );
}

#[tokio::test]
async fn test_format_notebook_cells_skips_unknown_runtime() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _notebook_path) = test_room_with_path(&tmp, "unknown_runtime.ipynb");

    // Add a code cell (no kernelspec metadata set = unknown runtime)
    {
        let mut doc = room.doc.write().await;
        doc.add_cell(0, "cell1", "code").unwrap();
        doc.update_source("cell1", "x=1").unwrap(); // Would be formatted if Python
    }

    // Run format - should skip (return 0) since no kernelspec
    let result = format_notebook_cells(&room).await;
    assert!(result.is_ok());
    assert_eq!(
        result.unwrap(),
        0,
        "Should format 0 cells for unknown runtime"
    );

    // Source should be unchanged
    let cells = {
        let doc = room.doc.read().await;
        doc.get_cells()
    };
    assert_eq!(cells[0].source, "x=1", "Source should remain unchanged");
}

// ========================================================================
// Tests for daemon-owned notebook loading functions (Phase 2)
// ========================================================================

#[test]
fn test_build_new_notebook_metadata_deno() {
    let metadata = build_new_notebook_metadata(
        "deno",
        "test-env-id",
        crate::settings_doc::PythonEnvType::Uv,
        None,
        &[],
    );

    assert_eq!(metadata.kernelspec.as_ref().unwrap().name, "deno");
    assert_eq!(metadata.kernelspec.as_ref().unwrap().display_name, "Deno");
    assert_eq!(
        metadata.kernelspec.as_ref().unwrap().language,
        Some("typescript".to_string())
    );
    assert_eq!(metadata.language_info.as_ref().unwrap().name, "typescript");
    assert_eq!(metadata.runt.env_id, Some("test-env-id".to_string()));
    assert!(metadata.runt.uv.is_none());
    assert!(metadata.runt.conda.is_none());
}

#[test]
fn test_build_new_notebook_metadata_python_uv() {
    let metadata = build_new_notebook_metadata(
        "python",
        "test-env-id",
        crate::settings_doc::PythonEnvType::Uv,
        None,
        &[],
    );

    assert_eq!(metadata.kernelspec.as_ref().unwrap().name, "python3");
    assert_eq!(
        metadata.kernelspec.as_ref().unwrap().display_name,
        "Python 3"
    );
    assert_eq!(
        metadata.kernelspec.as_ref().unwrap().language,
        Some("python".to_string())
    );
    assert_eq!(metadata.language_info.as_ref().unwrap().name, "python");
    assert_eq!(metadata.runt.env_id, Some("test-env-id".to_string()));
    assert!(metadata.runt.uv.is_some());
    assert!(metadata.runt.conda.is_none());
    assert!(metadata.runt.uv.as_ref().unwrap().dependencies.is_empty());
}

#[test]
fn test_build_new_notebook_metadata_python_conda() {
    let metadata = build_new_notebook_metadata(
        "python",
        "test-env-id",
        crate::settings_doc::PythonEnvType::Conda,
        None,
        &[],
    );

    assert_eq!(metadata.kernelspec.as_ref().unwrap().name, "python3");
    assert_eq!(metadata.language_info.as_ref().unwrap().name, "python");
    assert_eq!(metadata.runt.env_id, Some("test-env-id".to_string()));
    assert!(metadata.runt.uv.is_none());
    assert!(metadata.runt.conda.is_some());
    assert!(metadata
        .runt
        .conda
        .as_ref()
        .unwrap()
        .dependencies
        .is_empty());
    // Verify default channels to avoid false channel-drift detection
    assert_eq!(
        metadata.runt.conda.as_ref().unwrap().channels,
        vec!["conda-forge".to_string()]
    );
}

#[test]
fn test_create_empty_notebook_python() {
    let mut doc = NotebookDoc::new("test");
    let result = create_empty_notebook(
        &mut doc,
        "python",
        crate::settings_doc::PythonEnvType::Uv,
        None,
        None,
        &[],
    );

    assert!(result.is_ok());
    let env_id = result.unwrap();
    assert!(!env_id.is_empty(), "Should generate an env_id");

    // Should have zero cells (frontend creates the first cell locally)
    assert_eq!(doc.cell_count(), 0);
}

#[test]
fn test_create_empty_notebook_deno() {
    let mut doc = NotebookDoc::new("test");
    let result = create_empty_notebook(
        &mut doc,
        "deno",
        crate::settings_doc::PythonEnvType::Uv, // Ignored for deno
        None,
        None,
        &[],
    );

    assert!(result.is_ok());
    assert_eq!(doc.cell_count(), 0);

    // Check metadata was set correctly
    let metadata = doc.get_metadata_snapshot();
    assert!(metadata.is_some());
    let metadata = metadata.unwrap();
    assert_eq!(metadata.kernelspec.as_ref().unwrap().name, "deno");
}

#[test]
fn test_create_empty_notebook_with_provided_env_id() {
    let mut doc = NotebookDoc::new("test");
    let provided_id = "my-custom-env-id";
    let result = create_empty_notebook(
        &mut doc,
        "python",
        crate::settings_doc::PythonEnvType::Uv,
        Some(provided_id),
        None,
        &[],
    );

    assert!(result.is_ok());
    let env_id = result.unwrap();
    assert_eq!(env_id, provided_id, "Should use provided env_id");

    let metadata = doc.get_metadata_snapshot().unwrap();
    assert_eq!(
        metadata.runt.env_id,
        Some(provided_id.to_string()),
        "Metadata should have provided env_id"
    );
}

/// Benchmark streaming load phases against a real notebook.
///
/// Reads `/tmp/gelmanschools-bench.ipynb` and profiles:
/// - jiter parse time
/// - blob store output processing per batch
/// - add_cell_full per batch
/// - generate_sync_message per batch
///
/// Run with: cargo test -p runtimed -- bench_streaming_load_phases --nocapture --ignored
#[tokio::test]
#[ignore] // Only run manually — requires the fixture notebook
async fn bench_streaming_load_phases() {
    let notebook_path = std::path::Path::new("/tmp/gelmanschools-bench.ipynb");
    if !notebook_path.exists() {
        eprintln!("Skipping: /tmp/gelmanschools-bench.ipynb not found");
        eprintln!("Copy the gelmanschools notebook there first:");
        eprintln!("  cp ~/Downloads/gelmanschools/index.ipynb /tmp/gelmanschools-bench.ipynb");
        return;
    }

    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);

    // Phase 0: Read + parse
    let t0 = std::time::Instant::now();
    let bytes = std::fs::read(notebook_path).unwrap();
    let read_elapsed = t0.elapsed();

    let t_parse = std::time::Instant::now();
    let (cells, _metadata, _attachments) = parse_notebook_jiter(&bytes).unwrap();
    let parse_elapsed = t_parse.elapsed();

    eprintln!(
        "--- Notebook: {} cells, {} bytes ---",
        cells.len(),
        bytes.len()
    );
    eprintln!("  Read file:  {:?}", read_elapsed);
    eprintln!("  jiter parse: {:?}", parse_elapsed);

    // Create doc + peer state
    let mut doc = crate::notebook_doc::NotebookDoc::new("bench");
    let mut peer_state = automerge::sync::State::new();

    let batch_size = STREAMING_BATCH_SIZE;
    let mut cell_iter = cells.into_iter().enumerate().peekable();
    let mut batch_num = 0u32;

    let mut total_blob = std::time::Duration::ZERO;
    let mut total_add = std::time::Duration::ZERO;
    let mut total_sync_gen = std::time::Duration::ZERO;

    while cell_iter.peek().is_some() {
        // Blob store phase
        let t_blob = std::time::Instant::now();
        let mut batch: Vec<(usize, StreamingCell, Vec<serde_json::Value>)> = Vec::new();
        let mut batch_output_bytes = 0usize;
        for _ in 0..batch_size {
            let Some((idx, cell)) = cell_iter.next() else {
                break;
            };
            let mut output_refs = Vec::with_capacity(cell.outputs.len());
            for output in &cell.outputs {
                batch_output_bytes += output.to_string().len();
                output_refs.push(output_value_to_manifest_ref(output, &blob_store).await);
            }
            batch.push((idx, cell, output_refs));
        }
        let blob_elapsed = t_blob.elapsed();

        // add_cell_full phase
        let t_add = std::time::Instant::now();
        for (_idx, cell, _output_refs) in &batch {
            doc.add_cell_full(
                &cell.id,
                &cell.cell_type,
                &cell.position,
                &cell.source,
                &cell.execution_count,
                &cell.metadata,
            )
            .unwrap();
        }
        let add_elapsed = t_add.elapsed();

        // generate_sync_message phase
        let t_sync = std::time::Instant::now();
        let encoded = doc
            .generate_sync_message(&mut peer_state)
            .map(|m| m.encode());
        let sync_elapsed = t_sync.elapsed();
        let msg_size = encoded.as_ref().map(|e| e.len()).unwrap_or(0);

        batch_num += 1;
        eprintln!(
            "  Batch {:2} ({} cells, {:6}KB output): blob={:>8?}  add={:>8?}  sync_gen={:>8?}  msg={}KB",
            batch_num,
            batch.len(),
            batch_output_bytes / 1024,
            blob_elapsed,
            add_elapsed,
            sync_elapsed,
            msg_size / 1024,
        );

        total_blob += blob_elapsed;
        total_add += add_elapsed;
        total_sync_gen += sync_elapsed;
    }

    eprintln!("--- Totals ---");
    eprintln!("  blob store:         {:?}", total_blob);
    eprintln!("  add_cell_full:      {:?}", total_add);
    eprintln!("  generate_sync_msg:  {:?}", total_sync_gen);
    eprintln!(
        "  total (no I/O):     {:?}",
        total_blob + total_add + total_sync_gen
    );
    eprintln!("  cells: {}, batches: {}", doc.cell_count(), batch_num);
}

#[tokio::test]
async fn test_update_kernel_presence_publishes_state_and_relays() {
    let presence_state = Arc::new(RwLock::new(PresenceState::new()));
    let (presence_tx, mut presence_rx) = broadcast::channel::<(String, Vec<u8>)>(16);

    update_kernel_presence(
        &presence_state,
        &presence_tx,
        presence::KernelStatus::Idle,
        "uv:prewarmed",
    )
    .await;

    // Verify presence state contains the daemon peer with KernelState channel
    let state = presence_state.read().await;
    let peers = state.peers();
    let daemon_peer = peers.get("daemon").expect("daemon peer should exist");
    assert_eq!(daemon_peer.peer_id, "daemon");

    let kernel_channel = daemon_peer
        .channels
        .get(&presence::Channel::KernelState)
        .expect("kernel_state channel should exist");
    match kernel_channel {
        presence::ChannelData::KernelState(data) => {
            assert_eq!(data.status, presence::KernelStatus::Idle);
            assert_eq!(data.env_source, "uv:prewarmed");
        }
        other => panic!("expected KernelState, got {:?}", other),
    }
    drop(state);

    // Verify a relay frame was sent
    let (peer_id, bytes) = presence_rx
        .recv()
        .await
        .expect("should receive relay frame");
    assert_eq!(peer_id, "daemon");
    // Decode the frame to verify it's a valid KernelState update
    let msg = presence::decode_message(&bytes).expect("should decode presence message");
    match msg {
        presence::PresenceMessage::Update { peer_id, data, .. } => {
            assert_eq!(peer_id, "daemon");
            match data {
                presence::ChannelData::KernelState(data) => {
                    assert_eq!(data.status, presence::KernelStatus::Idle);
                    assert_eq!(data.env_source, "uv:prewarmed");
                }
                other => panic!("expected KernelState data, got {:?}", other),
            }
        }
        other => panic!("expected Update message, got {:?}", other),
    }
}

// ── Regression test: autosave after save_notebook path update ──────

/// Verify that saving an untitled (UUID-keyed) room updates path_index and
/// room.path, while keeping the UUID stable in the rooms map.
#[tokio::test]
async fn saving_untitled_notebook_updates_path_index_and_keeps_uuid() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);
    let docs_dir = tmp.path().join("docs");
    std::fs::create_dir_all(&docs_dir).unwrap();

    // 1. Create an ephemeral-but-persisted room (UUID, no path)
    let uuid = Uuid::new_v4();
    let room = Arc::new(NotebookRoom::new_fresh(
        uuid, None, &docs_dir, blob_store, false,
    ));
    {
        let mut doc = room.doc.write().await;
        doc.add_cell(0, "c1", "code").unwrap();
    }
    let rooms: NotebookRooms = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    rooms.lock().await.insert(uuid, room.clone());
    let path_index = Arc::new(tokio::sync::Mutex::new(PathIndex::new()));

    // 2. Simulate the handler's transition: save to disk then wire path_index.
    let save_target = tmp.path().join("note.ipynb");
    let written = save_notebook_to_disk(&room, Some(save_target.to_str().unwrap()))
        .await
        .unwrap();
    let canonical = tokio::fs::canonicalize(&written)
        .await
        .unwrap_or_else(|_| PathBuf::from(&written));

    path_index
        .lock()
        .await
        .insert(canonical.clone(), room.id)
        .unwrap();
    *room.path.write().await = Some(canonical.clone());

    // UUID key unchanged, path_index populated, room.path set.
    assert!(rooms.lock().await.contains_key(&uuid));
    assert_eq!(path_index.lock().await.lookup(&canonical), Some(uuid));
    assert_eq!(room.path.read().await.as_deref(), Some(canonical.as_path()));
}

/// Verify that `promote_untitled_to_file_backed` returns
/// `SaveErrorKind::PathAlreadyOpen` when the target path is already held by
/// another room, and does NOT mutate the fresh room's state on error.
#[tokio::test]
async fn saving_to_already_open_path_returns_path_already_open_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);
    let docs_dir = tmp.path().join("docs");
    std::fs::create_dir_all(&docs_dir).unwrap();

    // Existing room already claiming `target_path`.
    let existing_uuid = Uuid::new_v4();
    let target_path = tmp.path().join("existing.ipynb");
    let path_index = Arc::new(tokio::sync::Mutex::new(PathIndex::new()));
    path_index
        .lock()
        .await
        .insert(target_path.clone(), existing_uuid)
        .unwrap();

    // Fresh untitled room that tries to claim the same path.
    let new_uuid = Uuid::new_v4();
    let room = Arc::new(NotebookRoom::new_fresh(
        new_uuid, None, &docs_dir, blob_store, false,
    ));

    // Try to claim the path — must fail.
    let err = try_claim_path(&path_index, &target_path, new_uuid)
        .await
        .unwrap_err();

    match err {
        notebook_protocol::protocol::SaveErrorKind::PathAlreadyOpen { uuid, path: p } => {
            assert_eq!(uuid, existing_uuid.to_string());
            assert_eq!(p, target_path.to_string_lossy());
        }
        other => panic!("expected PathAlreadyOpen, got {:?}", other),
    }

    // room.path must NOT have been mutated on error.
    assert!(
        room.path.read().await.is_none(),
        "room.path should still be None after a failed claim"
    );
}

/// Regression test for the demo-day incident: when a second room tries to
/// save to a path that another room already claims, the claim check must
/// happen BEFORE any disk write. Otherwise the second room's save writes
/// 0 cells to the shared path, the first room's file watcher interprets
/// that as an external edit, and the first room's CRDT cells are wiped.
#[tokio::test]
async fn path_collision_does_not_overwrite_existing_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);
    let docs_dir = tmp.path().join("docs");
    std::fs::create_dir_all(&docs_dir).unwrap();

    // Room A claims the path; write a known marker payload to disk.
    let target_path = tmp.path().join("shared.ipynb");
    let marker_content = r#"{"cells":[{"cell_type":"code","source":"x = 1"}],"metadata":{},"nbformat":4,"nbformat_minor":5}"#;
    tokio::fs::write(&target_path, marker_content)
        .await
        .unwrap();

    // Canonicalize before inserting so the key matches what the handler
    // would compute via canonical_target_path at save time.
    let canonical = canonical_target_path(&target_path).await;
    let path_index = Arc::new(tokio::sync::Mutex::new(PathIndex::new()));
    let uuid_a = Uuid::new_v4();
    path_index
        .lock()
        .await
        .insert(canonical.clone(), uuid_a)
        .unwrap();

    // Room B attempts to save to the same path. Per the handler's
    // claim-before-write ordering, it must fail at try_claim_path without
    // ever invoking save_notebook_to_disk.
    let uuid_b = Uuid::new_v4();
    let _room_b = Arc::new(NotebookRoom::new_fresh(
        uuid_b, None, &docs_dir, blob_store, false,
    ));
    let claim = try_claim_path(&path_index, &canonical, uuid_b).await;
    assert!(claim.is_err(), "claim must fail on collision");

    // Target file must be byte-for-byte identical.
    let on_disk = tokio::fs::read_to_string(&target_path).await.unwrap();
    assert_eq!(
        on_disk, marker_content,
        "collision attempt must not touch the file on disk"
    );
}

/// Verify the full lifecycle: create untitled room → save to disk →
/// promote via `promote_untitled_to_file_backed` → edit → autosave flushes
/// the edit to the .ipynb file.
///
/// This test calls the production helper directly, so it validates the real
/// code path rather than an inline copy of the transition logic.
#[tokio::test(start_paused = true)]
async fn test_promote_untitled_starts_autosave() {
    use std::time::Duration;

    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);
    let docs_dir = tmp.path().join("docs");
    std::fs::create_dir_all(&docs_dir).unwrap();

    // 1. Create an untitled (UUID-keyed) room with one cell.
    let uuid_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let uuid = Uuid::parse_str(uuid_id).unwrap();
    let room = Arc::new(NotebookRoom::new_fresh(
        uuid, None, &docs_dir, blob_store, false,
    ));
    assert!(is_untitled_notebook(uuid_id));

    {
        let mut doc = room.doc.write().await;
        doc.add_cell(0, "cell-1", "code").unwrap();
        doc.update_source("cell-1", "x = 1").unwrap();
    }

    // 2. Insert into rooms map under UUID key (UUID key stays constant).
    let rooms: NotebookRooms = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    rooms.lock().await.insert(uuid, room.clone());

    // 3. Save to disk — creates the .ipynb file.
    let save_path = tmp.path().join("saved.ipynb");
    let written = save_notebook_to_disk(&room, Some(save_path.to_str().unwrap()))
        .await
        .unwrap();
    assert!(save_path.exists());

    // 4. Promote the room using the production helper.
    let canonical = tokio::fs::canonicalize(&written)
        .await
        .unwrap_or_else(|_| PathBuf::from(&written));
    let path_index = Arc::new(tokio::sync::Mutex::new(PathIndex::new()));

    try_claim_path(&path_index, &canonical, room.id)
        .await
        .expect("path claim should succeed");
    finalize_untitled_promotion(&room, canonical.clone()).await;

    // Verify post-promotion state.
    assert!(
        rooms.lock().await.contains_key(&uuid),
        "UUID key should still be present after promotion"
    );
    assert_eq!(
        room.path.read().await.as_deref(),
        Some(canonical.as_path()),
        "room.path should be set after promotion"
    );
    assert_eq!(
        path_index.lock().await.lookup(&canonical),
        Some(uuid),
        "path_index should contain the room's UUID"
    );
    assert!(
        !room.is_ephemeral.load(Ordering::Relaxed),
        "is_ephemeral should be cleared after promotion"
    );

    // 5. Add a new cell AFTER promotion (simulates MCP create_cell).
    {
        let mut doc = room.doc.write().await;
        doc.add_cell(1, "cell-2", "code").unwrap();
        doc.update_source("cell-2", "y = 2").unwrap();
    }
    let _ = room.changed_tx.send(());

    // 6. Poll until the autosave debouncer flushes both cells to disk.
    //    Each sleep(100ms) advances the paused clock and yields to the
    //    runtime, letting the debouncer make progress. Timeout after 10s
    //    (well beyond the 2s debounce + 500ms check interval defaults).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let nb = loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let content = tokio::fs::read_to_string(&save_path).await.unwrap();
        let nb: serde_json::Value = serde_json::from_str(&content).unwrap();
        if nb["cells"].as_array().is_some_and(|c| c.len() == 2) {
            break nb;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Timed out waiting for autosave to flush both cells; got: {}",
            serde_json::to_string_pretty(&nb["cells"]).unwrap()
        );
    };

    // 7. Verify the post-promotion cell's source is present.
    let cells = nb["cells"].as_array().unwrap();
    let sources: Vec<String> = cells
        .iter()
        .map(|c| match &c["source"] {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(arr) => arr
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(""),
            _ => String::new(),
        })
        .collect();
    assert!(
        sources.iter().any(|s| s.contains("y = 2")),
        "Post-promotion cell should be persisted; sources: {:?}",
        sources
    );
}

// ── find_room_by_path tests ───────────────────────────────────────────

#[tokio::test]
async fn find_room_by_path_returns_room_after_index_insert() {
    let tmp = tempfile::tempdir().unwrap();
    let blob_store = test_blob_store(&tmp);
    let rooms: NotebookRooms = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let path_index = Arc::new(tokio::sync::Mutex::new(PathIndex::new()));
    let uuid = Uuid::new_v4();
    let fake = tmp.path().join("note.ipynb");
    let room = Arc::new(NotebookRoom::new_fresh(
        uuid,
        Some(fake.clone()),
        tmp.path(),
        blob_store,
        false,
    ));
    rooms.lock().await.insert(uuid, room.clone());
    path_index.lock().await.insert(fake.clone(), uuid).unwrap();

    let found = find_room_by_path(&rooms, &path_index, &fake).await;
    assert!(found.is_some());
    assert!(Arc::ptr_eq(&found.unwrap(), &room));
}

#[tokio::test]
async fn find_room_by_path_returns_none_when_not_indexed() {
    let tmp = tempfile::tempdir().unwrap();
    let rooms: NotebookRooms = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let path_index = Arc::new(tokio::sync::Mutex::new(PathIndex::new()));
    let found = find_room_by_path(&rooms, &path_index, &tmp.path().join("nope.ipynb")).await;
    assert!(found.is_none());
}

// ── C1 regression: NotebookSync path handshake must reuse existing room ──

/// Verify that the pattern used by the NotebookSync handshake — consulting
/// `find_room_by_path` before calling `get_or_create_room` — produces
/// exactly one room for a given path even when called twice.
///
/// Before the C1 fix the handshake would mint a fresh UUID on every call,
/// so a second connection to the same path created a second room (zombie
/// room: two file watchers, two autosave debouncers, two writers).
///
/// The fix: if `find_room_by_path` returns `Some(existing)`, reuse its UUID
/// so `get_or_create_room` returns the existing room instead of creating one.
#[tokio::test]
async fn test_notebook_sync_path_handshake_reuses_existing_room() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);
    let docs_dir = tmp.path().to_path_buf();
    let rooms: NotebookRooms = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let path_index = Arc::new(tokio::sync::Mutex::new(PathIndex::new()));

    // Simulate a file-backed path (doesn't need to exist for this test).
    let notebook_path = tmp.path().join("my_notebook.ipynb");

    // --- First handshake (simulates the fixed NotebookSync path) ---
    // 1. Check path_index — not yet indexed, so mint a new UUID.
    let room1 = {
        let (uuid, path) = match find_room_by_path(&rooms, &path_index, &notebook_path).await {
            Some(existing) => (existing.id, Some(notebook_path.clone())),
            None => (Uuid::new_v4(), Some(notebook_path.clone())),
        };
        get_or_create_room(
            &rooms,
            &path_index,
            uuid,
            path,
            &docs_dir,
            blob_store.clone(),
            false,
        )
        .await
    };

    // --- Second handshake for the same path ---
    // find_room_by_path should now return the existing room.
    let room2 = {
        let (uuid, path) = match find_room_by_path(&rooms, &path_index, &notebook_path).await {
            Some(existing) => (existing.id, Some(notebook_path.clone())),
            None => (Uuid::new_v4(), Some(notebook_path.clone())),
        };
        get_or_create_room(
            &rooms,
            &path_index,
            uuid,
            path,
            &docs_dir,
            blob_store.clone(),
            false,
        )
        .await
    };

    // Both handshakes must return the same room Arc — no zombie duplicates.
    assert!(
        Arc::ptr_eq(&room1, &room2),
        "Second NotebookSync handshake for same path must reuse existing room"
    );

    // Exactly one room in the map (not two).
    assert_eq!(
        rooms.lock().await.len(),
        1,
        "Only one room should exist after two handshakes for the same path"
    );

    // path_index has exactly one entry.
    assert_eq!(
        path_index.lock().await.len(),
        1,
        "path_index should have exactly one entry"
    );
}

// ── compute_env_sync_diff tests ───────────────────────────────────────

#[test]
fn test_compute_env_sync_diff_in_sync() {
    let launched = LaunchedEnvConfig {
        uv_deps: Some(vec!["numpy".to_string(), "pandas".to_string()]),
        conda_deps: None,
        conda_channels: None,
        pixi_deps: None,
        pixi_toml_deps: None,
        pixi_toml_path: None,
        pyproject_path: None,
        environment_yml_path: None,
        environment_yml_deps: None,
        deno_config: None,
        venv_path: None,
        python_path: None,
        launch_id: Some("abc".to_string()),
        feature_flags: notebook_protocol::protocol::FeatureFlags::default(),
        prewarmed_packages: vec![],
    };
    let snapshot = snapshot_with_uv(vec!["numpy".to_string(), "pandas".to_string()]);
    assert!(
        compute_env_sync_diff(&launched, &snapshot).is_none(),
        "identical deps should be in sync"
    );
}

#[test]
fn test_compute_env_sync_diff_added() {
    let launched = LaunchedEnvConfig {
        uv_deps: Some(vec!["numpy".to_string()]),
        conda_deps: None,
        conda_channels: None,
        pixi_deps: None,
        pixi_toml_deps: None,
        pixi_toml_path: None,
        pyproject_path: None,
        environment_yml_path: None,
        environment_yml_deps: None,
        deno_config: None,
        venv_path: None,
        python_path: None,
        launch_id: None,
        feature_flags: notebook_protocol::protocol::FeatureFlags::default(),
        prewarmed_packages: vec![],
    };
    let snapshot = snapshot_with_uv(vec!["numpy".to_string(), "requests".to_string()]);
    let diff = compute_env_sync_diff(&launched, &snapshot).expect("should detect drift");
    assert_eq!(diff.added, vec!["requests".to_string()]);
    assert!(diff.removed.is_empty());
    assert!(!diff.channels_changed);
}

#[test]
fn test_compute_env_sync_diff_removed() {
    let launched = LaunchedEnvConfig {
        uv_deps: Some(vec!["numpy".to_string(), "pandas".to_string()]),
        conda_deps: None,
        conda_channels: None,
        pixi_deps: None,
        pixi_toml_deps: None,
        pixi_toml_path: None,
        pyproject_path: None,
        environment_yml_path: None,
        environment_yml_deps: None,
        deno_config: None,
        venv_path: None,
        python_path: None,
        launch_id: None,
        feature_flags: notebook_protocol::protocol::FeatureFlags::default(),
        prewarmed_packages: vec![],
    };
    let snapshot = snapshot_with_uv(vec!["numpy".to_string()]);
    let diff = compute_env_sync_diff(&launched, &snapshot).expect("should detect drift");
    assert!(diff.added.is_empty());
    assert_eq!(diff.removed, vec!["pandas".to_string()]);
}

#[test]
fn test_compute_env_sync_diff_added_and_removed() {
    let launched = LaunchedEnvConfig {
        uv_deps: Some(vec!["numpy".to_string(), "old-pkg".to_string()]),
        conda_deps: None,
        conda_channels: None,
        pixi_deps: None,
        pixi_toml_deps: None,
        pixi_toml_path: None,
        pyproject_path: None,
        environment_yml_path: None,
        environment_yml_deps: None,
        deno_config: None,
        venv_path: None,
        python_path: None,
        launch_id: None,
        feature_flags: notebook_protocol::protocol::FeatureFlags::default(),
        prewarmed_packages: vec![],
    };
    let snapshot = snapshot_with_uv(vec!["numpy".to_string(), "new-pkg".to_string()]);
    let diff = compute_env_sync_diff(&launched, &snapshot).expect("should detect drift");
    assert_eq!(diff.added, vec!["new-pkg".to_string()]);
    assert_eq!(diff.removed, vec!["old-pkg".to_string()]);
}

#[test]
fn test_compute_env_sync_diff_conda_channels_changed() {
    let launched = LaunchedEnvConfig {
        uv_deps: None,
        conda_deps: Some(vec!["scipy".to_string()]),
        conda_channels: Some(vec!["conda-forge".to_string()]),
        pixi_deps: None,
        pixi_toml_deps: None,
        pixi_toml_path: None,
        pyproject_path: None,
        environment_yml_path: None,
        environment_yml_deps: None,
        deno_config: None,
        venv_path: None,
        python_path: None,
        launch_id: None,
        feature_flags: notebook_protocol::protocol::FeatureFlags::default(),
        prewarmed_packages: vec![],
    };
    // Build a conda snapshot with a different channel
    let mut snapshot = snapshot_with_conda(vec!["scipy".to_string()]);
    snapshot.runt.conda.as_mut().unwrap().channels = vec!["defaults".to_string()];
    let diff = compute_env_sync_diff(&launched, &snapshot).expect("should detect channel drift");
    assert!(diff.added.is_empty());
    assert!(diff.removed.is_empty());
    assert!(diff.channels_changed);
}

#[test]
fn test_compute_env_sync_diff_no_tracking() {
    // Prewarmed kernel: no uv_deps, no conda_deps, no deno_config
    let launched = LaunchedEnvConfig::default();
    let snapshot = snapshot_with_uv(vec!["numpy".to_string()]);
    // When the kernel isn't tracking any deps, diff is None (no drift to report)
    assert!(compute_env_sync_diff(&launched, &snapshot).is_none());
}

#[test]
fn test_build_launched_config_uv_prewarmed_stores_paths() {
    let venv = PathBuf::from("/tmp/pool/env-abc");
    let python = PathBuf::from("/tmp/pool/env-abc/bin/python");
    let pkgs = vec!["ipykernel".to_string(), "pandas".to_string()];
    let config = build_launched_config(
        "python",
        "uv:prewarmed",
        None,
        None,
        Some(venv.clone()),
        Some(python.clone()),
        Some(&pkgs),
        None,
        notebook_protocol::protocol::FeatureFlags::default(),
        None,
    );
    assert_eq!(config.venv_path.as_ref(), Some(&venv));
    assert_eq!(config.python_path.as_ref(), Some(&python));
    assert!(config.uv_deps.is_none(), "prewarmed should not set uv_deps");
    assert_eq!(config.prewarmed_packages, pkgs);
}

#[test]
fn test_build_launched_config_uv_prewarmed_with_captured_baseline() {
    // P3 regression: when a captured env fires the prewarmed path,
    // launched_config must record captured deps as the baseline so
    // drift detection treats the launch as "tracking" rather than
    // reporting captured deps as pending additions on every reopen.
    let captured = CapturedEnv::Uv {
        deps: kernel_env::UvDependencies {
            dependencies: vec!["pandas".to_string(), "numpy".to_string()],
            requires_python: Some(">=3.10".to_string()),
            prerelease: None,
        },
        env_id: "nb-1".to_string(),
    };
    let config = build_launched_config(
        "python",
        "uv:prewarmed",
        None,
        None,
        Some(PathBuf::from("/tmp/env")),
        Some(PathBuf::from("/tmp/env/bin/python")),
        None,
        None,
        notebook_protocol::protocol::FeatureFlags::default(),
        Some(&captured),
    );
    assert_eq!(
        config.uv_deps.as_deref(),
        Some(["pandas".to_string(), "numpy".to_string()].as_slice()),
        "captured-prewarmed must record deps as baseline"
    );
}

#[test]
fn test_build_launched_config_conda_prewarmed_with_captured_baseline() {
    // Captured conda baseline must include channels so channel edits
    // surface as drift rather than being silently ignored.
    let captured = CapturedEnv::Conda {
        deps: kernel_env::CondaDependencies {
            dependencies: vec!["scipy".to_string()],
            channels: vec!["conda-forge".to_string(), "pytorch".to_string()],
            python: Some("3.11".to_string()),
            env_id: None,
        },
        env_id: "nb-2".to_string(),
    };
    let config = build_launched_config(
        "python",
        "conda:prewarmed",
        None,
        None,
        Some(PathBuf::from("/tmp/conda-env")),
        Some(PathBuf::from("/tmp/conda-env/bin/python")),
        None,
        None,
        notebook_protocol::protocol::FeatureFlags::default(),
        Some(&captured),
    );
    assert_eq!(
        config.conda_deps.as_deref(),
        Some([String::from("scipy")].as_slice())
    );
    assert_eq!(
        config.conda_channels.as_deref(),
        Some(["conda-forge".to_string(), "pytorch".to_string()].as_slice())
    );
}

#[test]
fn test_compute_env_sync_diff_prewarmed_promoted_to_empty_baseline() {
    // Simulates handle_sync_environment promoting uv_deps from None to
    // Some([]) for a prewarmed kernel, then computing the diff.
    let mut launched = LaunchedEnvConfig {
        venv_path: Some(PathBuf::from("/tmp/pool/env-abc")),
        python_path: Some(PathBuf::from("/tmp/pool/env-abc/bin/python")),
        ..LaunchedEnvConfig::default()
    };
    // Promote to empty baseline (what handle_sync_environment does)
    launched.uv_deps = Some(vec![]);

    let snapshot = snapshot_with_uv(vec!["httpx".to_string()]);
    let diff = compute_env_sync_diff(&launched, &snapshot).expect("should detect added deps");
    assert_eq!(diff.added, vec!["httpx".to_string()]);
    assert!(diff.removed.is_empty());
}

#[test]
fn test_build_launched_config_conda_prewarmed_stores_paths() {
    // conda:prewarmed stores paths so hot-sync can install deps later
    let venv = PathBuf::from("/tmp/conda-env");
    let python = PathBuf::from("/tmp/conda-env/bin/python");
    let config = build_launched_config(
        "python",
        "conda:prewarmed",
        None,
        None,
        Some(venv.clone()),
        Some(python.clone()),
        None,
        None,
        notebook_protocol::protocol::FeatureFlags::default(),
        None,
    );
    assert_eq!(config.venv_path.as_ref(), Some(&venv));
    assert_eq!(config.python_path.as_ref(), Some(&python));
    assert!(config.uv_deps.is_none());
    assert!(
        config.conda_deps.is_none(),
        "prewarmed should not set conda_deps"
    );
}

// ── check_and_broadcast_sync_state tests ──────────────────────────────

#[tokio::test]
async fn test_check_and_broadcast_sync_state_no_kernel() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _path) = test_room_with_path(&tmp, "no_kernel.ipynb");

    // Write metadata so the function gets past the metadata check
    let snapshot = snapshot_with_uv(vec!["numpy".to_string()]);
    {
        let mut doc = room.doc.write().await;
        doc.set_metadata_snapshot(&snapshot).unwrap();
    }

    // Pre-set RuntimeStateDoc env to dirty so we can verify it's NOT changed
    room.state
        .with_doc(|sd| sd.set_env_sync(false, &["numpy".to_string()], &[], false, false))
        .unwrap();

    // No kernel in the room — should be a no-op
    check_and_broadcast_sync_state(&room).await;

    // Verify env state was NOT touched (still dirty from pre-set)
    let state = room.state.read(|sd| sd.read_state()).unwrap();
    assert!(
        !state.env.in_sync,
        "env should remain dirty when no kernel is present"
    );
    assert_eq!(state.env.added, vec!["numpy".to_string()]);
}

/// P3 regression: a captured-prewarmed launch must report `in_sync = true`
/// when metadata matches the captured baseline. Before the fix,
/// `LaunchedEnvConfig.uv_deps` was left `None` for the prewarmed path, so
/// `check_and_broadcast_sync_state` took the "prewarmed + inline deps
/// added" branch and flagged the captured deps as pending additions on
/// every reopen.
#[tokio::test]
async fn test_check_and_broadcast_sync_state_captured_uv_prewarmed_in_sync() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _path) = test_room_with_path(&tmp, "captured.ipynb");

    // Notebook has captured deps in metadata.
    let snapshot = snapshot_with_uv(vec!["pandas".to_string(), "numpy".to_string()]);
    {
        let mut doc = room.doc.write().await;
        doc.set_metadata_snapshot(&snapshot).unwrap();
    }

    // Kernel was launched via the captured-prewarmed path, so launched
    // config records the captured deps as the baseline (what our P3 fix
    // does in `build_launched_config`).
    {
        let mut lc = room.runtime_agent_launched_config.write().await;
        *lc = Some(LaunchedEnvConfig {
            uv_deps: Some(vec!["pandas".to_string(), "numpy".to_string()]),
            venv_path: Some(PathBuf::from("/tmp/captured-env")),
            python_path: Some(PathBuf::from("/tmp/captured-env/bin/python")),
            ..LaunchedEnvConfig::default()
        });
    }

    // Kernel is idle (otherwise the function returns early).
    {
        room.state
            .with_doc(|sd| {
                sd.set_kernel_status("idle")?;
                // Pre-set to dirty so we can verify it flips to in_sync.
                sd.set_env_sync(false, &["pandas".to_string()], &[], false, false)?;
                Ok(())
            })
            .unwrap();
    }

    check_and_broadcast_sync_state(&room).await;

    let state = room.state.read(|sd| sd.read_state()).unwrap();
    assert!(
        state.env.in_sync,
        "captured-prewarmed launch with matching metadata must be in_sync"
    );
    assert!(state.env.added.is_empty());
    assert!(state.env.removed.is_empty());
}

/// Complementary to the above: when metadata diverges from the captured
/// baseline (user added a new dep post-capture), the drift detector
/// should surface the new dep in `env.added`. This verifies drift still
/// works when the captured baseline is populated.
#[tokio::test]
async fn test_check_and_broadcast_sync_state_captured_uv_prewarmed_reports_additions() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _path) = test_room_with_path(&tmp, "captured-drift.ipynb");

    // User added a third dep post-capture.
    let snapshot = snapshot_with_uv(vec![
        "pandas".to_string(),
        "numpy".to_string(),
        "polars".to_string(),
    ]);
    {
        let mut doc = room.doc.write().await;
        doc.set_metadata_snapshot(&snapshot).unwrap();
    }

    // Launched baseline still only has the original captured set.
    {
        let mut lc = room.runtime_agent_launched_config.write().await;
        *lc = Some(LaunchedEnvConfig {
            uv_deps: Some(vec!["pandas".to_string(), "numpy".to_string()]),
            venv_path: Some(PathBuf::from("/tmp/captured-env")),
            python_path: Some(PathBuf::from("/tmp/captured-env/bin/python")),
            ..LaunchedEnvConfig::default()
        });
    }

    {
        room.state
            .with_doc(|sd| sd.set_kernel_status("idle"))
            .unwrap();
    }

    check_and_broadcast_sync_state(&room).await;

    let state = room.state.read(|sd| sd.read_state()).unwrap();
    assert!(
        !state.env.in_sync,
        "added dep post-capture must surface as drift"
    );
    assert_eq!(state.env.added, vec!["polars".to_string()]);
}

#[tokio::test]
async fn test_check_and_broadcast_sync_state_no_metadata() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _path) = test_room_with_path(&tmp, "no_meta.ipynb");

    // Don't write any metadata to the doc

    // Pre-set RuntimeStateDoc env to dirty
    {
        room.state
            .with_doc(|sd| sd.set_env_sync(false, &["pandas".to_string()], &[], false, false))
            .unwrap();
    }

    // No metadata in doc — should return early
    check_and_broadcast_sync_state(&room).await;

    // Verify env state was NOT touched
    let state = room.state.read(|sd| sd.read_state()).unwrap();
    assert!(
        !state.env.in_sync,
        "env should remain dirty when no metadata is present"
    );
}

// ── verify_trust_from_snapshot tests ───────────────────────────────────

#[test]
fn test_verify_trust_from_snapshot_no_deps() {
    let snapshot = snapshot_empty();
    let result = verify_trust_from_snapshot(&snapshot);
    assert_eq!(result.status, runt_trust::TrustStatus::NoDependencies);
    assert!(!result.pending_launch);
}

#[test]
#[serial]
fn test_verify_trust_from_snapshot_unsigned_deps() {
    let temp_dir = tempfile::tempdir().unwrap();
    let key_path = temp_dir.path().join("trust-key");
    std::env::set_var("RUNT_TRUST_KEY_PATH", key_path.to_str().unwrap());

    let snapshot = snapshot_with_uv(vec!["numpy".to_string()]);
    let result = verify_trust_from_snapshot(&snapshot);
    assert_eq!(result.status, runt_trust::TrustStatus::Untrusted);
    assert!(!result.pending_launch);

    std::env::remove_var("RUNT_TRUST_KEY_PATH");
}

#[test]
#[serial]
fn test_verify_trust_from_snapshot_signed_trusted() {
    let temp_dir = tempfile::tempdir().unwrap();
    let key_path = temp_dir.path().join("trust-key");
    std::env::set_var("RUNT_TRUST_KEY_PATH", key_path.to_str().unwrap());

    let mut snapshot = snapshot_with_uv(vec!["numpy".to_string()]);

    // Build the same HashMap that verify_trust_from_snapshot builds, then sign.
    let mut metadata = std::collections::HashMap::new();
    if let Ok(runt_value) = serde_json::to_value(&snapshot.runt) {
        metadata.insert("runt".to_string(), runt_value);
    }
    let signature = runt_trust::sign_notebook_dependencies(&metadata).unwrap();
    snapshot.runt.trust_signature = Some(signature);

    let result = verify_trust_from_snapshot(&snapshot);
    assert_eq!(result.status, runt_trust::TrustStatus::Trusted);
    assert!(!result.pending_launch);

    std::env::remove_var("RUNT_TRUST_KEY_PATH");
}

#[test]
#[serial]
fn test_verify_trust_from_snapshot_invalid_signature() {
    let temp_dir = tempfile::tempdir().unwrap();
    let key_path = temp_dir.path().join("trust-key");
    std::env::set_var("RUNT_TRUST_KEY_PATH", key_path.to_str().unwrap());

    let mut snapshot = snapshot_with_uv(vec!["numpy".to_string()]);
    // Set a bogus signature that won't match.
    snapshot.runt.trust_signature = Some("bad-signature-value".to_string());

    let result = verify_trust_from_snapshot(&snapshot);
    assert_eq!(result.status, runt_trust::TrustStatus::SignatureInvalid);
    assert!(!result.pending_launch);

    std::env::remove_var("RUNT_TRUST_KEY_PATH");
}

#[test]
#[serial]
fn test_verify_trust_from_snapshot_conda_trusted() {
    let temp_dir = tempfile::tempdir().unwrap();
    let key_path = temp_dir.path().join("trust-key");
    std::env::set_var("RUNT_TRUST_KEY_PATH", key_path.to_str().unwrap());

    let mut snapshot = snapshot_with_conda(vec!["pandas".to_string()]);

    // Build the same HashMap that verify_trust_from_snapshot builds, then sign.
    let mut metadata = std::collections::HashMap::new();
    if let Ok(runt_value) = serde_json::to_value(&snapshot.runt) {
        metadata.insert("runt".to_string(), runt_value);
    }
    let signature = runt_trust::sign_notebook_dependencies(&metadata).unwrap();
    snapshot.runt.trust_signature = Some(signature);

    let result = verify_trust_from_snapshot(&snapshot);
    assert_eq!(result.status, runt_trust::TrustStatus::Trusted);
    assert!(!result.pending_launch);

    std::env::remove_var("RUNT_TRUST_KEY_PATH");
}

#[tokio::test]
async fn test_check_and_update_trust_state_empty_doc() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _path) = test_room_with_path(&tmp, "empty_doc.ipynb");

    // Doc has no metadata written — should not crash.
    check_and_update_trust_state(&room).await;

    // trust_state should remain Untrusted (the default from test_room_with_path).
    let ts = room.trust_state.read().await;
    assert_eq!(ts.status, runt_trust::TrustStatus::Untrusted);
}

#[tokio::test]
async fn test_check_and_update_trust_state_no_deps() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _path) = test_room_with_path(&tmp, "no_deps.ipynb");

    // Align RuntimeStateDoc with the room's initial Untrusted state so we
    // can verify the function actually writes the new value.
    {
        room.state
            .with_doc(|sd| sd.set_trust("untrusted", true))
            .unwrap();
    }

    // Write an empty metadata snapshot (no dependencies).
    let snapshot = snapshot_empty();
    {
        let mut doc = room.doc.write().await;
        doc.set_metadata_snapshot(&snapshot).unwrap();
    }

    check_and_update_trust_state(&room).await;

    // Room trust_state should change from Untrusted → NoDependencies.
    let ts = room.trust_state.read().await;
    assert_eq!(ts.status, runt_trust::TrustStatus::NoDependencies);
    drop(ts);

    // RuntimeStateDoc should reflect "no_dependencies" with needs_approval=false.
    let state = room.state.read(|sd| sd.read_state()).unwrap();
    assert_eq!(state.trust.status, "no_dependencies");
    assert!(!state.trust.needs_approval);
}

#[tokio::test]
#[serial]
async fn test_check_and_update_trust_state_approval_updates_room() {
    let temp_dir = tempfile::tempdir().unwrap();
    let key_path = temp_dir.path().join("trust-key");
    std::env::set_var("RUNT_TRUST_KEY_PATH", key_path.to_str().unwrap());

    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _path) = test_room_with_path(&tmp, "signed.ipynb");

    // Align RuntimeStateDoc with the room's initial Untrusted state.
    {
        room.state
            .with_doc(|sd| sd.set_trust("untrusted", true))
            .unwrap();
    }

    // Build a snapshot with UV deps and a valid trust signature.
    let mut snapshot = snapshot_with_uv(vec!["numpy".to_string()]);
    let mut metadata = std::collections::HashMap::new();
    if let Ok(runt_value) = serde_json::to_value(&snapshot.runt) {
        metadata.insert("runt".to_string(), runt_value);
    }
    let signature = runt_trust::sign_notebook_dependencies(&metadata).unwrap();
    snapshot.runt.trust_signature = Some(signature);

    {
        let mut doc = room.doc.write().await;
        doc.set_metadata_snapshot(&snapshot).unwrap();
    }

    check_and_update_trust_state(&room).await;

    // Room trust_state should be Trusted.
    let ts = room.trust_state.read().await;
    assert_eq!(ts.status, runt_trust::TrustStatus::Trusted);
    drop(ts);

    // RuntimeStateDoc should have "trusted" with needs_approval=false.
    let state = room.state.read(|sd| sd.read_state()).unwrap();
    assert_eq!(state.trust.status, "trusted");
    assert!(!state.trust.needs_approval);

    std::env::remove_var("RUNT_TRUST_KEY_PATH");
}

#[tokio::test]
async fn test_check_and_update_trust_state_idempotent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _path) = test_room_with_path(&tmp, "idempotent.ipynb");

    // Align RuntimeStateDoc with the room's initial Untrusted state so the
    // first transition to NoDependencies actually mutates the doc and fires
    // a notification.
    {
        room.state
            .with_doc(|sd| sd.set_trust("untrusted", true))
            .unwrap();
    }

    // Write an empty metadata snapshot to trigger Untrusted → NoDependencies.
    let snapshot = snapshot_empty();
    {
        let mut doc = room.doc.write().await;
        doc.set_metadata_snapshot(&snapshot).unwrap();
    }

    // Subscribe before either call so we capture all notifications.
    let mut rx = room.state.subscribe();

    // First call: state changes from Untrusted → NoDependencies → notification sent.
    check_and_update_trust_state(&room).await;

    // Second call: state is already NoDependencies → no change, no notification.
    check_and_update_trust_state(&room).await;

    // Drain the channel and count how many notifications arrived.
    let mut count = 0usize;
    while rx.try_recv().is_ok() {
        count += 1;
    }
    assert_eq!(count, 1, "expected exactly one state_changed notification");

    // Final trust_state should be NoDependencies.
    let ts = room.trust_state.read().await;
    assert_eq!(ts.status, runt_trust::TrustStatus::NoDependencies);
}

/// Simulates the file-watcher path: a notebook starts Trusted (signed
/// metadata on disk), then an external editor / `uv add numpy` rewrites
/// the dependency list. The signature no longer covers the new dep set,
/// so `check_and_update_trust_state` should flip trust_state to
/// SignatureInvalid and emit a state_changed_tx notification.
///
/// This is the regression this PR fixes: before the fix the file watcher
/// merged new deps into the CRDT but never re-verified trust, so
/// room.trust_state stayed Trusted and auto-launch used a stale signature.
#[tokio::test]
#[serial]
async fn test_check_and_update_trust_state_external_dep_add_invalidates() {
    let temp_dir = tempfile::tempdir().unwrap();
    let key_path = temp_dir.path().join("trust-key");
    std::env::set_var("RUNT_TRUST_KEY_PATH", key_path.to_str().unwrap());

    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _path) = test_room_with_path(&tmp, "signed_then_edited.ipynb");

    // Build a signed snapshot (numpy only) and seed the room with Trusted
    // state, matching what happens at room creation time on disk.
    let mut signed = snapshot_with_uv(vec!["numpy".to_string()]);
    let mut metadata = std::collections::HashMap::new();
    if let Ok(runt_value) = serde_json::to_value(&signed.runt) {
        metadata.insert("runt".to_string(), runt_value);
    }
    let signature = runt_trust::sign_notebook_dependencies(&metadata).unwrap();
    signed.runt.trust_signature = Some(signature.clone());

    {
        let mut doc = room.doc.write().await;
        doc.set_metadata_snapshot(&signed).unwrap();
    }
    {
        let mut ts = room.trust_state.write().await;
        *ts = verify_trust_from_snapshot(&signed);
    }
    {
        room.state
            .with_doc(|sd| sd.set_trust("trusted", false))
            .unwrap();
    }

    // Sanity check: starting state is Trusted.
    {
        let ts = room.trust_state.read().await;
        assert_eq!(ts.status, runt_trust::TrustStatus::Trusted);
    }

    // Simulate external edit: user runs `uv add pandas` + saves. The
    // file watcher merges the new deps into the CRDT but carries over
    // the stale signature (because the external tool doesn't resign).
    let mut edited = snapshot_with_uv(vec!["numpy".to_string(), "pandas".to_string()]);
    edited.runt.trust_signature = Some(signature);
    {
        let mut doc = room.doc.write().await;
        doc.set_metadata_snapshot(&edited).unwrap();
    }

    // Subscribe before the re-verification so we can observe the flip.
    let mut rx = room.state.subscribe();

    check_and_update_trust_state(&room).await;

    // Trust should flip to SignatureInvalid — the signature is over the
    // numpy-only dep set, so adding pandas breaks it.
    {
        let ts = room.trust_state.read().await;
        assert_eq!(
            ts.status,
            runt_trust::TrustStatus::SignatureInvalid,
            "external dep add must flip trust from Trusted to SignatureInvalid"
        );
    }

    // RuntimeStateDoc should reflect the flip for the frontend banner.
    {
        let state = room.state.read(|sd| sd.read_state()).unwrap();
        assert_eq!(state.trust.status, "signature_invalid");
        assert!(state.trust.needs_approval);
    }

    // state_changed_tx must have fired at least once so subscribers
    // (frontend, auto-launch) pick up the new trust state.
    assert!(
        rx.try_recv().is_ok(),
        "trust flip must emit state_changed_tx notification"
    );

    std::env::remove_var("RUNT_TRUST_KEY_PATH");
}

// ── Per-agent oneshot channel tests ──────────────────────────────

#[tokio::test]
async fn test_per_runtime_agent_oneshot_isolation() {
    // Verify that each spawn generation gets its own oneshot channel
    // and that connecting one agent doesn't resolve another's receiver.
    let pending: Arc<Mutex<Option<oneshot::Sender<()>>>> = Arc::new(Mutex::new(None));

    // Spawn A: create oneshot, store sender
    let (tx_a, rx_a) = oneshot::channel();
    *pending.lock().await = Some(tx_a);

    // A connects: take and send
    if let Some(tx) = pending.lock().await.take() {
        tx.send(()).unwrap();
    }
    assert!(rx_a.await.is_ok(), "A's receiver should resolve Ok");

    // Spawn B: create new oneshot (A's sender already consumed via take)
    let (tx_b, rx_b) = oneshot::channel();
    *pending.lock().await = Some(tx_b);

    // B connects
    if let Some(tx) = pending.lock().await.take() {
        tx.send(()).unwrap();
    }
    assert!(rx_b.await.is_ok(), "B's receiver should resolve Ok");

    // After both consumed, pending should be None
    assert!(pending.lock().await.is_none());
}

#[tokio::test]
async fn test_oneshot_replaced_before_runtime_agent_connect() {
    // When a new spawn replaces the oneshot before the previous agent
    // connects, the old receiver should resolve with Err (sender dropped).
    let pending: Arc<Mutex<Option<oneshot::Sender<()>>>> = Arc::new(Mutex::new(None));

    // Spawn A
    let (_tx_a, rx_a) = oneshot::channel();
    *pending.lock().await = Some(_tx_a);

    // Spawn B BEFORE A connects — replaces A's sender (drops tx_a)
    let (tx_b, rx_b) = oneshot::channel();
    *pending.lock().await = Some(tx_b); // tx_a dropped here

    // A's receiver resolves with Err (sender dropped = superseded)
    assert!(
        rx_a.await.is_err(),
        "A's receiver should get Err (sender was dropped by B's spawn)"
    );

    // B connects normally
    if let Some(tx) = pending.lock().await.take() {
        tx.send(()).unwrap();
    }
    assert!(rx_b.await.is_ok(), "B's receiver should resolve Ok");
}

#[tokio::test]
async fn test_reset_starting_state_guard() {
    // Verify that reset_starting_state skips when expected_runtime_agent_id
    // doesn't match current_runtime_agent_id.
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _notebook_path) = test_room_with_path(&tmp, "guard_test.ipynb");

    // Set current runtime agent to "agent-B"
    {
        let mut id = room.current_runtime_agent_id.write().await;
        *id = Some("agent-B".to_string());
    }

    // Set kernel status to "starting" (simulates in-progress launch)
    room.state
        .with_doc(|sd| sd.set_kernel_status("starting"))
        .unwrap();

    // Call reset with expected="agent-A" (stale handler) — should skip
    reset_starting_state(&room, Some("agent-A")).await;

    // Verify: kernel_status should still be "starting" (NOT reset)
    {
        let status = room
            .state
            .read(|sd| sd.read_state().kernel.status.clone())
            .unwrap();
        assert_eq!(
            status, "starting",
            "Guard should have prevented reset (agent-A != agent-B)"
        );
    }

    // Verify: current_runtime_agent_id unchanged
    {
        let id = room.current_runtime_agent_id.read().await;
        assert_eq!(id.as_deref(), Some("agent-B"));
    }

    // Now call with matching expected="agent-B" — should reset
    reset_starting_state(&room, Some("agent-B")).await;

    // Verify: kernel_status should be "not_started"
    {
        let status = room
            .state
            .read(|sd| sd.read_state().kernel.status.clone())
            .unwrap();
        assert_eq!(
            status, "not_started",
            "Reset should proceed when expected matches current"
        );
    }

    // Verify: current_runtime_agent_id cleared (provenance cleanup)
    {
        let id = room.current_runtime_agent_id.read().await;
        assert!(
            id.is_none(),
            "Provenance should be cleared after guarded reset"
        );
    }

    // Call with None (pre-spawn) — should always reset
    room.state
        .with_doc(|sd| sd.set_kernel_status("starting"))
        .unwrap();
    reset_starting_state(&room, None).await;
    {
        let status = room
            .state
            .read(|sd| sd.read_state().kernel.status.clone())
            .unwrap();
        assert_eq!(
            status, "not_started",
            "None (pre-spawn) should always reset"
        );
    }
}

#[tokio::test]
async fn test_reset_starting_state_cleanup() {
    // Verify that guarded reset clears request_tx, connect_tx, and handle
    // (belt-and-suspenders cleanup prevents zombie runtime agents).
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _notebook_path) = test_room_with_path(&tmp, "cleanup_test.ipynb");

    // Simulate a runtime agent that has connected: set provenance,
    // request channel, and connect sender.
    {
        let mut id = room.current_runtime_agent_id.write().await;
        *id = Some("agent-A".to_string());
    }
    {
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let mut guard = room.runtime_agent_request_tx.lock().await;
        *guard = Some(tx);
    }
    {
        let (tx, _rx) = oneshot::channel();
        let mut guard = room.pending_runtime_agent_connect_tx.lock().await;
        *guard = Some(tx);
    }

    // Reset with matching agent — should clean up everything
    reset_starting_state(&room, Some("agent-A")).await;

    // Verify all fields cleared
    assert!(
        room.runtime_agent_request_tx.lock().await.is_none(),
        "request_tx should be cleared"
    );
    assert!(
        room.pending_runtime_agent_connect_tx.lock().await.is_none(),
        "connect_tx should be cleared"
    );
    assert!(
        room.runtime_agent_handle.lock().await.is_none(),
        "handle should be cleared"
    );
    assert!(
        room.current_runtime_agent_id.read().await.is_none(),
        "provenance should be cleared"
    );
}

#[tokio::test]
async fn test_reset_aborts_when_new_spawn_detected() {
    // Verify that guarded reset_starting_state aborts field cleanup
    // if a new spawn sets provenance between the provenance-clear and
    // the field clears (TOCTOU re-check).
    //
    // We simulate this by:
    // 1. Setting provenance to "agent-old" + populating fields
    // 2. Clearing provenance to None (as reset_starting_state would)
    // 3. Setting provenance to "agent-new" + new field values (simulating interleaving spawn)
    // 4. Calling reset_starting_state with None expected (pre-spawn path) — always proceeds
    //    But for the guarded path: we test manually by checking the re-check logic.
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _notebook_path) = test_room_with_path(&tmp, "toctou_test.ipynb");

    // Simulate: agent-old's reset already cleared provenance to None,
    // then a new spawn set provenance to "agent-new" with fresh channels.
    {
        let mut id = room.current_runtime_agent_id.write().await;
        *id = Some("agent-new".to_string());
    }
    let (new_tx, mut new_rx) = oneshot::channel::<()>();
    {
        let mut guard = room.pending_runtime_agent_connect_tx.lock().await;
        *guard = Some(new_tx);
    }
    let (req_tx, _req_rx) = tokio::sync::mpsc::channel(16);
    {
        let mut guard = room.runtime_agent_request_tx.lock().await;
        *guard = Some(req_tx);
    }

    // Now call reset with expected="agent-old" — provenance is "agent-new",
    // so the guard should skip entirely (mismatch).
    reset_starting_state(&room, Some("agent-old")).await;

    // Verify: new spawn's fields are untouched
    assert!(
        room.pending_runtime_agent_connect_tx.lock().await.is_some(),
        "new spawn's connect_tx should not be cleared"
    );
    assert!(
        room.runtime_agent_request_tx.lock().await.is_some(),
        "new spawn's request_tx should not be cleared"
    );
    assert_eq!(
        room.current_runtime_agent_id.read().await.as_deref(),
        Some("agent-new"),
        "new spawn's provenance should not be cleared"
    );

    // Verify new_rx is still alive (sender not dropped)
    // Use try_recv — should return TryRecvError::Empty (not Closed)
    assert!(
        new_rx.try_recv().is_err(),
        "new spawn's oneshot should still be pending (sender alive)"
    );
}

#[tokio::test]
async fn test_reset_generation_guard_with_concurrent_spawn() {
    // Regression test for TOCTOU in reset_starting_state: verifies that a
    // new spawn interleaving AFTER provenance is cleared (but before field
    // clears) is detected by the generation counter, causing reset to abort
    // and preserving the new spawn's fields.
    //
    // The test spawns a concurrent task that simulates a new spawn sequence
    // (set provenance → bump generation → store fields) as soon as it
    // detects provenance cleared to None. The main task calls
    // reset_starting_state with a matching expected_runtime_agent_id.
    //
    // Two valid orderings exist:
    // 1. Concurrent spawn completes between provenance clear and field clears
    //    → generation mismatch → reset aborts → new fields preserved
    // 2. Concurrent spawn completes after reset_starting_state returns
    //    → reset clears old fields normally → concurrent spawn stores new fields
    // In both cases, the new spawn's fields are present at the end.
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, _notebook_path) = test_room_with_path(&tmp, "gen_concurrent.ipynb");

    // Setup: agent-old at generation 0 with populated fields.
    {
        let mut id = room.current_runtime_agent_id.write().await;
        *id = Some("agent-old".to_string());
    }
    let (old_tx, _old_rx) = oneshot::channel::<()>();
    {
        let mut guard = room.pending_runtime_agent_connect_tx.lock().await;
        *guard = Some(old_tx);
    }
    let (old_req_tx, _old_req_rx) = tokio::sync::mpsc::channel(16);
    {
        let mut guard = room.runtime_agent_request_tx.lock().await;
        *guard = Some(old_req_tx);
    }

    // Clone Arc fields for the concurrent task.
    let id_arc = room.current_runtime_agent_id.clone();
    let gen_arc = room.runtime_agent_generation.clone();
    let connect_arc = room.pending_runtime_agent_connect_tx.clone();
    let req_arc = room.runtime_agent_request_tx.clone();

    // Channel to receive the new spawn's oneshot receiver (for liveness check).
    let (done_tx, done_rx) = oneshot::channel::<oneshot::Receiver<()>>();

    // Spawn concurrent task: simulate a new spawn that fires as soon as
    // provenance is cleared (the trigger for the TOCTOU scenario).
    tokio::spawn(async move {
        // Poll for provenance → None (reset_starting_state clears it).
        loop {
            {
                let current = id_arc.read().await;
                if current.is_none() {
                    break;
                }
            }
            tokio::task::yield_now().await;
        }

        // Simulate new spawn sequence: provenance → generation → fields.
        {
            let mut id = id_arc.write().await;
            *id = Some("agent-new".to_string());
        }
        gen_arc.fetch_add(1, Ordering::Release);
        let (new_tx, new_rx) = oneshot::channel::<()>();
        {
            let mut guard = connect_arc.lock().await;
            *guard = Some(new_tx);
        }
        let (new_req_tx, _) = tokio::sync::mpsc::channel(16);
        {
            let mut guard = req_arc.lock().await;
            *guard = Some(new_req_tx);
        }

        let _ = done_tx.send(new_rx);
    });

    // Main task: call reset — provenance matches "agent-old", so it proceeds.
    // Generation was captured inside the provenance write lock (gen=0).
    // If the concurrent spawn bumps gen to 1 before field clears, the
    // generation guard aborts the clears. Otherwise, reset clears old fields
    // and the concurrent spawn stores new ones afterward.
    reset_starting_state(&room, Some("agent-old")).await;

    // Wait for concurrent task to complete its spawn simulation.
    let mut new_rx = done_rx
        .await
        .expect("concurrent spawn task should complete");

    // Verify: new spawn's fields must be present regardless of ordering.
    assert!(
        room.pending_runtime_agent_connect_tx.lock().await.is_some(),
        "connect_tx should be present (new spawn's)"
    );
    assert!(
        room.runtime_agent_request_tx.lock().await.is_some(),
        "request_tx should be present (new spawn's)"
    );
    assert_eq!(
        room.current_runtime_agent_id.read().await.as_deref(),
        Some("agent-new"),
        "provenance should be agent-new (set by concurrent spawn)"
    );
    // Verify oneshot sender is still alive (not dropped by reset).
    assert!(
        new_rx.try_recv().is_err(),
        "new spawn's oneshot sender should be alive"
    );
    // Generation should be 1 (bumped by concurrent spawn).
    assert_eq!(
        room.runtime_agent_generation.load(Ordering::Acquire),
        1,
        "generation should be 1 after concurrent spawn"
    );
}

#[test]
fn test_env_yml_insertion_point_no_trailing_newline() {
    // File without trailing newline — must not panic or return out-of-bounds offset
    let content = "dependencies:\n  - numpy";
    let point = find_env_yml_deps_insertion_point(content);
    assert!(point.is_some());
    assert!(point.unwrap() <= content.len());
}

#[test]
fn test_env_yml_insertion_point_with_trailing_newline() {
    let content = "dependencies:\n  - numpy\n  - pandas\n";
    let point = find_env_yml_deps_insertion_point(content);
    assert_eq!(point, Some(content.len()));
}

#[test]
fn test_env_yml_insertion_point_before_next_key() {
    let content = "dependencies:\n  - numpy\nchannels:\n  - conda-forge\n";
    let point = find_env_yml_deps_insertion_point(content);
    // Should insert after "  - numpy\n", before "channels:"
    assert_eq!(point, Some("dependencies:\n  - numpy\n".len()));
}

/// Pre-v4 .ipynb (no output_id fields) gets IDs minted on load,
/// persisted through save, and stable across reload.
#[tokio::test]
async fn test_pre_v4_ipynb_output_id_round_trip() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);

    let notebook_json = serde_json::json!({
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": {},
        "cells": [
            {
                "id": "cell-a",
                "cell_type": "code",
                "source": "1 + 1",
                "execution_count": 1,
                "metadata": {},
                "outputs": [
                    {
                        "output_type": "execute_result",
                        "execution_count": 1,
                        "data": { "text/plain": "2" },
                        "metadata": {}
                    }
                ]
            },
            {
                "id": "cell-b",
                "cell_type": "code",
                "source": "print('hi')",
                "execution_count": 2,
                "metadata": {},
                "outputs": [
                    {
                        "output_type": "stream",
                        "name": "stdout",
                        "text": "hi\n"
                    }
                ]
            },
            {
                "id": "cell-c",
                "cell_type": "code",
                "source": "display('x')",
                "execution_count": 3,
                "metadata": {},
                "outputs": [
                    {
                        "output_type": "display_data",
                        "data": { "text/plain": "x" },
                        "metadata": {}
                    }
                ]
            },
            {
                "id": "cell-d",
                "cell_type": "code",
                "source": "1/0",
                "execution_count": 4,
                "metadata": {},
                "outputs": [
                    {
                        "output_type": "error",
                        "ename": "ZeroDivisionError",
                        "evalue": "division by zero",
                        "traceback": ["line 1"]
                    }
                ]
            }
        ]
    });

    let ipynb_path = tmp.path().join("legacy.ipynb");
    std::fs::write(
        &ipynb_path,
        serde_json::to_string_pretty(&notebook_json).unwrap(),
    )
    .unwrap();

    // --- Load 1: pre-v4 notebook, no output_id fields ---
    let notebook_id = ipynb_path.to_string_lossy().to_string();
    let mut doc = crate::notebook_doc::NotebookDoc::new(&notebook_id);
    let mut state_doc = RuntimeStateDoc::new();
    load_notebook_from_disk_with_state_doc(
        &mut doc,
        Some(&mut state_doc),
        &ipynb_path,
        &blob_store,
    )
    .await
    .unwrap();

    // Collect minted output_ids from RuntimeStateDoc
    let mut first_load_ids: Vec<(String, String)> = Vec::new();
    for cell_id in ["cell-a", "cell-b", "cell-c", "cell-d"] {
        let eid = doc
            .get_execution_id(cell_id)
            .unwrap_or_else(|| panic!("{cell_id} should have execution_id"));
        let outputs = state_doc.get_outputs(&eid);
        assert_eq!(outputs.len(), 1, "{cell_id} should have 1 output");
        let manifest: crate::output_store::OutputManifest =
            serde_json::from_value(outputs[0].clone()).unwrap();
        let id = manifest.output_id().to_string();
        assert!(
            !id.is_empty(),
            "{cell_id} should have a non-empty output_id"
        );
        first_load_ids.push((cell_id.to_string(), id));
    }

    // All IDs should be distinct
    let id_set: std::collections::HashSet<&str> =
        first_load_ids.iter().map(|(_, id)| id.as_str()).collect();
    assert_eq!(id_set.len(), 4, "All output_ids should be unique");

    // --- Save: resolve manifests to .ipynb JSON ---
    let mut saved_ids: Vec<(String, String)> = Vec::new();
    for (cell_id, expected_id) in &first_load_ids {
        let eid = doc.get_execution_id(cell_id).unwrap();
        let outputs = state_doc.get_outputs(&eid);
        let manifest: crate::output_store::OutputManifest =
            serde_json::from_value(outputs[0].clone()).unwrap();
        let resolved = crate::output_store::resolve_manifest(&manifest, &blob_store)
            .await
            .unwrap();
        let saved_id = resolved["output_id"]
            .as_str()
            .unwrap_or_else(|| panic!("{cell_id} resolved JSON should have output_id"));
        assert_eq!(
            saved_id, expected_id,
            "{cell_id}: resolve_manifest should preserve output_id"
        );
        saved_ids.push((cell_id.clone(), saved_id.to_string()));
    }

    // --- Reload: simulate saving and reloading ---
    // Build an .ipynb with output_id fields (as resolve_manifest now produces)
    let mut cells_with_ids = Vec::new();
    for (cell_id, _) in &first_load_ids {
        let eid = doc.get_execution_id(cell_id).unwrap();
        let outputs = state_doc.get_outputs(&eid);
        let manifest: crate::output_store::OutputManifest =
            serde_json::from_value(outputs[0].clone()).unwrap();
        let resolved = crate::output_store::resolve_manifest(&manifest, &blob_store)
            .await
            .unwrap();
        cells_with_ids.push((cell_id.clone(), resolved));
    }

    let saved_notebook = serde_json::json!({
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": {},
        "cells": [
            {
                "id": "cell-a",
                "cell_type": "code",
                "source": "1 + 1",
                "execution_count": 1,
                "metadata": {},
                "outputs": [cells_with_ids[0].1]
            },
            {
                "id": "cell-b",
                "cell_type": "code",
                "source": "print('hi')",
                "execution_count": 2,
                "metadata": {},
                "outputs": [cells_with_ids[1].1]
            },
            {
                "id": "cell-c",
                "cell_type": "code",
                "source": "display('x')",
                "execution_count": 3,
                "metadata": {},
                "outputs": [cells_with_ids[2].1]
            },
            {
                "id": "cell-d",
                "cell_type": "code",
                "source": "1/0",
                "execution_count": 4,
                "metadata": {},
                "outputs": [cells_with_ids[3].1]
            }
        ]
    });

    let ipynb_path2 = tmp.path().join("saved.ipynb");
    std::fs::write(
        &ipynb_path2,
        serde_json::to_string_pretty(&saved_notebook).unwrap(),
    )
    .unwrap();

    // Load the saved notebook
    let mut doc2 = crate::notebook_doc::NotebookDoc::new("reload-test");
    let mut state_doc2 = RuntimeStateDoc::new();
    load_notebook_from_disk_with_state_doc(
        &mut doc2,
        Some(&mut state_doc2),
        &ipynb_path2,
        &blob_store,
    )
    .await
    .unwrap();

    // Verify IDs are stable across the round-trip
    for (cell_id, expected_id) in &first_load_ids {
        let eid = doc2.get_execution_id(cell_id).unwrap();
        let outputs = state_doc2.get_outputs(&eid);
        let manifest: crate::output_store::OutputManifest =
            serde_json::from_value(outputs[0].clone()).unwrap();
        assert_eq!(
            manifest.output_id(),
            expected_id,
            "{cell_id}: output_id should be stable across save/load round-trip"
        );
    }
}

// ── PR 2: prewarmed env capture (spec 2026-04-20) ───────────────────────

/// Build a minimal room suitable for exercising metadata writes. Avoids
/// pulling in the full daemon stack — we only touch `room.doc`.
///
/// Returns `(room, _tmp)` so the TempDir lives at least as long as
/// the room; dropping the TempDir mid-test would remove the docs dir
/// under the room's persist debouncer.
async fn test_room_for_capture() -> (NotebookRoom, tempfile::TempDir) {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);
    let room = NotebookRoom::load_or_create("capture-test", tmp.path(), blob_store);
    // Seed the doc so `get_metadata_snapshot` returns Some, mirroring
    // what `create_empty_notebook` does on a fresh notebook.
    {
        let mut doc = room.doc.write().await;
        let _ = create_empty_notebook(
            &mut doc,
            "python",
            crate::settings_doc::PythonEnvType::Uv,
            Some("test-env-id"),
            None,
            &[],
        );
    }
    (room, tmp)
}

#[tokio::test]
async fn capture_writes_deps_and_env_id_for_fresh_uv_notebook() {
    let (room, _tmp) = test_room_for_capture().await;
    // Wipe env_id first so the capture step sets it.
    {
        let mut doc = room.doc.write().await;
        doc.fork_and_merge(|fork| {
            let mut snap = fork.get_metadata_snapshot().unwrap_or_default();
            snap.runt.env_id = None;
            let _ = fork.set_metadata_snapshot(&snap);
        });
    }
    let user_defaults = vec!["pandas".to_string(), "numpy".to_string()];
    let wrote =
        capture_env_into_metadata(&room, CapturedEnvRuntime::Uv, &user_defaults, "nb-42").await;
    assert!(wrote, "first capture should write both deps and env_id");

    let snap = room.doc.read().await.get_metadata_snapshot().unwrap();
    assert_eq!(snap.runt.env_id.as_deref(), Some("nb-42"));
    assert_eq!(
        snap.runt.uv.as_ref().unwrap().dependencies,
        vec!["pandas".to_string(), "numpy".to_string()]
    );
}

#[tokio::test]
async fn capture_is_idempotent_on_existing_deps() {
    let (room, _tmp) = test_room_for_capture().await;
    // Pre-populate with user-edited deps.
    {
        let mut doc = room.doc.write().await;
        doc.fork_and_merge(|fork| {
            let mut snap = fork.get_metadata_snapshot().unwrap_or_default();
            let uv = snap
                .runt
                .uv
                .get_or_insert_with(|| notebook_doc::metadata::UvInlineMetadata {
                    dependencies: Vec::new(),
                    requires_python: None,
                    prerelease: None,
                });
            uv.dependencies = vec!["scikit-learn".to_string()];
            let _ = fork.set_metadata_snapshot(&snap);
        });
    }

    // Second capture tries to overwrite with different defaults — must not.
    let wrote = capture_env_into_metadata(
        &room,
        CapturedEnvRuntime::Uv,
        &["pandas".to_string()],
        "nb-x",
    )
    .await;
    assert!(
        !wrote,
        "capture must not overwrite user-edited deps (env_id already set)"
    );

    let snap = room.doc.read().await.get_metadata_snapshot().unwrap();
    assert_eq!(
        snap.runt.uv.as_ref().unwrap().dependencies,
        vec!["scikit-learn".to_string()],
        "captured deps must not overwrite existing non-empty list"
    );
}

#[tokio::test]
async fn capture_preserves_existing_env_id_across_calls() {
    let (room, _tmp) = test_room_for_capture().await;
    // env_id is already set by create_empty_notebook to "test-env-id".
    let wrote_first = capture_env_into_metadata(
        &room,
        CapturedEnvRuntime::Uv,
        &["polars".to_string()],
        "different-env-id-ignored",
    )
    .await;
    assert!(wrote_first, "deps filled in, write_id left alone");

    let snap = room.doc.read().await.get_metadata_snapshot().unwrap();
    assert_eq!(
        snap.runt.env_id.as_deref(),
        Some("test-env-id"),
        "existing env_id must survive capture"
    );

    // Second call with same defaults is a no-op.
    let wrote_second =
        capture_env_into_metadata(&room, CapturedEnvRuntime::Uv, &["polars".to_string()], "x")
            .await;
    assert!(
        !wrote_second,
        "second capture must be a no-op when deps and env_id are already set"
    );
}

#[tokio::test]
async fn capture_handles_conda_section_independently() {
    let tmp = tempfile::TempDir::new().unwrap();
    let blob_store = test_blob_store(&tmp);
    let room = NotebookRoom::load_or_create("capture-conda-test", tmp.path(), blob_store);
    {
        let mut doc = room.doc.write().await;
        let _ = create_empty_notebook(
            &mut doc,
            "python",
            crate::settings_doc::PythonEnvType::Conda,
            Some("conda-env-id"),
            None,
            &[],
        );
    }

    let user_defaults = vec!["scipy".to_string()];
    let wrote = capture_env_into_metadata(
        &room,
        CapturedEnvRuntime::Conda,
        &user_defaults,
        "conda-env-id",
    )
    .await;
    assert!(wrote);

    let snap = room.doc.read().await.get_metadata_snapshot().unwrap();
    assert_eq!(
        snap.runt.conda.as_ref().unwrap().dependencies,
        vec!["scipy".to_string()]
    );
    // UV section should remain untouched.
    assert!(snap.runt.uv.is_none());
}

#[test]
fn captured_env_for_runtime_reads_uv_deps_and_env_id() {
    let mut snap = NotebookMetadataSnapshot::default();
    snap.runt.env_id = Some("abc".to_string());
    snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
        dependencies: vec!["pandas".to_string()],
        requires_python: None,
        prerelease: None,
    });
    let captured =
        captured_env_for_runtime(Some(&snap), CapturedEnvRuntime::Uv).expect("captured env");
    assert_eq!(captured.env_id(), "abc");
    assert_eq!(captured.dependencies(), &["pandas".to_string()]);
    match &captured {
        CapturedEnv::Uv { deps, .. } => {
            assert_eq!(deps.requires_python, None);
            assert_eq!(deps.prerelease, None);
        }
        _ => panic!("expected UV captured env"),
    }
}

#[test]
fn captured_env_for_runtime_returns_empty_deps_when_section_missing() {
    let mut snap = NotebookMetadataSnapshot::default();
    snap.runt.env_id = Some("xyz".to_string());
    // No uv or conda section populated.
    let captured =
        captured_env_for_runtime(Some(&snap), CapturedEnvRuntime::Uv).expect("captured env");
    assert!(captured.dependencies().is_empty());
    assert_eq!(captured.env_id(), "xyz");
}

#[test]
fn captured_env_for_runtime_includes_uv_resolver_fields() {
    // P2 regression: captured lookup must carry requires-python and
    // prerelease, not just the dep list. Otherwise the on-disk hash
    // computed on reopen would differ from what the capture step
    // originally wrote, causing false cache misses or worse, matching
    // the wrong cached env.
    let mut snap = NotebookMetadataSnapshot::default();
    snap.runt.env_id = Some("env-uv".to_string());
    snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
        dependencies: vec!["pandas".to_string()],
        requires_python: Some(">=3.10".to_string()),
        prerelease: Some("allow".to_string()),
    });

    let captured =
        captured_env_for_runtime(Some(&snap), CapturedEnvRuntime::Uv).expect("captured env");
    match &captured {
        CapturedEnv::Uv { deps, env_id } => {
            assert_eq!(env_id, "env-uv");
            assert_eq!(deps.dependencies, vec!["pandas".to_string()]);
            assert_eq!(deps.requires_python.as_deref(), Some(">=3.10"));
            assert_eq!(deps.prerelease.as_deref(), Some("allow"));
        }
        _ => panic!("expected UV captured env"),
    }
}

#[test]
fn captured_env_for_runtime_includes_conda_resolver_fields() {
    // P2 regression: captured lookup must carry channels and python pin.
    let mut snap = NotebookMetadataSnapshot::default();
    snap.runt.env_id = Some("env-conda".to_string());
    snap.runt.conda = Some(notebook_doc::metadata::CondaInlineMetadata {
        dependencies: vec!["scipy".to_string()],
        channels: vec!["pytorch".to_string(), "nvidia".to_string()],
        python: Some("3.11".to_string()),
    });

    let captured =
        captured_env_for_runtime(Some(&snap), CapturedEnvRuntime::Conda).expect("captured env");
    match &captured {
        CapturedEnv::Conda { deps, env_id } => {
            assert_eq!(env_id, "env-conda");
            assert_eq!(deps.dependencies, vec!["scipy".to_string()]);
            assert_eq!(
                deps.channels,
                vec!["pytorch".to_string(), "nvidia".to_string()]
            );
            assert_eq!(deps.python.as_deref(), Some("3.11"));
        }
        _ => panic!("expected Conda captured env"),
    }
}

#[test]
fn captured_env_hash_differs_when_uv_prerelease_changes() {
    // P2 invariant: two captures with identical deps + env_id but a
    // different prerelease strategy must hash to different paths. If
    // they didn't, the on-disk lookup would happily find the wrong
    // prior env and reuse it with the wrong install set.
    let base_deps = vec!["pandas".to_string()];
    let a = kernel_env::UvDependencies {
        dependencies: base_deps.clone(),
        requires_python: None,
        prerelease: None,
    };
    let b = kernel_env::UvDependencies {
        dependencies: base_deps,
        requires_python: None,
        prerelease: Some("allow".to_string()),
    };
    let hash_a = kernel_env::uv::compute_unified_env_hash(&a, "same-env-id");
    let hash_b = kernel_env::uv::compute_unified_env_hash(&b, "same-env-id");
    assert_ne!(hash_a, hash_b);
}

#[test]
fn captured_env_hash_differs_when_conda_channels_change() {
    let base_deps = vec!["scipy".to_string()];
    let a = kernel_env::CondaDependencies {
        dependencies: base_deps.clone(),
        channels: vec!["conda-forge".to_string()],
        python: None,
        env_id: None,
    };
    let b = kernel_env::CondaDependencies {
        dependencies: base_deps.clone(),
        channels: vec!["conda-forge".to_string(), "pytorch".to_string()],
        python: None,
        env_id: None,
    };
    let hash_a = kernel_env::conda::compute_unified_env_hash(&a, "same-env-id");
    let hash_b = kernel_env::conda::compute_unified_env_hash(&b, "same-env-id");
    assert_ne!(hash_a, hash_b);

    // Python pin also contributes.
    let c = kernel_env::CondaDependencies {
        dependencies: base_deps,
        channels: vec!["conda-forge".to_string()],
        python: Some("3.12".to_string()),
        env_id: None,
    };
    let hash_c = kernel_env::conda::compute_unified_env_hash(&c, "same-env-id");
    assert_ne!(hash_a, hash_c);
}

#[test]
fn captured_env_for_runtime_requires_env_id() {
    let snap = NotebookMetadataSnapshot::default();
    assert!(captured_env_for_runtime(Some(&snap), CapturedEnvRuntime::Uv).is_none());
}

#[test]
fn captured_env_source_override_returns_none_when_no_env_id() {
    let snap = NotebookMetadataSnapshot::default();
    assert!(captured_env_source_override(Some(&snap)).is_none());
}

#[test]
fn captured_env_source_override_returns_none_when_deps_present_but_env_missing() {
    // Deps-present-but-disk-absent is intentionally NOT treated as
    // captured: we cannot tell a GC'd captured env apart from a fresh
    // notebook whose user added inline deps before the first launch.
    // Falling through to the normal inline path is the safer default —
    // same deps still dedup across notebooks via the legacy inline
    // cache; they just lose per-notebook env_id isolation for that
    // rebuild.
    let mut snap = NotebookMetadataSnapshot::default();
    snap.runt.env_id = Some(format!("unlikely-env-id-{}", uuid::Uuid::new_v4()));
    snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
        dependencies: vec!["pandas".to_string()],
        requires_python: None,
        prerelease: None,
    });
    assert!(captured_env_source_override(Some(&snap)).is_none());
}

#[test]
fn captured_env_source_override_returns_none_for_fresh_notebook_empty_deps() {
    // `create_empty_notebook` assigns an env_id and an empty uv/conda
    // section on every new notebook. Empty deps + no env on disk must
    // NOT be treated as captured, otherwise brand-new `uv:prewarmed`
    // launches bypass the warmed pool and build a base env from scratch.
    let mut snap = NotebookMetadataSnapshot::default();
    snap.runt.env_id = Some(format!("fresh-env-{}", uuid::Uuid::new_v4()));
    snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
        dependencies: vec![],
        requires_python: None,
        prerelease: None,
    });
    assert!(captured_env_source_override(Some(&snap)).is_none());
}

/// Given a tmpdir pretending to be the UV cache, materialise a fake
/// venv at `{cache}/{hash}/bin/python` for the given captured env so
/// `unified_env_on_disk_in` finds it.
fn materialise_fake_uv_venv(
    deps: &kernel_env::UvDependencies,
    env_id: &str,
    cache_dir: &Path,
) -> PathBuf {
    let hash = kernel_env::uv::compute_unified_env_hash(deps, env_id);
    let venv_path = cache_dir.join(&hash);
    #[cfg(target_os = "windows")]
    let python_path = venv_path.join("Scripts").join("python.exe");
    #[cfg(not(target_os = "windows"))]
    let python_path = venv_path.join("bin").join("python");
    std::fs::create_dir_all(python_path.parent().unwrap()).unwrap();
    std::fs::write(&python_path, b"#!/bin/sh\n").unwrap();
    venv_path
}

#[test]
fn should_preserve_env_on_eviction_untitled_notebook_deletes() {
    // Even with captured deps + env on disk, no saved path means the
    // .ipynb won't persist the env_id binding — the env is orphaned.
    let tmp = tempfile::tempdir().unwrap();
    let uv_cache = tmp.path().to_path_buf();
    let conda_cache = tmp.path().join("conda");
    std::fs::create_dir_all(&conda_cache).unwrap();

    let deps = kernel_env::UvDependencies {
        dependencies: vec!["pandas".to_string()],
        requires_python: None,
        prerelease: None,
    };
    let env_id = "env-untitled";
    let venv_path = materialise_fake_uv_venv(&deps, env_id, &uv_cache);

    let mut snap = NotebookMetadataSnapshot::default();
    snap.runt.env_id = Some(env_id.to_string());
    snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
        dependencies: deps.dependencies.clone(),
        requires_python: None,
        prerelease: None,
    });

    assert!(!should_preserve_env_on_eviction(
        false, // has_saved_path
        &venv_path,
        Some(&snap),
        &uv_cache,
        &conda_cache,
    ));
}

#[test]
fn should_preserve_env_on_eviction_saved_captured_notebook_preserves() {
    let tmp = tempfile::tempdir().unwrap();
    let uv_cache = tmp.path().to_path_buf();
    let conda_cache = tmp.path().join("conda");
    std::fs::create_dir_all(&conda_cache).unwrap();

    let deps = kernel_env::UvDependencies {
        dependencies: vec!["pandas".to_string(), "numpy".to_string()],
        requires_python: None,
        prerelease: None,
    };
    let env_id = "env-saved";
    let venv_path = materialise_fake_uv_venv(&deps, env_id, &uv_cache);

    let mut snap = NotebookMetadataSnapshot::default();
    snap.runt.env_id = Some(env_id.to_string());
    snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
        dependencies: deps.dependencies.clone(),
        requires_python: None,
        prerelease: None,
    });

    assert!(should_preserve_env_on_eviction(
        true,
        &venv_path,
        Some(&snap),
        &uv_cache,
        &conda_cache,
    ));
}

#[test]
fn should_preserve_env_on_eviction_path_mismatch_deletes() {
    // Room has a saved path but its runtime_agent_env_path points at a
    // pool env (runtimed-uv-xxx), not the captured hash dir. That's a
    // pool-launched notebook — delete on eviction like before.
    let tmp = tempfile::tempdir().unwrap();
    let uv_cache = tmp.path().to_path_buf();
    let conda_cache = tmp.path().join("conda");
    std::fs::create_dir_all(&conda_cache).unwrap();

    let deps = kernel_env::UvDependencies {
        dependencies: vec!["pandas".to_string()],
        requires_python: None,
        prerelease: None,
    };
    let env_id = "env-pool";
    let _ = materialise_fake_uv_venv(&deps, env_id, &uv_cache);

    let pool_path = uv_cache.join("runtimed-uv-abc123");

    let mut snap = NotebookMetadataSnapshot::default();
    snap.runt.env_id = Some(env_id.to_string());
    snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
        dependencies: deps.dependencies.clone(),
        requires_python: None,
        prerelease: None,
    });

    assert!(!should_preserve_env_on_eviction(
        true,
        &pool_path,
        Some(&snap),
        &uv_cache,
        &conda_cache,
    ));
}

#[test]
fn should_preserve_env_on_eviction_no_captured_metadata_deletes() {
    // Saved notebook but empty metadata — never captured, nothing to
    // preserve. This covers fresh notebooks that got saved before
    // first launch.
    let tmp = tempfile::tempdir().unwrap();
    let uv_cache = tmp.path().to_path_buf();
    let conda_cache = tmp.path().join("conda");
    std::fs::create_dir_all(&conda_cache).unwrap();

    let some_path = uv_cache.join("runtimed-uv-deadbeef");

    assert!(!should_preserve_env_on_eviction(
        true,
        &some_path,
        Some(&NotebookMetadataSnapshot::default()),
        &uv_cache,
        &conda_cache,
    ));
}

#[test]
fn effective_user_deps_from_launched_uses_uv_deps_when_set() {
    // Hot-sync appends to launched.uv_deps, so that's the source of
    // truth for what should land in metadata.
    let launched = LaunchedEnvConfig {
        uv_deps: Some(vec!["pandas".to_string(), "numpy".to_string()]),
        ..LaunchedEnvConfig::default()
    };
    let deps = effective_user_deps_from_launched(&launched, CapturedEnvRuntime::Uv).unwrap();
    assert_eq!(deps, vec!["pandas".to_string(), "numpy".to_string()]);
}

#[test]
fn effective_user_deps_from_launched_ignores_prewarmed_packages() {
    // Pure prewarmed kernel that never hot-synced: uv_deps and
    // conda_deps are both None, prewarmed_packages has the pool's
    // install list. We intentionally return None here so the eviction
    // flush doesn't mistakenly populate metadata.runt.conda for a
    // pure UV kernel (or vice versa). For this case captured
    // metadata was already written at claim time and there's nothing
    // to flush.
    let launched = LaunchedEnvConfig {
        uv_deps: None,
        conda_deps: None,
        prewarmed_packages: vec![
            "ipykernel".to_string(),
            "ipywidgets".to_string(),
            "anywidget".to_string(),
            "nbformat".to_string(),
            "uv".to_string(),
            "dx".to_string(),
            "pandas".to_string(),
        ],
        ..LaunchedEnvConfig::default()
    };
    assert!(effective_user_deps_from_launched(&launched, CapturedEnvRuntime::Uv).is_none());
    assert!(effective_user_deps_from_launched(&launched, CapturedEnvRuntime::Conda).is_none());
}

#[test]
fn effective_user_deps_from_launched_strips_base_from_uv_deps() {
    // Hot-synced a package into a captured-reopen kernel: uv_deps
    // = [captured_user_deps + synced]. Base packages should never
    // be in uv_deps in practice, but strip_base is idempotent and
    // this guards against accidental inclusion.
    let launched = LaunchedEnvConfig {
        uv_deps: Some(vec![
            "ipykernel".to_string(),
            "pandas".to_string(),
            "numpy".to_string(),
        ]),
        ..LaunchedEnvConfig::default()
    };
    let deps = effective_user_deps_from_launched(&launched, CapturedEnvRuntime::Uv).unwrap();
    assert_eq!(deps, vec!["pandas".to_string(), "numpy".to_string()]);
}

#[test]
fn effective_user_deps_from_launched_returns_none_for_wrong_runtime() {
    // Kernel launched with uv_deps; asking for Conda view returns None.
    let launched = LaunchedEnvConfig {
        uv_deps: Some(vec!["pandas".to_string()]),
        ..LaunchedEnvConfig::default()
    };
    assert!(effective_user_deps_from_launched(&launched, CapturedEnvRuntime::Conda).is_none());
}

#[test]
fn effective_user_deps_from_launched_returns_none_for_deno_only() {
    // Deno kernel: no uv/conda deps at all. No flush applicable.
    let launched = LaunchedEnvConfig::default();
    assert!(effective_user_deps_from_launched(&launched, CapturedEnvRuntime::Uv).is_none());
    assert!(effective_user_deps_from_launched(&launched, CapturedEnvRuntime::Conda).is_none());
}

#[test]
fn effective_user_deps_from_launched_conda_uses_conda_deps() {
    let launched = LaunchedEnvConfig {
        conda_deps: Some(vec!["scipy".to_string()]),
        ..LaunchedEnvConfig::default()
    };
    let deps = effective_user_deps_from_launched(&launched, CapturedEnvRuntime::Conda).unwrap();
    assert_eq!(deps, vec!["scipy".to_string()]);
}

#[tokio::test]
async fn rename_env_dir_moves_to_unified_hash_target() {
    let tmp = tempfile::tempdir().unwrap();
    let uv_cache = tmp.path().to_path_buf();
    let conda_cache = tmp.path().join("conda");
    std::fs::create_dir_all(&conda_cache).unwrap();

    // Initial state: env lives under the OLD hash (captured deps
    // before hot-sync). We materialise a fake venv there.
    let old_deps = kernel_env::UvDependencies {
        dependencies: vec!["pandas".to_string()],
        requires_python: None,
        prerelease: None,
    };
    let env_id = "rename-target";
    let old_path = materialise_fake_uv_venv(&old_deps, env_id, &uv_cache);

    // Metadata after flush: deps grew to include numpy (new hash).
    let new_deps = kernel_env::UvDependencies {
        dependencies: vec!["pandas".to_string(), "numpy".to_string()],
        requires_python: None,
        prerelease: None,
    };
    let mut snap = NotebookMetadataSnapshot::default();
    snap.runt.env_id = Some(env_id.to_string());
    snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
        dependencies: new_deps.dependencies.clone(),
        requires_python: None,
        prerelease: None,
    });

    let expected_target =
        uv_cache.join(kernel_env::uv::compute_unified_env_hash(&new_deps, env_id));
    assert!(!expected_target.exists());

    let returned = rename_env_dir_to_unified_hash(
        &old_path,
        Some(&snap),
        CapturedEnvRuntime::Uv,
        &uv_cache,
        &conda_cache,
    )
    .await;

    assert_eq!(returned, expected_target);
    assert!(!old_path.exists());
    assert!(expected_target.exists());
}

#[tokio::test]
async fn rename_env_dir_noop_when_already_at_target() {
    let tmp = tempfile::tempdir().unwrap();
    let uv_cache = tmp.path().to_path_buf();
    let conda_cache = tmp.path().join("conda");
    std::fs::create_dir_all(&conda_cache).unwrap();

    let deps = kernel_env::UvDependencies {
        dependencies: vec!["pandas".to_string()],
        requires_python: None,
        prerelease: None,
    };
    let env_id = "already-correct";
    let path = materialise_fake_uv_venv(&deps, env_id, &uv_cache);

    let mut snap = NotebookMetadataSnapshot::default();
    snap.runt.env_id = Some(env_id.to_string());
    snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
        dependencies: deps.dependencies.clone(),
        requires_python: None,
        prerelease: None,
    });

    let returned = rename_env_dir_to_unified_hash(
        &path,
        Some(&snap),
        CapturedEnvRuntime::Uv,
        &uv_cache,
        &conda_cache,
    )
    .await;

    assert_eq!(returned, path);
    assert!(path.exists());
}

#[tokio::test]
async fn rename_env_dir_skips_when_target_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let uv_cache = tmp.path().to_path_buf();
    let conda_cache = tmp.path().join("conda");
    std::fs::create_dir_all(&conda_cache).unwrap();

    // Two distinct envs on disk — an old one and the target name
    // from a different notebook already claimed. We must not
    // clobber the target.
    let old_deps = kernel_env::UvDependencies {
        dependencies: vec!["pandas".to_string()],
        requires_python: None,
        prerelease: None,
    };
    let env_id = "collide";
    let old_path = materialise_fake_uv_venv(&old_deps, env_id, &uv_cache);

    let new_deps = kernel_env::UvDependencies {
        dependencies: vec!["pandas".to_string(), "numpy".to_string()],
        requires_python: None,
        prerelease: None,
    };
    let occupied_path = materialise_fake_uv_venv(&new_deps, env_id, &uv_cache);
    assert!(occupied_path.exists());

    let mut snap = NotebookMetadataSnapshot::default();
    snap.runt.env_id = Some(env_id.to_string());
    snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
        dependencies: new_deps.dependencies.clone(),
        requires_python: None,
        prerelease: None,
    });

    let returned = rename_env_dir_to_unified_hash(
        &old_path,
        Some(&snap),
        CapturedEnvRuntime::Uv,
        &uv_cache,
        &conda_cache,
    )
    .await;

    // Target is occupied — leave source intact.
    assert_eq!(returned, old_path);
    assert!(old_path.exists());
    assert!(occupied_path.exists());
}

#[tokio::test]
async fn rename_env_dir_noop_when_no_captured_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let uv_cache = tmp.path().to_path_buf();
    let conda_cache = tmp.path().join("conda");
    std::fs::create_dir_all(&conda_cache).unwrap();

    let some_path = uv_cache.join("runtimed-uv-abc123");
    std::fs::create_dir_all(&some_path).unwrap();

    let returned = rename_env_dir_to_unified_hash(
        &some_path,
        Some(&NotebookMetadataSnapshot::default()),
        CapturedEnvRuntime::Uv,
        &uv_cache,
        &conda_cache,
    )
    .await;

    assert_eq!(returned, some_path);
    assert!(some_path.exists());
}

/// Given a tmpdir pretending to be the Conda cache, materialise a
/// fake env at `{cache}/{hash}/bin/python`.
fn materialise_fake_conda_env(
    deps: &kernel_env::CondaDependencies,
    env_id: &str,
    cache_dir: &Path,
) -> PathBuf {
    let hash = kernel_env::conda::compute_unified_env_hash(deps, env_id);
    let env_path = cache_dir.join(&hash);
    #[cfg(target_os = "windows")]
    let python_path = env_path.join("python.exe");
    #[cfg(not(target_os = "windows"))]
    let python_path = env_path.join("bin").join("python");
    std::fs::create_dir_all(python_path.parent().unwrap()).unwrap();
    std::fs::write(&python_path, b"#!/bin/sh\n").unwrap();
    env_path
}

/// Regression: conda-only notebook with env_id set must not route
/// its rename through the UV hash function. Before the runtime
/// parameter was explicit, `captured_env_for_runtime(Uv)` would
/// synthesise a zero-dep UV capture from `runt.env_id`, and the
/// rename helper would pick the UV-hash path first even though the
/// kernel was conda.
#[tokio::test]
async fn rename_env_dir_uses_conda_hash_when_runtime_is_conda() {
    let tmp = tempfile::tempdir().unwrap();
    let uv_cache = tmp.path().join("uv");
    let conda_cache = tmp.path().join("conda");
    std::fs::create_dir_all(&uv_cache).unwrap();
    std::fs::create_dir_all(&conda_cache).unwrap();

    let old_conda_deps = kernel_env::CondaDependencies {
        dependencies: vec!["numpy".to_string()],
        channels: vec!["conda-forge".to_string()],
        python: None,
        env_id: None,
    };
    let env_id = "conda-rename";
    let old_path = materialise_fake_conda_env(&old_conda_deps, env_id, &conda_cache);

    let new_conda_deps = kernel_env::CondaDependencies {
        dependencies: vec!["numpy".to_string(), "scipy".to_string()],
        channels: vec!["conda-forge".to_string()],
        python: None,
        env_id: None,
    };
    let expected = conda_cache.join(kernel_env::conda::compute_unified_env_hash(
        &new_conda_deps,
        env_id,
    ));

    let mut snap = NotebookMetadataSnapshot::default();
    snap.runt.env_id = Some(env_id.to_string());
    snap.runt.conda = Some(notebook_doc::metadata::CondaInlineMetadata {
        dependencies: new_conda_deps.dependencies.clone(),
        channels: new_conda_deps.channels.clone(),
        python: None,
    });

    let returned = rename_env_dir_to_unified_hash(
        &old_path,
        Some(&snap),
        CapturedEnvRuntime::Conda,
        &uv_cache,
        &conda_cache,
    )
    .await;

    assert_eq!(returned, expected);
    assert!(expected.exists());
    assert!(!old_path.exists());
}

/// P1 regression: the manual LaunchKernel handler must apply the captured
/// override when the requested `env_source` is auto/prewarmed but respect
/// explicit `auto:uv` / `auto:conda` scopes when they disagree with the
/// captured runtime.
///
/// This mirrors the filter inside the LaunchKernel handler. The daemon
/// side of the launch pipeline needs real pool state, so we can't spin
/// it up from a unit test — the filter is factored so the logic it
/// consumes is unit-testable in isolation.
#[test]
fn launch_kernel_captured_override_respects_auto_scope() {
    // The `captured` inputs here are the *string* outputs of
    // `captured_env_source_override`. Simulate a UV-captured notebook.
    let captured_uv = Some("uv:prewarmed".to_string());
    let captured_conda = Some("conda:prewarmed".to_string());

    // Replicates the inline filter inside the LaunchKernel handler.
    fn apply_scope(captured: Option<String>, auto_scope: Option<&str>) -> Option<String> {
        captured.filter(|src| match auto_scope {
            Some("uv") => src == "uv:prewarmed",
            Some("conda") => src == "conda:prewarmed",
            Some("pixi") => false,
            _ => true,
        })
    }

    // Plain auto (no scope) — captured override wins.
    assert_eq!(
        apply_scope(captured_uv.clone(), None),
        Some("uv:prewarmed".to_string())
    );
    assert_eq!(
        apply_scope(captured_conda.clone(), None),
        Some("conda:prewarmed".to_string())
    );

    // Explicit matching scope still fires.
    assert_eq!(
        apply_scope(captured_uv.clone(), Some("uv")),
        Some("uv:prewarmed".to_string())
    );
    assert_eq!(
        apply_scope(captured_conda.clone(), Some("conda")),
        Some("conda:prewarmed".to_string())
    );

    // Explicit mismatched scope drops the override so the project-file /
    // inline-deps priority chain takes over — user intent wins.
    assert_eq!(apply_scope(captured_uv.clone(), Some("conda")), None);
    assert_eq!(apply_scope(captured_conda.clone(), Some("uv")), None);

    // `auto:pixi` always drops the override (no pixi captures today).
    assert_eq!(apply_scope(captured_uv, Some("pixi")), None);
    assert_eq!(apply_scope(captured_conda, Some("pixi")), None);
}

/// Pre-upgrade notebooks: env_id is set but deps are empty. The capture
/// step must still record the env_id (no-op) and populate user_defaults
/// if they were derived from the pool. This is the migration path from
/// § 4 Migration of the spec.
#[tokio::test]
async fn capture_migrates_pre_upgrade_notebook() {
    let (room, _tmp) = test_room_for_capture().await;
    // Pre-upgrade state: env_id exists, uv section exists but empty.
    let snap_before = room.doc.read().await.get_metadata_snapshot().unwrap();
    assert_eq!(snap_before.runt.env_id.as_deref(), Some("test-env-id"));
    assert!(
        snap_before
            .runt
            .uv
            .as_ref()
            .map(|u| u.dependencies.is_empty())
            .unwrap_or(true),
        "pre-upgrade notebook starts with empty uv deps"
    );

    let user_defaults = vec!["polars".to_string()];
    let wrote =
        capture_env_into_metadata(&room, CapturedEnvRuntime::Uv, &user_defaults, "test-env-id")
            .await;
    assert!(wrote, "migration should populate user_defaults into deps");

    let snap_after = room.doc.read().await.get_metadata_snapshot().unwrap();
    assert_eq!(
        snap_after.runt.uv.as_ref().unwrap().dependencies,
        vec!["polars".to_string()]
    );
}
