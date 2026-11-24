#!/usr/bin/env python3
import json
import os
import sys


def request(method, request_id=None):
    message = json.loads(sys.stdin.readline())
    assert (message["method"], message.get("id")) == (method, request_id), message
    return message


def response(request_id, result):
    return (json.dumps({"id": request_id, "result": result}) + "\n").encode()


assert sys.argv[1:] == ["app-server", "--listen", "stdio://"], sys.argv
if timeout_probe := os.environ.get("SXMC_TEST_CODEX_TIMEOUT_PROBE"):
    import fcntl
    from pathlib import Path
    import signal
    import time

    probe = Path(timeout_probe)
    signal.alarm(15)
    with (probe / "app-server.lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        request("initialize", 1)
        assert os.getsid(0) == os.getpid()
        assert os.getsid(os.getppid()) != os.getsid(0)
        staging = probe / "ready.tmp"
        staging.write_text(str(Path.cwd().parent))
        staging.rename(probe / "ready")
        while probe.is_dir() and not (probe / "release").exists():
            time.sleep(0.025)
    sys.exit(0)

server_name = "sxmc-cli-ai-gh"
request("initialize", 1)
initial = response(1, {"codexHome": os.environ["CODEX_HOME"]})
config = response(2, {
    "config": {"mcp_servers": {server_name: {}}},
    "layers": [{"name": {"type": "project"}}],
})
# A single pipe write makes the second line available to the first read.
os.write(sys.stdout.fileno(), initial + config)
request("initialized")
request("config/read", 2)

request("thread/start", 3)
# The reply depends on the reader draining stderr as well as stdout.
sys.stderr.buffer.write(b"fixture stderr\n" * 100_000)
sys.stderr.buffer.flush()
os.write(sys.stdout.fileno(), response(3, {"thread": {"id": "fixture"}}))

request("mcpServerStatus/list", 4)
os.write(sys.stdout.fileno(), response(4, {
    "data": [{"name": server_name, "tools": {"get_available_skills": {}}}],
}))
call = request("mcpServer/tool/call", 5)
assert call["params"]["tool"] == "get_available_skills", call
os.write(sys.stdout.fileno(), response(5, {"content": [{"text": "simple-skill"}]}))
sys.stdin.read()
