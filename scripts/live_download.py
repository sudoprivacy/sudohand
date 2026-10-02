#!/usr/bin/env python3
"""Real Chrome: discover a URL, save it, upload the returned file, see its receipt.

Also covers delayed bodies, empty files, HTTP failures and aborted fetches. This
asserts the desired contract; the pinned Python implementations have a documented
premature-success bug (live_download_completion_probe.py preserves that comparison).
"""
import argparse
import hashlib
import http.server
import json
import threading
import time
from pathlib import Path

from browser_fixture import BrowserFixture

BODY = b'controlled download\x00\xff'
HTML = b'''<!doctype html><meta charset="utf-8"><title>Download and import</title>
<h1>Import your export</h1><p><a id="export" href="/slow.bin">Export binary</a></p>
<label>Import file <input type="file" id="import"></label><pre id="receipt">Waiting for import</pre>
<script>document.querySelector('#import').onchange=async e=>{
 const file=e.target.files[0]; const data=Array.from(new Uint8Array(await file.arrayBuffer()));
 window.imported={name:file.name,bytes:file.size,data};
 document.querySelector('#receipt').textContent='Imported '+file.name+': '+file.size+' bytes';
};</script>'''


class DownloadServer:
    def __enter__(self):
        self.requests = []
        self.stop = threading.Event()
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                fixture.requests.append(self.path)
                path = self.path.split('?')[0]
                status = 404 if path == '/missing.bin' else 200
                body = HTML if path == '/' else b'' if path == '/empty.bin' else BODY
                self.send_response(status)
                self.send_header('Content-Type', 'text/html' if path == '/' else 'application/octet-stream')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                try:
                    if path == '/slow.bin':
                        self.wfile.write(body[:4])
                        self.wfile.flush()
                        fixture.stop.wait(1.5)
                        self.wfile.write(body[4:])
                    elif path == '/stalled.bin':
                        self.wfile.write(body[:4])
                        self.wfile.flush()
                        fixture.stop.wait(35)
                        self.wfile.write(body[4:])
                    else:
                        self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                    pass

            def log_message(self, *_):
                pass

        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.url = f'http://127.0.0.1:{self.server.server_port}'
        return self

    def __exit__(self, *_):
        self.stop.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


def check_saved(result, directory, expected):
    assert result['success'] is True, result
    path = Path(result['path'])
    assert path.is_absolute() and path.parent == directory.resolve(), result
    assert path.name == result['filename'] and result['bytes'] == len(expected), result
    # No poll/sleep: a consumer can immediately read or upload the returned path.
    assert path.read_bytes() == expected, result
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suh', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    report = {'status': 'failed', 'binary_sha256': hashlib.sha256(args.suh.read_bytes()).hexdigest()}
    with DownloadServer() as server, BrowserFixture(args.suh, output) as browser:
        try:
            browser.call('page_goto', '--url', server.url)
            export_url = browser.js('document.querySelector("#export").href')
            destination = output / 'exports.v1' / '\u4e0b\u8f7d'
            start = time.monotonic()
            result = browser.call('download', '--url', export_url, '--path', destination)
            saved = check_saved(result, destination, BODY)
            report['slow_download_seconds'] = time.monotonic() - start
            elements = browser.call('page_discover', '--no-interactable-only')
            targets = [e['ref'] for e in elements if e.get('name', '').strip() == 'Import file' and e.get('role') == 'button']
            assert len(targets) == 1, elements
            target = targets[0]
            browser.call('upload_by_ref', '--ref', target, '--paths', saved)
            receipt = None
            for _ in range(20):
                receipt = browser.js('window.imported || null')
                if receipt:
                    break
                time.sleep(.05)
            assert receipt == {'name': 'slow.bin', 'bytes': len(BODY), 'data': list(BODY)}, receipt
            browser.screenshot('import-receipt.png')
            assert server.requests.count('/slow.bin') == 1, server.requests
            report['receipt'] = receipt
            # A second same-name export must return the file Chrome actually saved.
            check_saved(browser.call('download', '--url', export_url, '--path', destination), destination, BODY)
            assert server.requests.count('/slow.bin') == 2, server.requests
            check_saved(browser.call('download', '--url', server.url + '/empty.bin', '--path', destination), destination, b'')
            before = {p.name: p.read_bytes() for p in destination.iterdir()}
            for route, marker in [('/missing.bin', 'HTTP 404'), ('/stalled.bin', 'abort')]:
                code, payload = browser.call_raw('download', '--url', server.url + route, '--path', destination)
                assert code == 9 and payload['error']['retryable'] is False, payload
                assert marker.lower() in payload['error']['message'].lower(), payload
                assert 'before retrying' in payload['error']['hint'], payload
                assert server.requests.count(route) == 1, server.requests
                after = {p.name: p.read_bytes() for p in destination.iterdir()}
                report.setdefault('failure_directory_checks', []).append({
                    'route': route, 'before': {k: len(v) for k, v in before.items()},
                    'after': {k: len(v) for k, v in after.items()}})
                assert after == before, report['failure_directory_checks'][-1]
            report['status'] = 'passed'
            print('PASS download -> immediate upload -> visible receipt; empty file; HTTP error; aborted body', flush=True)
        finally:
            report['calls'] = browser.calls
            report['requests'] = server.requests
            (output / 'downloads.json').write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
