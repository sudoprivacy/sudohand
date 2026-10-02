#!/usr/bin/env python3
"""Reproduce premature download success with real Chrome and a delayed response.

Opt-in diagnostic. Exit 1 means at least one backend returned success before the
requested file existed; this is a migration failure, not a passing acceptance.
Chrome uses isolated profiles and an explicit temporary fallback download folder.
"""

import argparse
import hashlib
import http.server
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from parity_process import run_capture, capture_process_tree, finish_process_tree


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        data = (
            b"<html><title>Download completion probe</title><body>fixture</body></html>"
        )
        if self.path.endswith(".bin"):
            time.sleep(1)
            data = b"controlled download\x00\xff"
        self.send_response(200)
        self.send_header(
            "Content-Type",
            "application/octet-stream" if self.path.endswith(".bin") else "text/html",
        )
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--suh", type=Path, required=True)
parser.add_argument("--chrome", type=Path, required=True)
parser.add_argument("--reference", type=Path, required=True)
parser.add_argument("--legacy-reference", type=Path)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
args.output.parent.mkdir(parents=True, exist_ok=True)
backends = [("python-current", args.reference), ("rust", None)]
if args.legacy_reference:
    backends.insert(0, ("python-old", args.legacy_reference))
server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
records = []
try:
    with tempfile.TemporaryDirectory(prefix="suh-download-probe-") as temp:
        root = Path(temp)
        for backend, ref in backends:
            profile = root / backend / "profile"
            (profile / "Default").mkdir(parents=True)
            fallback = root / backend / "fallback"
            fallback.mkdir()
            expected = root / backend / "expected"
            expected.mkdir()
            (profile / "Default/Preferences").write_text(
                json.dumps(
                    {
                        "download": {
                            "default_directory": str(fallback),
                            "prompt_for_download": False,
                        }
                    }
                )
            )
            with socket.socket() as s:
                s.bind(("127.0.0.1", 0))
                port = s.getsockname()[1]
            chrome = subprocess.Popen(
                [
                    str(args.chrome.resolve()),
                    f"--user-data-dir={profile}",
                    f"--remote-debugging-port={port}",
                    "--headless=new",
                    "--no-first-run",
                    "--no-default-browser-check",
                    "about:blank",
                ],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            processes = capture_process_tree(chrome.pid)
            env = dict(
                os.environ,
                PYTHONIOENCODING="utf-8",
                AI_DEV_BROWSER_TRANSPORT="cdp",
                AI_DEV_BROWSER_OS_CLICK="false",
            )
            if ref:
                env["PYTHONPATH"] = str(Path(ref).resolve())

            def call(tool, *flags, impl=backend):
                prefix = (
                    [str(args.suh.resolve()), "browser", tool]
                    if impl == "rust"
                    else [sys.executable, "-m", f"ai_dev_browser.tools.{tool}"]
                )
                run = run_capture(
                    prefix + ["--port", str(port), *map(str, flags)],
                    env=env,
                    timeout=40,
                )
                assert run.returncode == 0, (run.stdout, run.stderr)
                return json.loads(run.stdout)

            try:
                deadline = time.monotonic() + 15
                while True:
                    try:
                        urllib.request.urlopen(
                            f"http://127.0.0.1:{port}/json/version", timeout=1
                        ).close()
                        break
                    except OSError:
                        assert time.monotonic() < deadline
                        time.sleep(0.1)
                url = f"http://127.0.0.1:{server.server_port}"
                call("page_goto", "--url", url, impl="rust")
                before = time.monotonic()
                result = call(
                    "download", "--url", url + "/slow.bin", "--path", expected
                )
                duration = time.monotonic() - before
                immediate = [p.name for p in expected.iterdir()]
                complete_at_return = (expected / "slow.bin").is_file() and (
                    expected / "slow.bin"
                ).read_bytes() == b"controlled download\x00\xff"
                time.sleep(2)
                call("tab_new", "--url", "chrome://downloads/", impl="rust")
                verdict = call(
                    "js_evaluate",
                    "--tab-url",
                    "chrome://downloads/",
                    "--expression",
                    'Array.from(document.querySelector("downloads-manager")?.shadowRoot?.querySelectorAll("downloads-item")||[],e=>({name:e.data?.fileName,path:e.data?.filePath,state:e.data?.state,danger:e.data?.dangerType}))',
                    impl="rust",
                )["result"]
                files = [
                    {
                        "relative": str(p.relative_to(root / backend)),
                        "bytes": p.read_bytes().hex(),
                    }
                    for d in (expected, fallback)
                    for p in d.iterdir()
                    if p.is_file()
                ]
                record = {
                    "backend": backend,
                    "reference_commit": subprocess.check_output(
                        ["git", "-C", str(ref), "rev-parse", "HEAD"], text=True
                    ).strip()
                    if ref
                    else None,
                    "binary_sha256": hashlib.sha256(args.suh.read_bytes()).hexdigest(),
                    "duration": duration,
                    "result": result,
                    "files_at_return": immediate,
                    "files_after_wait": files,
                    "download_history": verdict,
                }
                record["returned_success_before_file"] = (
                    result.get("success") is True and not complete_at_return
                )
                record["download_accepted"] = (
                    result.get("success") is True and complete_at_return
                )
                records.append(record)
                print(json.dumps(record), flush=True)
            finally:
                processes = capture_process_tree(chrome.pid) or processes
                try:
                    call("browser_stop", impl="rust")
                finally:
                    finish_process_tree(processes, timeout=15)
finally:
    server.shutdown()
    server.server_close()
    args.output.write_text(json.dumps(records, indent=2) + "\n", encoding="utf-8")

raise SystemExit(1 if any(not r["download_accepted"] for r in records) else 0)
