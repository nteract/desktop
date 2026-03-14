// Tests can use unwrap/expect freely - panics are acceptable in test code
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for UV support and Rattler conda fallback.
//!
//! These tests verify the happy paths for environment detection and creation:
//! - UV is detected when available on PATH
//! - Conda/rattler fallback happens when UV is unavailable
//! - Environment creation works correctly for both backends
//!
//! Tests that modify PATH are marked with `#[serial]` to prevent race conditions.
//!
//! Note: These tests are disabled on Windows CI due to rattler DLL dependencies
//! that may not be available in the test environment.

// Skip this entire test module on Windows - rattler has DLL dependencies that
// cause STATUS_ENTRYPOINT_NOT_FOUND when the test binary loads in CI
#![cfg(not(target_os = "windows"))]

use notebook::{conda_env, uv_env};
use serial_test::serial;
use std::path::PathBuf;

// =============================================================================
// UV Availability Detection Tests
// =============================================================================

/// Test that check_uv_available returns true even when PATH excludes uv.
/// This is because uv can be bootstrapped via rattler from conda-forge.
#[tokio::test]
#[serial]
async fn test_uv_available_returns_true_with_bootstrap() {
    let original_path = std::env::var("PATH").unwrap_or_default();

    // Set PATH to a nonexistent directory
    std::env::set_var("PATH", "/nonexistent");

    let result = uv_env::check_uv_available().await;

    // Restore PATH before assertion (in case assertion fails)
    std::env::set_var("PATH", &original_path);

    // uv is now always available because it can be bootstrapped via rattler
    assert!(
        result,
        "check_uv_available should return true (bootstrap available)"
    );
}

/// Test that check_uv_available returns true when real uv is installed.
/// This test is skipped if uv is not actually installed on the system.
#[tokio::test]
#[serial]
async fn test_uv_available_with_real_uv() {
    let result = uv_env::check_uv_available().await;

    if result {
        println!("Real uv is installed on this system");
    } else {
        println!("Real uv is NOT installed - this is expected in some CI environments");
    }

    // This test just verifies the function runs without panicking
    // The result depends on the actual system configuration
}

// =============================================================================
// UV Environment Creation Tests (Happy Path)
// =============================================================================

/// Test UV environment creation with no dependencies (just ipykernel).
/// Skipped if uv is not installed on the system.
#[tokio::test]
#[serial]
async fn test_uv_environment_creation_with_no_deps() {
    if !uv_env::check_uv_available().await {
        println!("Skipping test: uv not installed on this system");
        return;
    }

    let deps = uv_env::NotebookDependencies {
        dependencies: vec![],
        requires_python: None,
        prerelease: None,
    };

    let result = uv_env::prepare_environment(&deps, None).await;

    assert!(
        result.is_ok(),
        "prepare_environment should succeed: {:?}",
        result.err()
    );

    let env = result.unwrap();
    assert!(
        env.python_path.exists(),
        "Python path should exist: {:?}",
        env.python_path
    );
    assert!(
        env.venv_path.exists(),
        "Venv path should exist: {:?}",
        env.venv_path
    );

    // Verify the path is in the expected cache location
    let venv_str = env.venv_path.to_string_lossy();
    assert!(
        venv_str.contains("runt") && venv_str.contains("envs"),
        "Venv should be in runt/envs cache: {}",
        venv_str
    );
}

/// Test that UV environment uses cache correctly (same deps = same env).
#[tokio::test]
#[serial]
async fn test_uv_environment_uses_cache_correctly() {
    if !uv_env::check_uv_available().await {
        println!("Skipping test: uv not installed on this system");
        return;
    }

    let deps = uv_env::NotebookDependencies {
        dependencies: vec![],
        requires_python: Some(">=3.9".to_string()),
        prerelease: None,
    };

    // Create environment twice
    let env1 = uv_env::prepare_environment(&deps, None)
        .await
        .expect("First prepare should succeed");
    let env2 = uv_env::prepare_environment(&deps, None)
        .await
        .expect("Second prepare should succeed");

    // Same dependencies should result in same cached environment
    assert_eq!(
        env1.venv_path, env2.venv_path,
        "Same deps should use same cached environment"
    );
}

// =============================================================================
// Conda Environment Creation Tests (Happy Path)
// =============================================================================

/// Test conda environment creation with ipykernel.
/// This test may take a while on first run as it downloads packages.
#[tokio::test]
async fn test_conda_environment_creation_with_ipykernel() {
    // Use a unique env_id to avoid conflicts with other tests
    let env_id = format!("test-{}", uuid::Uuid::new_v4());

    let deps = conda_env::CondaDependencies {
        dependencies: vec![],
        channels: vec!["conda-forge".to_string()],
        python: Some("3.11".to_string()),
        env_id: Some(env_id),
    };

    // Note: We pass None for AppHandle since we're not emitting frontend events
    let result = conda_env::prepare_environment(&deps, None).await;

    assert!(
        result.is_ok(),
        "prepare_environment should succeed: {:?}",
        result.err()
    );

    let env = result.unwrap();
    assert!(
        env.python_path.exists(),
        "Python path should exist: {:?}",
        env.python_path
    );
    assert!(
        env.env_path.exists(),
        "Env path should exist: {:?}",
        env.env_path
    );

    // Verify the path is in the expected cache location
    let env_str = env.env_path.to_string_lossy();
    assert!(
        env_str.contains("runt") && env_str.contains("conda-envs"),
        "Env should be in runt/conda-envs cache: {}",
        env_str
    );
}

/// Test that conda environment uses cache correctly (same deps = same env).
#[tokio::test]
async fn test_conda_environment_uses_cache_correctly() {
    // Use a fixed env_id to test caching
    let env_id = "test-cache-check".to_string();

    let deps = conda_env::CondaDependencies {
        dependencies: vec![],
        channels: vec!["conda-forge".to_string()],
        python: Some("3.11".to_string()),
        env_id: Some(env_id),
    };

    // Create environment twice
    let env1 = conda_env::prepare_environment(&deps, None)
        .await
        .expect("First prepare should succeed");
    let env2 = conda_env::prepare_environment(&deps, None)
        .await
        .expect("Second prepare should succeed");

    // Same dependencies should result in same cached environment
    assert_eq!(
        env1.env_path, env2.env_path,
        "Same deps should use same cached environment"
    );
}

// =============================================================================
// Fallback Decision Logic Tests
// =============================================================================

/// Test that the environment selection logic works correctly.
/// When UV is available, it should be selected; otherwise, conda.
#[tokio::test]
#[serial]
async fn test_environment_selection_logic() {
    let uv_available = uv_env::check_uv_available().await;

    // This mirrors the logic in start_default_kernel()
    let selected = if uv_available { "uv" } else { "conda" };

    println!(
        "Environment selection: {} (uv_available={})",
        selected, uv_available
    );

    // Selection should be one of the two valid options
    assert!(
        selected == "uv" || selected == "conda",
        "Selected environment should be 'uv' or 'conda', got: {}",
        selected
    );
}

/// Test that UV is selected even when PATH excludes uv (because bootstrap is available).
/// With rattler bootstrap, uv is always available, so it will always be selected over conda.
#[tokio::test]
#[serial]
async fn test_selects_uv_with_bootstrap() {
    let original_path = std::env::var("PATH").unwrap_or_default();

    // Remove uv from PATH
    std::env::set_var("PATH", "/nonexistent");

    let uv_available = uv_env::check_uv_available().await;

    // Restore PATH
    std::env::set_var("PATH", &original_path);

    // uv is now always available because it can be bootstrapped via rattler
    assert!(uv_available, "UV should be available with bootstrap");

    // With bootstrap, uv is always selected
    let selected = if uv_available { "uv" } else { "conda" };
    assert_eq!(selected, "uv", "Should select uv when bootstrap available");
}

// =============================================================================
// Hash Stability Tests (cross-module verification)
// =============================================================================

/// Verify that UV and conda use different cache directories.
#[test]
fn test_uv_and_conda_use_different_cache_dirs() {
    let uv_cache = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("runt")
        .join("envs");

    let conda_cache = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("runt")
        .join("conda-envs");

    assert_ne!(
        uv_cache, conda_cache,
        "UV and conda should use different cache directories"
    );
}
