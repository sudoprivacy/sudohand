#!/usr/bin/env python3
"""Real Chrome regression observations for the pinned migration audit.

Two disposable browser journeys: save a draft, reload, read its visible value.
Records the known Python storage failure separately from the desired Rust result.
This is an audit of two fixed revisions, not a claim of general parity.
No skips, mocks, personal profiles, external services or API keys are used.
"""

import argparse
import hashlib
import http.server
import json
import os
import socket
import subprocess
import sys
import threading
from pathlib import Path

ADB = "c349d347251779357136685b6a698e3d2c59ce6d"
RUST = "045202ed16e58527743d00006ddececf79471a93"
HTML = b"""<!doctype html><meta charset=utf-8><title>Draft recovery</title>
<h1>Saved draft</h1><output id=draft></output><script>
document.querySelector('#draft').textContent=localStorage.getItem('draft')||'EMPTY';
</script>"""


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(HTML)))
        self.end_headers()
        self.wfile.write(HTML)

    def log_message(self, *_):
        pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reference", required=True, type=Path)
    parser.add_argument("--suh-repo", required=True, type=Path)
    parser.add_argument("--suh", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    for repo, sha in ((args.reference, ADB), (args.suh_repo, RUST)):
        actual = subprocess.check_output(
            ["git", "-C", str(repo), "rev-parse", "HEAD"], text=True
        ).strip()
        assert actual == sha, (actual, sha)
    env = dict(
        os.environ,
        PYTHONUTF8="1",
        PYTHONIOENCODING="utf-8",
        AI_DEV_BROWSER_TRANSPORT="cdp",
        AI_DEV_BROWSER_OS_CLICK="false",
    )
    for key in (
        "AI_DEV_BROWSER_PORT",
        "AI_DEV_BROWSER_TAB_URL",
        "AI_DEV_BROWSER_VIEWPORT",
        "AI_DEV_BROWSER_REDIRECT",
    ):
        env.pop(key, None)
    report = {
        "schema": 1,
        "adb": ADB,
        "rust_checkout": RUST,
        "suh_binary_sha256": hashlib.sha256(args.suh.read_bytes()).hexdigest(),
        "environment": "Real Chrome, disposable profiles, local HTTP fixture, independent CLI processes",
        "scope": "Storage draft persistence; known baseline defect is an expected audit observation, not desired replacement behavior",
        "calls": [],
        "observations": {},
    }

    def call(backend, tool, port, *flags):
        prefix = (
            [str(args.suh.resolve()), "browser", tool]
            if backend == "rust"
            else [sys.executable, "-m", "ai_dev_browser.tools." + tool]
        )
        result = subprocess.run(
            prefix + ["--port", str(port), *map(str, flags)],
            cwd=args.reference,
            env=env,
            capture_output=True,
            encoding="utf-8",
            timeout=70,
            check=False,
        )
        value = json.loads(result.stdout) if result.stdout.strip() else None
        # Only retain storage/DOM values, never machine-specific browser/profile paths.
        record = {"backend": backend, "tool": tool, "exit": result.returncode}
        if tool in ("storage_set", "storage_get", "js_evaluate"):
            record["result"] = value
        report["calls"].append(record)
        print(f"{backend} {tool}: exit={result.returncode}", flush=True)
        return result.returncode, value

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        for backend in ("python", "rust"):
            with socket.socket() as sock:
                sock.bind(("127.0.0.1", 0))
                port = sock.getsockname()[1]
            started = False
            try:
                code, launch = call(
                    backend,
                    "browser_start",
                    port,
                    "--headless",
                    "true",
                    "--match-proxy",
                    "false",
                )
                started = bool(launch and launch.get("pid"))
                assert code == 0 and started, launch
                assert (
                    call(
                        backend,
                        "page_goto",
                        port,
                        "--url",
                        f"http://127.0.0.1:{server.server_port}/",
                    )[0]
                    == 0
                )
                set_code, set_value = call(
                    backend,
                    "storage_set",
                    port,
                    "--key",
                    "draft",
                    "--value",
                    "migration-draft",
                )
                assert call(backend, "page_reload", port)[0] == 0
                get_code, get_value = call(
                    backend, "storage_get", port, "--key", "draft"
                )
                ui_code, ui = call(
                    backend,
                    "js_evaluate",
                    port,
                    "--expression",
                    "document.querySelector('#draft').textContent",
                )
                assert ui_code == 0
                if backend == "python":
                    assert set_code != 0 and "set_local_storage" in set_value.get(
                        "error", ""
                    ), set_value
                    assert get_code != 0 and "get_local_storage" in get_value.get(
                        "error", ""
                    ), get_value
                    assert ui["result"] == "EMPTY", ui
                    report["observations"][backend] = (
                        "Confirmed baseline bug: obsolete storage methods; UI remains EMPTY after reload"
                    )
                else:
                    assert set_code == get_code == 0, (set_value, get_value)
                    assert get_value["value"] == ui["result"] == "migration-draft", (
                        get_value,
                        ui,
                    )
                    report["observations"][backend] = (
                        "Draft survives reload and is visible in the page; intentional correction passes this journey"
                    )
            finally:
                if started:
                    assert call(backend, "browser_stop", port)[0] == 0
    finally:
        server.shutdown()
        server.server_close()
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(
            json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
    print("PASS: pinned storage observations reproduced in real Chrome", flush=True)


if __name__ == "__main__":
    main()
