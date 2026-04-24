//! Integration tests for `TrustAllowlist`. Exercises the full Lance
//! code path — open → add → contains → remove → reopen — against a
//! fresh tempdir per test.

use runt_store::{PackageManager, TrustAllowlist};
use tempfile::TempDir;

#[tokio::test]
async fn fresh_store_is_empty() {
    let tmp = TempDir::new().unwrap();
    let store = TrustAllowlist::open(tmp.path()).await.unwrap();
    assert!(store.is_empty());
    assert!(!store.contains(PackageManager::Uv, "pandas"));
}

#[tokio::test]
async fn add_then_contains() {
    let tmp = TempDir::new().unwrap();
    let store = TrustAllowlist::open(tmp.path()).await.unwrap();
    store
        .add(
            PackageManager::Uv,
            &["pandas".to_string(), "numpy".to_string()],
        )
        .await
        .unwrap();
    assert!(store.contains(PackageManager::Uv, "pandas"));
    assert!(store.contains(PackageManager::Uv, "numpy"));
    assert!(!store.contains(PackageManager::Conda, "pandas"));
}

#[tokio::test]
async fn add_is_idempotent_in_memory() {
    let tmp = TempDir::new().unwrap();
    let store = TrustAllowlist::open(tmp.path()).await.unwrap();
    store
        .add(PackageManager::Uv, &["pandas".to_string()])
        .await
        .unwrap();
    store
        .add(PackageManager::Uv, &["pandas".to_string()])
        .await
        .unwrap();
    assert_eq!(store.len(), 1);
}

#[tokio::test]
async fn novel_returns_uncovered_only() {
    let tmp = TempDir::new().unwrap();
    let store = TrustAllowlist::open(tmp.path()).await.unwrap();
    store
        .add(
            PackageManager::Uv,
            &["pandas".to_string(), "numpy".to_string()],
        )
        .await
        .unwrap();
    let novel = store.novel([
        (PackageManager::Uv, "pandas"),
        (PackageManager::Uv, "numpy"),
        (PackageManager::Uv, "polars"),
    ]);
    assert_eq!(novel, vec![(PackageManager::Uv, "polars".to_string())]);
}

#[tokio::test]
async fn reopen_restores_entries() {
    let tmp = TempDir::new().unwrap();
    {
        let store = TrustAllowlist::open(tmp.path()).await.unwrap();
        store
            .add(
                PackageManager::Conda,
                &["scipy".to_string(), "matplotlib".to_string()],
            )
            .await
            .unwrap();
    }
    // Drop the first handle, reopen against the same directory.
    let store = TrustAllowlist::open(tmp.path()).await.unwrap();
    assert_eq!(store.len(), 2);
    assert!(store.contains(PackageManager::Conda, "scipy"));
    assert!(store.contains(PackageManager::Conda, "matplotlib"));
}

#[tokio::test]
async fn remove_drops_single_entry() {
    let tmp = TempDir::new().unwrap();
    let store = TrustAllowlist::open(tmp.path()).await.unwrap();
    store
        .add(
            PackageManager::Uv,
            &["pandas".to_string(), "numpy".to_string()],
        )
        .await
        .unwrap();
    store.remove(PackageManager::Uv, "pandas").await.unwrap();
    assert!(!store.contains(PackageManager::Uv, "pandas"));
    assert!(store.contains(PackageManager::Uv, "numpy"));

    // Removing a not-present entry is a no-op.
    store
        .remove(PackageManager::Uv, "never-existed")
        .await
        .unwrap();
    assert_eq!(store.len(), 1);
}

#[tokio::test]
async fn list_filters_by_manager() {
    let tmp = TempDir::new().unwrap();
    let store = TrustAllowlist::open(tmp.path()).await.unwrap();
    store
        .add(
            PackageManager::Uv,
            &["pandas".to_string(), "numpy".to_string()],
        )
        .await
        .unwrap();
    store
        .add(PackageManager::Conda, &["scipy".to_string()])
        .await
        .unwrap();

    let uv = store.list(PackageManager::Uv);
    assert_eq!(uv, vec!["numpy".to_string(), "pandas".to_string()]);
    let conda = store.list(PackageManager::Conda);
    assert_eq!(conda, vec!["scipy".to_string()]);
}

#[tokio::test]
async fn list_all_round_trips_timestamps() {
    let tmp = TempDir::new().unwrap();
    let store = TrustAllowlist::open(tmp.path()).await.unwrap();
    store
        .add(PackageManager::Uv, &["pandas".to_string()])
        .await
        .unwrap();
    let all = store.list_all().await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].name, "pandas");
    assert!(all[0].added_at > 0);
}

#[tokio::test]
async fn clear_empties_both_views() {
    let tmp = TempDir::new().unwrap();
    let store = TrustAllowlist::open(tmp.path()).await.unwrap();
    store
        .add(
            PackageManager::Uv,
            &["pandas".to_string(), "numpy".to_string()],
        )
        .await
        .unwrap();
    store.clear().await.unwrap();
    assert!(store.is_empty());
    // Reopen to confirm the on-disk state matches.
    drop(store);
    let reopened = TrustAllowlist::open(tmp.path()).await.unwrap();
    assert!(reopened.is_empty());
}
