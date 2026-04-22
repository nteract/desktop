#!/usr/bin/env bash
# Assemble a distribution tree for one of the Claude Code plugin repos.
#
# Inputs:
#   --channel {stable|nightly}     which plugin to stage
#   --binaries-dir <path>          directory containing nteract-mcp-<target>{,.exe}
#   --out-dir <path>               destination (will be wiped and recreated)
#   --marketplace-name <name>      name field in generated marketplace.json (default: "notebook")
#
# Output layout (under --out-dir):
#   README.md                                     (generated)
#   .claude-plugin/marketplace.json              (generated)
#   plugins/<plugin_name>/
#     .mcp.json                                   (copied verbatim from source repo)
#     .claude-plugin/plugin.json                  (copied verbatim)
#     .codex-plugin/plugin.json                   (copied verbatim, forward-compat)
#     skills/...                                  (copied verbatim)
#     bin/nteract-mcp                             (dispatch wrapper)
#     bin/nteract-mcp-aarch64-apple-darwin        (per-target binary)
#     bin/nteract-mcp-x86_64-apple-darwin
#     bin/nteract-mcp-x86_64-unknown-linux-gnu
#     bin/nteract-mcp-x86_64-pc-windows-msvc.exe
#
# The resulting tree is what gets force-pushed to main of the target
# distribution repo. Users install via the marketplace-name ("notebook"
# by default), not the repo name — so /plugin install nteract@notebook
# works regardless of whether the repo is nteract/claude-plugin or
# nteract/claude-plugin-nightly.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

channel=""
binaries_dir=""
out_dir=""
marketplace_name=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --channel) channel="$2"; shift 2 ;;
    --binaries-dir) binaries_dir="$2"; shift 2 ;;
    --out-dir) out_dir="$2"; shift 2 ;;
    --marketplace-name) marketplace_name="$2"; shift 2 ;;
    -h|--help)
      sed -n '2,25p' "$0"
      exit 0
      ;;
    *)
      echo "unknown arg: $1" >&2
      exit 2
      ;;
  esac
done

[[ -n "$channel" ]] || { echo "--channel required" >&2; exit 2; }
[[ -n "$binaries_dir" ]] || { echo "--binaries-dir required" >&2; exit 2; }
[[ -n "$out_dir" ]] || { echo "--out-dir required" >&2; exit 2; }

# Marketplace name defaults to the plugin name for the channel. It's what
# users see when they run `/plugin marketplace add <repo>` and what they
# type after `@` in `/plugin install <plugin>@<marketplace>`. Calling it
# "notebook" overloaded the label with the MCP server's name (which IS
# "notebook" — that one's correct) and made the install confusing.
case "$channel" in
  stable)
    plugin_name="nteract"
    ;;
  nightly)
    plugin_name="nteract-nightly"
    ;;
  *)
    echo "unknown channel '$channel' (expected stable|nightly)" >&2
    exit 2
    ;;
esac

[[ -n "$marketplace_name" ]] || marketplace_name="$plugin_name"

source_plugin="$REPO_ROOT/plugins/$plugin_name"
[[ -d "$source_plugin" ]] || { echo "source plugin not found: $source_plugin" >&2; exit 1; }

# Per-release targets. Must match plugin-dispatch-wrapper.js TARGETS map.
declare -a TARGETS=(
  "aarch64-apple-darwin"
  "x86_64-apple-darwin"
  "x86_64-unknown-linux-gnu"
  "x86_64-pc-windows-msvc"
)

# Verify every binary is present before we touch out_dir.
missing=()
for t in "${TARGETS[@]}"; do
  if [[ "$t" == *windows* ]]; then
    candidate="$binaries_dir/nteract-mcp-$t.exe"
  else
    candidate="$binaries_dir/nteract-mcp-$t"
  fi
  [[ -f "$candidate" ]] || missing+=("$candidate")
done
if (( ${#missing[@]} > 0 )); then
  echo "missing binaries:" >&2
  printf '  %s\n' "${missing[@]}" >&2
  exit 1
fi

# Clean + recreate output tree.
rm -rf "$out_dir"
mkdir -p "$out_dir/plugins/$plugin_name/bin"

# Copy plugin manifests + skills verbatim.
for item in .mcp.json .claude-plugin .codex-plugin skills assets; do
  if [[ -e "$source_plugin/$item" ]]; then
    cp -R "$source_plugin/$item" "$out_dir/plugins/$plugin_name/"
  fi
done

# Drop the source-repo .gitignore from bin/ (distribution repo wants
# binaries tracked).
rm -f "$out_dir/plugins/$plugin_name/bin/.gitignore"

# Copy binaries with target-suffixed names.
for t in "${TARGETS[@]}"; do
  if [[ "$t" == *windows* ]]; then
    src="$binaries_dir/nteract-mcp-$t.exe"
    dest="$out_dir/plugins/$plugin_name/bin/nteract-mcp-$t.exe"
  else
    src="$binaries_dir/nteract-mcp-$t"
    dest="$out_dir/plugins/$plugin_name/bin/nteract-mcp-$t"
  fi
  cp "$src" "$dest"
  chmod 0755 "$dest"
done

# Copy the two dispatch wrappers. Unix (POSIX sh) + Windows (batch).
# Both .exec the right sibling binary — no long-lived parent process,
# signals and exit codes are transparent.
cp "$REPO_ROOT/scripts/plugin-dispatch-wrapper.sh" \
   "$out_dir/plugins/$plugin_name/bin/nteract-mcp"
chmod 0755 "$out_dir/plugins/$plugin_name/bin/nteract-mcp"

cp "$REPO_ROOT/scripts/plugin-dispatch-wrapper.cmd" \
   "$out_dir/plugins/$plugin_name/bin/nteract-mcp.cmd"

# Generate marketplace.json. "name" is the install-time identifier —
# users type /plugin install <plugin_name>@${marketplace_name}.
mkdir -p "$out_dir/.claude-plugin"

if [[ "$channel" == "stable" ]]; then
  plugin_description="Jupyter notebooks in Claude Code."
else
  plugin_description="Jupyter notebooks in Claude Code (nightly channel)."
fi

cat > "$out_dir/.claude-plugin/marketplace.json" <<JSON
{
  "name": "${marketplace_name}",
  "owner": { "name": "nteract" },
  "plugins": [
    {
      "name": "${plugin_name}",
      "source": "./plugins/${plugin_name}",
      "description": "${plugin_description}",
      "version": "0.1.0"
    }
  ]
}
JSON

# Generate README. Auto-generated repos shouldn't accept PRs.
cat > "$out_dir/README.md" <<MARKDOWN
# nteract Claude Code plugin (${channel})

This repository is auto-generated by the [nteract/desktop](https://github.com/nteract/desktop) release pipeline. **Do not open pull requests here** — edits will be overwritten on the next release.

## Install

\`\`\`
/plugin marketplace add $( [[ "$channel" == "stable" ]] && echo "nteract/claude-plugin" || echo "nteract/claude-plugin-nightly" )
/plugin install ${plugin_name}@${marketplace_name}
\`\`\`

## Pin to a specific version

\`\`\`
/plugin install ${plugin_name}@${marketplace_name} --ref v2.3.0
\`\`\`

Tags are published per release. \`main\` always points at the latest ${channel} release.

## What lives here

- \`plugins/${plugin_name}/bin/nteract-mcp\` — Node dispatch wrapper
- \`plugins/${plugin_name}/bin/nteract-mcp-<target>\` — per-platform binaries
- \`plugins/${plugin_name}/.mcp.json\` and friends — plugin manifests
- \`plugins/${plugin_name}/skills/\` — plugin skills

Source: <https://github.com/nteract/desktop/tree/main/plugins/${plugin_name}>
MARKDOWN

echo "assembled ${channel} distribution at $out_dir"
