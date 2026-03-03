"""Tests for binary discovery logic."""

import os
from unittest.mock import patch

import pytest

from runtimed._binary import BinaryNotFoundError, find_binary


def test_find_binary_via_env_var(tmp_path):
    """Binary found via environment variable override."""
    fake_bin = tmp_path / "runt"
    fake_bin.touch()
    fake_bin.chmod(0o755)

    with patch.dict(os.environ, {"RUNTIMED_RUNT_PATH": str(fake_bin)}):
        result = find_binary("runt")
        assert result == str(fake_bin)


def test_find_binary_via_path(tmp_path):
    """Binary found via system PATH."""
    fake_bin = tmp_path / "runt"
    fake_bin.touch()
    fake_bin.chmod(0o755)

    with (
        patch(
            "runtimed._binary.sysconfig.get_path",
            return_value=str(tmp_path / "no_scripts"),
        ),
        patch("shutil.which", return_value=str(fake_bin)),
    ):
        result = find_binary("runt")
        assert result == str(fake_bin)


def test_find_binary_not_found(tmp_path):
    """BinaryNotFoundError raised when binary not found anywhere."""
    with (
        patch.dict(os.environ, {}, clear=True),
        patch(
            "runtimed._binary.sysconfig.get_path",
            return_value=str(tmp_path / "no_scripts"),
        ),
        patch("shutil.which", return_value=None),
    ):
        with pytest.raises(BinaryNotFoundError, match="runt"):
            find_binary("runt")


def test_env_var_takes_precedence(tmp_path):
    """Env var path takes precedence over PATH."""
    env_bin = tmp_path / "env_runt"
    env_bin.touch()
    env_bin.chmod(0o755)

    path_bin = tmp_path / "path_runt"
    path_bin.touch()
    path_bin.chmod(0o755)

    with (
        patch.dict(os.environ, {"RUNTIMED_RUNT_PATH": str(env_bin)}),
        patch("shutil.which", return_value=str(path_bin)),
    ):
        result = find_binary("runt")
        assert result == str(env_bin)
