"""Binary discovery for runtimed Rust executables."""

from __future__ import annotations

import os
import platform
import shutil
import sys
import sysconfig


class BinaryNotFoundError(FileNotFoundError):
    """Raised when a required binary cannot be found."""

    def __init__(self, binary_name: str, searched_paths: list[str]) -> None:
        self.binary_name = binary_name
        self.searched_paths = searched_paths
        paths_str = "\n  ".join(searched_paths)
        super().__init__(
            f"Could not find '{binary_name}' binary. Searched:\n  {paths_str}\n\n"
            f"Install nteract (https://github.com/nteract/desktop/releases) "
            f"or download the binary directly.\n"
            f"Or set {_env_var(binary_name)} to the binary path."
        )


def _env_var(binary_name: str) -> str:
    """Return the environment variable name for overriding a binary path."""
    return f"RUNTIMED_{binary_name.upper()}_PATH"


def _well_known_paths(name: str) -> list[str]:
    """Return platform-specific well-known install locations for a binary."""
    paths: list[str] = []

    if sys.platform == "darwin":
        # nteract.app installs runt to /usr/local/bin via CLI install menu
        paths.append(f"/usr/local/bin/{name}")
        # runtimed daemon binary location
        home = os.path.expanduser("~")
        paths.append(
            os.path.join(home, "Library", "Application Support", "runt", "bin", name)
        )
    elif sys.platform == "linux":
        paths.append(f"/usr/local/bin/{name}")
        home = os.path.expanduser("~")
        paths.append(os.path.join(home, ".local", "share", "runt", "bin", name))
    elif sys.platform == "win32":
        local_app_data = os.environ.get("LOCALAPPDATA", "")
        if local_app_data:
            paths.append(os.path.join(local_app_data, "runt", "bin", f"{name}.exe"))

    return paths


def find_binary(name: str) -> str:
    """Find a runtimed binary by name.

    Search order:
    1. Environment variable override (RUNTIMED_RUNT_PATH, etc.)
    2. Python scripts directory (where pip/maturin install binaries)
    3. System PATH
    4. Well-known install locations (nteract.app, manual installs)

    Args:
        name: Binary name (e.g., "runt")

    Returns:
        Absolute path to the binary.

    Raises:
        BinaryNotFoundError: If the binary cannot be found.
    """
    searched: list[str] = []

    # 1. Environment variable override
    env_var = _env_var(name)
    env_path = os.environ.get(env_var)
    if env_path:
        if os.path.isfile(env_path):
            return env_path
        searched.append(f"${env_var}={env_path} (not found)")

    exe_suffix = sysconfig.get_config_var("EXE") or ""
    exe_name = name + exe_suffix

    # 2. Python scripts directory (where maturin/pip install binaries)
    scripts_path = os.path.join(sysconfig.get_path("scripts"), exe_name)
    if os.path.isfile(scripts_path):
        return scripts_path
    searched.append(f"scripts: {scripts_path}")

    # 3. System PATH
    which_result = shutil.which(name)
    if which_result:
        return which_result
    searched.append(f"PATH: {name} (not found via shutil.which)")

    # 4. Well-known install locations
    for path in _well_known_paths(name):
        if os.path.isfile(path):
            return path
        searched.append(f"well-known: {path} (not found)")

    raise BinaryNotFoundError(name, searched)
