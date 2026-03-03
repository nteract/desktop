"""Pytest configuration for runtimed tests.

This file contains shared fixtures and configuration.
"""

import os
import sys


def pytest_configure(config):
    """Configure pytest for runtimed tests."""
    # Add markers for different test categories
    config.addinivalue_line(
        "markers", "integration: marks tests as integration tests (may need daemon)"
    )
    config.addinivalue_line("markers", "slow: marks tests as slow running")


def pytest_collection_modifyitems(config, items):
    """Modify test collection based on environment."""
    # Auto-mark integration tests
    for item in items:
        if "daemon_integration" in item.nodeid:
            item.add_marker("integration")

    # Skip integration tests unless explicitly enabled or daemon is available
    skip_integration = False

    if os.environ.get("SKIP_INTEGRATION_TESTS", "0") == "1":
        skip_integration = True

    if skip_integration:
        import pytest

        skip_marker = pytest.mark.skip(reason="Integration tests disabled")
        for item in items:
            if "integration" in item.keywords:
                item.add_marker(skip_marker)


def pytest_report_header(config):
    """Add useful info to test report header."""
    lines = []

    # Check for daemon mode
    if os.environ.get("RUNTIMED_INTEGRATION_TEST") == "1":
        lines.append("runtimed: CI mode (will spawn daemon)")
    else:
        lines.append("runtimed: dev mode (expects running daemon)")

    # Check for custom socket path
    if "RUNTIMED_SOCKET_PATH" in os.environ:
        lines.append(f"runtimed socket: {os.environ['RUNTIMED_SOCKET_PATH']}")

    return lines
