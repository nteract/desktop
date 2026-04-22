@echo off
REM Windows plugin dispatch wrapper. Shipped as bin\nteract-mcp.cmd in each
REM of the distribution plugin repos.
REM
REM Claude Code's MCP spawn on Windows may or may not extension-hunt for
REM .cmd given a command like `${CLAUDE_PLUGIN_ROOT}/bin/nteract-mcp`. If
REM it does (Node default on Windows commonly does via PATHEXT, or via
REM cross-spawn), this wrapper runs and exec's the right .exe. If it
REM does not, the user gets a clear "command not found" error and we
REM need a `.mcp.json` tweak upstream.
REM
REM Edit scripts/plugin-dispatch-wrapper.cmd in nteract/desktop, not the
REM copy in the distribution repo — the distribution copy is overwritten
REM on every release.

setlocal
set "target=%~dp0nteract-mcp-x86_64-pc-windows-msvc.exe"
if not exist "%target%" (
  echo nteract-mcp: bundled binary not found at %target% 1>&2
  exit /b 1
)
"%target%" %*
exit /b %ERRORLEVEL%
