"""Shim entry point for MCPB packaging.

Claude Desktop runs this via its Python runtime. It delegates to the
nteract MCP server installed via uvx/pip.
"""

import subprocess
import sys

sys.exit(subprocess.call(["uvx", "--prerelease", "allow", "nteract"] + sys.argv[1:]))
