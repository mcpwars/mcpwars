#!/bin/sh
# One agent turn through a third-party coding CLI, for the orchestrator's
# `command` backend. Each pilot gets its own working directory holding a
# project-local MCP config with its own token, so nothing global is edited.
#
#   command = ["scripts/pilot-cli.sh", "kimi", "{model}", "/abs/pilots/{name}", "{prompt}"]
#
# The orchestrator exports AF_MCP_URL (the pilot's token URL).
#
# kimi and grok only load project-local MCP servers from folders they trust:
# mark each pilot directory trusted once (kimi: ~/.kimi-code/workspace-trust,
# grok: ~/.grok/trusted_folders.toml). agy reads ./.mcp.json without that.
set -eu
cli=$1 model=$2 dir=$3 prompt=$4
: "${AF_MCP_URL:?AF_MCP_URL must be set}"
mkdir -p "$dir"
cd "$dir"
case $cli in
kimi)
  mkdir -p .kimi-code
  printf '{"mcpServers":{"agentic-frontier":{"url":"%s"}}}\n' "$AF_MCP_URL" > .kimi-code/mcp.json
  exec kimi -p "$prompt" -m "$model"
  ;;
agy)
  printf '{"mcpServers":{"agentic-frontier":{"type":"http","url":"%s"}}}\n' "$AF_MCP_URL" > .mcp.json
  exec agy -p "$prompt" --dangerously-skip-permissions --model "$model"
  ;;
grok)
  mkdir -p .grok
  printf '[mcp_servers.agentic-frontier]\nurl = "%s"\nenabled = true\n' "$AF_MCP_URL" > .grok/config.toml
  exec grok -p "$prompt" -m "$model" --always-approve --disable-web-search \
    --disallowed-tools run_terminal_command,read_file,write,search_replace,list_dir,grep,spawn_subagent,image_gen,image_edit,image_to_video,reference_to_video
  ;;
*)
  echo "unknown cli: $cli (expected kimi, agy or grok)" >&2
  exit 2
  ;;
esac
