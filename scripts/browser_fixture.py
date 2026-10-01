"""Disposable Chrome + local page shared by actual CLI and model acceptance."""
import functools
import http.server
import json
import os
import shlex
import socket
import threading
from pathlib import Path

from parity_process import capture_process_tree, finish_process_tree, run_capture

ROOT = Path(__file__).resolve().parents[1]


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


class BrowserFixture:
    def __init__(self, suh, output):
        self.suh = str(Path(suh).resolve())
        self.output = Path(output).resolve()
        self.output.mkdir(parents=True, exist_ok=True)
        self.calls = []
        self.processes = []
        # Keep the OS account environment intact: installed Windows Chrome
        # rejects remote debugging under a fabricated USERPROFILE. Isolation
        # comes from browser_start's disposable profile and our explicit port.
        self.env = dict(os.environ, AI_DEV_BROWSER_TRANSPORT='cdp', AI_DEV_BROWSER_OS_CLICK='false',
                        AI_DEV_BROWSER_OUTPUT_DIR=str(self.output))
        for name in ('AI_DEV_BROWSER_PORT', 'AI_DEV_BROWSER_TAB_URL', 'AI_DEV_BROWSER_VIEWPORT',
                     'AI_DEV_BROWSER_REDIRECT', 'AI_DEV_BROWSER_HEADLESS'):
            self.env.pop(name, None)
        with socket.socket() as reservation:
            reservation.bind(('127.0.0.1', 0))
            self.port = reservation.getsockname()[1]
        handler = functools.partial(QuietHandler, directory=str(ROOT / 'crates/sudohand-browser/tests/fixtures'))
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.url = f'http://127.0.0.1:{self.server.server_port}/steering.html'

    def __enter__(self):
        flags = ['--headless']
        if self.env.get('ADB_TEST_CHROME_ARGS'):
            overrides = dict(item.partition('=')[::2] for item in shlex.split(self.env['ADB_TEST_CHROME_ARGS']))
            flags += ['--override-default-args', json.dumps(overrides)]
        try:
            result = self.call('browser_start', *flags)
            assert result.get('pid') and not result.get('reused'), result
            self.processes = capture_process_tree(result['pid'])
            self.reset()
            return self
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def call_raw(self, tool, *args):
        command = [self.suh, 'browser', tool, '--port', str(self.port), *map(str, args)]
        result = run_capture(command, env=self.env, timeout=70)
        payload = json.loads(result.stderr if result.returncode else result.stdout)
        self.calls.append({'tool': tool, 'args': list(map(str, args)), 'exit': result.returncode, 'result': payload})
        print(f'{tool}: exit={result.returncode}', flush=True)
        return result.returncode, payload

    def call(self, tool, *args):
        code, payload = self.call_raw(tool, *args)
        assert code == 0, payload
        assert not (isinstance(payload, dict) and payload.get('error')), payload
        return payload

    def js(self, expression):
        return self.call('js_evaluate', '--expression', expression)['result']

    def reset(self):
        self.call('page_goto', '--url', self.url)

    def screenshot(self, name):
        path = self.output / name
        result = self.call('page_screenshot', '--path', path)
        assert path.is_file() and path.stat().st_size > 1000, result
        return path

    def __exit__(self, *_):
        try:
            if self.processes:
                try:
                    self.call('browser_stop')
                finally:
                    # Windows Chrome helpers can finish after Browser.close's
                    # parent exits, especially while CI compiles/runs other tests.
                    finish_process_tree(self.processes, timeout=15)
        finally:
            self.server.shutdown()
            self.server.server_close()
            self.thread.join(timeout=5)
