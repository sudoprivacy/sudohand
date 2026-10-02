#!/usr/bin/env python3
"""Real CLI startup must close its pipes while the launched Chrome stays alive."""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import urllib.request

from parity_process import capture_process_tree, finish_process_tree, run_capture

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--suh', type=Path, required=True)
args = parser.parse_args()
suh = str(args.suh.resolve())
with tempfile.TemporaryDirectory(prefix='suh-stdio-test-') as root:
    # browser_start already creates an isolated temporary profile. Preserve the
    # real OS account: installed Windows Chrome can launch without opening its
    # debug endpoint under a fabricated USERPROFILE (reproduced locally).
    env = dict(os.environ, AI_DEV_BROWSER_OUTPUT_DIR=root, AI_DEV_BROWSER_TRANSPORT='cdp')
    with socket.socket() as reservation:
        reservation.bind(('127.0.0.1', 0))
        port = reservation.getsockname()[1]
    command = [suh, 'browser', 'browser_start', '--port', str(port), '--headless', '--silent-stderr', '--override-default-args', '{"--no-sandbox":""}']
    child = subprocess.Popen(command, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    finished = threading.Event()
    output = []
    def read():
        output.append(child.communicate())
        finished.set()
    reader = threading.Thread(target=read, daemon=True)
    reader.start()
    processes = []
    try:
        assert finished.wait(45), 'CLI pipes stayed open while Chrome was running'
        stdout, stderr = output[0]
        assert child.returncode == 0, stderr.decode('utf-8', errors='replace')
        result = json.loads(stdout)
        assert 'error' not in result and result.get('pid') and not result.get('reused'), result
        processes = capture_process_tree(result['pid'])
        with urllib.request.urlopen(f'http://127.0.0.1:{port}/json/version', timeout=3) as response:
            assert json.load(response)['webSocketDebuggerUrl']
        print('PASS CLI stdout/stderr reach EOF while its independent Chrome is still alive')
    finally:
        if child.poll() is None:
            child.kill()
            child.wait(timeout=5)
        try:
            stopped = run_capture([suh, 'browser', 'browser_stop', '--port', str(port)], env=env, timeout=15)
            assert stopped.returncode == 0, stopped.stderr
        finally:
            finish_process_tree(processes, timeout=15)
            reader.join(timeout=5)
