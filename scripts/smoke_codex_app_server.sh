#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd -P)"

BIN_ARG="${1:-${REPO_ROOT}/target/debug/sxmc}"
FIXTURES_ARG="${2:-${REPO_ROOT}/tests/fixtures}"
PROFILE_ARG="${3:-${REPO_ROOT}/examples/profiles/from_cli.json}"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Required command not found: $1" >&2
    exit 1
  fi
}

require_cmd codex
require_cmd python3

CANON_BIN="$(python3 -c 'import pathlib, sys; print(pathlib.Path(sys.argv[1]).resolve(strict=True))' "${BIN_ARG}")"
CANON_FIXTURES="$(python3 -c 'import pathlib, sys; print(pathlib.Path(sys.argv[1]).resolve(strict=True))' "${FIXTURES_ARG}")"
CANON_PROFILE="$(python3 -c 'import pathlib, sys; print(pathlib.Path(sys.argv[1]).resolve(strict=True))' "${PROFILE_ARG}")"

if [[ ! -x "${CANON_BIN}" ]]; then
  echo "Expected executable sxmc binary at ${CANON_BIN}" >&2
  exit 1
fi
if [[ ! -d "${CANON_FIXTURES}/simple-skill" ]]; then
  echo "Expected simple-skill fixture at ${CANON_FIXTURES}/simple-skill" >&2
  exit 1
fi

RAW_TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/sxmc-codex-smoke.XXXXXXXX")"
TMPDIR="$(python3 -c 'import pathlib, sys; print(pathlib.Path(sys.argv[1]).resolve(strict=True))' "${RAW_TMPDIR}")"
REAL_HOME_CANON="$(python3 -c 'import os, pathlib; print(pathlib.Path(os.path.expanduser("~")).resolve())')"

if [[ -z "${TMPDIR}" || "${TMPDIR}" != /* || "${TMPDIR}" == //* || "${TMPDIR}" == "/" || "${TMPDIR}" == "${REAL_HOME_CANON}" ]]; then
  echo "Refusing unsafe temp directory path: ${TMPDIR}" >&2
  exit 1
fi

cleanup() {
  local exit_code=$?
  if [[ -n "${TMPDIR}" && "${TMPDIR}" == /* && "${TMPDIR}" != //* && "${TMPDIR}" != "/" && "${TMPDIR}" != "${REAL_HOME_CANON}" ]]; then
    rm -rf "${TMPDIR}" # lessons: allow L1 canonicalized mktemp dir guarded against empty, relative, /, //, and $HOME
  fi
  exit "${exit_code}"
}
trap cleanup EXIT

FAKE_HOME="${TMPDIR}/home"
FAKE_XDG_CONFIG="${TMPDIR}/xdg-config"
FAKE_XDG_CACHE="${TMPDIR}/xdg-cache"
FAKE_SXMC_CONFIG="${TMPDIR}/sxmc-config"
FAKE_SXMC_CACHE="${TMPDIR}/sxmc-cache"
FAKE_CODEX_HOME="${TMPDIR}/codex-home"
FAKE_BIN="${TMPDIR}/bin"

mkdir -p "${FAKE_HOME}" "${FAKE_XDG_CONFIG}" "${FAKE_XDG_CACHE}" "${FAKE_SXMC_CONFIG}" "${FAKE_SXMC_CACHE}" "${FAKE_CODEX_HOME}" "${FAKE_BIN}"
ln -s "${CANON_BIN}" "${FAKE_BIN}/sxmc"

export HOME="${FAKE_HOME}"
export USERPROFILE="${FAKE_HOME}"
export XDG_CONFIG_HOME="${FAKE_XDG_CONFIG}"
export XDG_CACHE_HOME="${FAKE_XDG_CACHE}"
export SXMC_CONFIG_HOME="${FAKE_SXMC_CONFIG}"
export SXMC_CACHE_HOME="${FAKE_SXMC_CACHE}"
export CODEX_HOME="${FAKE_CODEX_HOME}"
export PATH="${FAKE_BIN}:${PATH}"
unset OPENAI_API_KEY CODEX_API_KEY AZURE_OPENAI_API_KEY

run_codex_smoke_for_project() {
  local label="$1"
  local project_dir="$2"
  local expected_server="$3"

  echo "Codex app-server host smoke (${label}): ${project_dir}"
  python3 - "${project_dir}" "${expected_server}" <<'PY'
import json
import os
import selectors
import signal
import subprocess
import sys
import time

project_dir = sys.argv[1]
expected_server = sys.argv[2]

proc = None
stop_requested = False
selector = selectors.DefaultSelector()
stdout_pending = bytearray()
stderr_tail = b""

def stop_app_server(app_server):
    try:
        os.killpg(app_server.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass

def request_stop(_signum, _frame):
    global stop_requested
    stop_requested = True
    if proc is not None:
        stop_app_server(proc)

def check_stop():
    if stop_requested:
        raise SystemExit(128 + signal.SIGTERM)

def send(payload):
    check_stop()
    proc.stdin.write((json.dumps(payload) + "\n").encode())
    proc.stdin.flush()

def read_response(request_id, timeout=15.0):
    global stderr_tail
    deadline = time.monotonic() + timeout
    while (remaining := deadline - time.monotonic()) > 0:
        check_stop()
        newline = stdout_pending.find(b"\n")
        if newline != -1:
            line = stdout_pending[:newline]
            del stdout_pending[:newline + 1]
            msg = json.loads(line)
            if msg.get("id") == request_id:
                if "error" in msg:
                    raise RuntimeError(f"JSON-RPC error for id {request_id}: {msg['error']}")
                return msg["result"]
            continue
        if not selector.get_map():
            raise RuntimeError(
                f"codex app-server closed its pipes before response {request_id}: "
                f"{stderr_tail.decode(errors='replace')}"
            )
        for key, _ in selector.select(remaining):
            try:
                chunk = os.read(key.fd, 65536)
            except BlockingIOError:
                continue
            if not chunk:
                selector.unregister(key.fileobj)
            elif key.fileobj is proc.stdout:
                stdout_pending.extend(chunk)
            else:
                stderr_tail = (stderr_tail + chunk)[-8192:]
    raise TimeoutError(
        f"Timed out waiting for JSON-RPC response id={request_id}: "
        f"{stderr_tail.decode(errors='replace')}"
    )

signal.signal(signal.SIGTERM, request_stop)
try:
    proc = subprocess.Popen(
        ["codex", "app-server", "--listen", "stdio://"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
        cwd=project_dir,
    )
    check_stop()
    for pipe in (proc.stdout, proc.stderr):
        os.set_blocking(pipe.fileno(), False)
        selector.register(pipe, selectors.EVENT_READ)
    send({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "clientInfo": {"name": "sxmc-smoke", "version": "0.1.0"},
            "capabilities": {"experimentalApi": True},
        },
    })
    init_res = read_response(1)
    assert init_res.get("codexHome") == os.environ["CODEX_HOME"], init_res

    send({"jsonrpc": "2.0", "method": "initialized"})

    send({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "config/read",
        "params": {"cwd": project_dir, "includeLayers": True},
    })
    cfg_res = read_response(2)
    mcp_servers = cfg_res.get("config", {}).get("mcp_servers", {})
    assert expected_server in mcp_servers, f"missing {expected_server} in config/read: {mcp_servers}"
    layers = cfg_res.get("layers", [])
    assert any(
        layer.get("name", {}).get("type") == "project"
        for layer in layers
    ), f"expected project config layer in {layers}"

    send({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "thread/start",
        "params": {"cwd": project_dir, "ephemeral": True},
    })
    thread_res = read_response(3)
    thread_id = thread_res["thread"]["id"]

    send({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "mcpServerStatus/list",
        "params": {"threadId": thread_id, "serverName": expected_server},
    })
    status_res = read_response(4)
    servers = status_res.get("data", [])
    matching = [s for s in servers if s.get("name") == expected_server]
    assert matching, f"expected {expected_server} in mcpServerStatus/list: {servers}"
    tools = matching[0].get("tools", {})
    assert "get_available_skills" in tools, f"expected get_available_skills tool in {list(tools.keys())}"

    send({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "mcpServer/tool/call",
        "params": {
            "threadId": thread_id,
            "server": expected_server,
            "tool": "get_available_skills",
            "arguments": {},
        },
    })
    call_res = read_response(5)
    assert not call_res.get("isError"), f"tool call returned error: {call_res}"
    content_text = "\n".join(
        item.get("text", "") for item in call_res.get("content", []) if isinstance(item, dict)
    )
    assert "simple-skill" in content_text, f"expected simple-skill in tool output: {content_text}"
    print(f"Verified {expected_server} discovery and get_available_skills call via codex app-server.")
finally:
    # The signal handler may stop the group until cleanup takes ownership and reaps it.
    app_server, proc = proc, None
    try:
        if app_server is not None:
            stop_app_server(app_server)
            app_server.wait(timeout=5)
    finally:
        selector.close()
        if app_server is not None:
            for pipe in (app_server.stdin, app_server.stdout, app_server.stderr):
                pipe.close()
PY
}

# 1. Fresh trusted project
PROJ_FRESH="${TMPDIR}/proj-fresh"
mkdir -p "${PROJ_FRESH}/.claude/skills"
cp -R "${CANON_FIXTURES}/simple-skill" "${PROJ_FRESH}/.claude/skills/simple-skill"

# 2. Upgrade trusted project (pre-seeded with legacy .codex/mcp.toml)
PROJ_UPGRADE="${TMPDIR}/proj-upgrade"
mkdir -p "${PROJ_UPGRADE}/.claude/skills" "${PROJ_UPGRADE}/.codex"
cp -R "${CANON_FIXTURES}/simple-skill" "${PROJ_UPGRADE}/.claude/skills/simple-skill"

cat >"${FAKE_CODEX_HOME}/config.toml" <<EOF
[projects."${PROJ_FRESH}"]
trust_level = "trusted"

[projects."${PROJ_UPGRADE}"]
trust_level = "trusted"
EOF

"${CANON_BIN}" scaffold client-config \
  --from-profile "${CANON_PROFILE}" \
  --client openai-codex \
  --root "${PROJ_FRESH}" \
  --mode apply >/dev/null

if [[ ! -f "${PROJ_FRESH}/.codex/config.toml" || -f "${PROJ_FRESH}/.codex/mcp.toml" ]]; then
  echo "Expected ${PROJ_FRESH}/.codex/config.toml without legacy mcp.toml" >&2
  exit 1
fi

run_codex_smoke_for_project "fresh" "${PROJ_FRESH}" "sxmc-cli-ai-gh"

LEGACY_MCP_CONTENT=$(cat <<EOF
# sxmc:begin cli-ai:openai-codex
# sxmc CLI->AI startup scaffold
[mcp_servers.sxmc-cli-ai-legacy]
command = "sxmc"
args = ["serve", "--paths", "${PROJ_UPGRADE}/.claude/skills"]
# sxmc:end cli-ai:openai-codex
EOF
)
printf '%s\n' "${LEGACY_MCP_CONTENT}" >"${PROJ_UPGRADE}/.codex/mcp.toml"
LEGACY_BEFORE_SHA="$(python3 -c 'import hashlib, pathlib, sys; print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())' "${PROJ_UPGRADE}/.codex/mcp.toml")"

"${CANON_BIN}" scaffold client-config \
  --from-profile "${CANON_PROFILE}" \
  --client openai-codex \
  --root "${PROJ_UPGRADE}" \
  --mode apply >/dev/null

if [[ ! -f "${PROJ_UPGRADE}/.codex/config.toml" || ! -f "${PROJ_UPGRADE}/.codex/mcp.toml" ]]; then
  echo "Expected upgraded ${PROJ_UPGRADE}/.codex/config.toml and preserved ${PROJ_UPGRADE}/.codex/mcp.toml" >&2
  exit 1
fi

LEGACY_AFTER_SHA="$(python3 -c 'import hashlib, pathlib, sys; print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())' "${PROJ_UPGRADE}/.codex/mcp.toml")"
if [[ "${LEGACY_BEFORE_SHA}" != "${LEGACY_AFTER_SHA}" ]]; then
  echo "Expected legacy ${PROJ_UPGRADE}/.codex/mcp.toml bytes to remain unchanged" >&2
  exit 1
fi

run_codex_smoke_for_project "upgrade" "${PROJ_UPGRADE}" "sxmc-cli-ai-gh"

echo "Codex app-server host smoke checks passed."
