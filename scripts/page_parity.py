#!/usr/bin/env python3
"""Compare page/navigation and file output contracts on a local HTTP fixture."""
import argparse
from parity_process import run_capture
import http.server
import json
import os
from pathlib import Path
import socket
import sys
import tempfile
import threading
import time

from PIL import Image


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suh', type=Path, required=True)
    parser.add_argument('--reference', type=Path, required=True)
    args = parser.parse_args()
    html = '<!doctype html><html><head><meta charset="utf-8"><title>Parity 页面</title></head><body><h1>Fixture 🙂</h1><input id="field" aria-label="Name" value="hello"><button id="hidden" style="display:none">Hidden</button><iframe src="/frame" title="child"></iframe><p>Deterministic content</p></body></html>'
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path == '/fixture.bin':
                body = b'fixture download \x00\xff'
            elif self.path == '/frame':
                body = b'<html><body><input id="child-field" aria-label="Child"><button id="child-button">Child action</button></body></html>'
            else:
                body = html.encode()
            self.send_response(200)
            self.send_header('Content-Type', 'text/html; charset=utf-8')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        def log_message(self, *_):
            pass
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='suh-page-parity-') as temporary:
            env = dict(os.environ, HOME=temporary, USERPROFILE=temporary,
                       PYTHONPATH=str(args.reference.resolve()), PYTHONIOENCODING='utf-8',
                       AI_DEV_BROWSER_TRANSPORT='cdp', AI_DEV_BROWSER_OS_CLICK='false')
            def invoke(command):
                run = run_capture(command, env=env, timeout=45)
                assert run.returncode == 0, (command[:4], run.stderr)
                return json.loads(run.stdout)
            def rust(name, *flags):
                return invoke([str(args.suh.resolve()), 'browser', name, *map(str, flags)])
            def python(name, *flags):
                return invoke([sys.executable, '-m', f'ai_dev_browser.tools.{name}', *map(str, flags)])
            with socket.socket() as reservation:
                reservation.bind(('127.0.0.1', 0))
                port = reservation.getsockname()[1]
            connection = ['--port', str(port)]
            started = rust('browser_start', *connection, '--headless', '--silent-stderr',
                           '--override-default-args', json.dumps({'--no-sandbox': ''}))
            assert started.get('pid') and not started.get('reused'), started
            url = f'http://127.0.0.1:{server.server_port}/fixture'
            try:
                def equivalent(name, *flags, omit=()):
                    results = []
                    for implementation in (python, rust):
                        if name == 'js_evaluate':
                            rust('cdp_send', *connection, '--method', 'Runtime.discardConsoleEntries')
                        results.append(implementation(name, *connection, *flags))
                    for result in results:
                        for key in omit:
                            assert isinstance(result[key], (int, float)) and result[key] >= 0, result
                            result.pop(key)
                    if name.startswith('find_by_') and results[0].get('found') is False:
                        for result in results:
                            hint = result.pop('hint')
                            assert isinstance(hint, str) and 'page_discover' in hint, (name, result, hint)
                    assert results[0] == results[1], (name, flags, results)
                    print(f'PASS {name} {flags}: {list(results[0])}', flush=True)
                    return results[0]
                # The reference returns its pre-navigation Target snapshot.
                # Verify that known bug explicitly; suh must report the live URL.
                reference_navigation = python('page_goto', *connection, '--url', url)
                assert reference_navigation == {'url': 'about:blank', 'title': 'Parity 页面', 'success': True}, reference_navigation
                assert python('js_evaluate', *connection, '--expression', 'location.href')['result'] == url
                navigation = rust('page_goto', *connection, '--url', url)
                assert navigation == {'url': url, 'title': 'Parity 页面', 'success': True}, navigation
                print('PASS page_goto: both navigate; suh reports live URL instead of stale reference snapshot', flush=True)
                info = equivalent('page_info')
                assert info == {'url': url, 'title': 'Parity 页面', 'ready': True, 'state': 'complete'}, info
                for flags in ([], ['--outer']):
                    result = equivalent('page_html', *flags)
                    assert result['length'] == len(result['html']) and '🙂' in result['html'], result
                for expression in (
                    '({nested:[1,true,null,"中文🙂"]})', 'null', 'undefined', 'document.title',
                    'console.log("中文", 12, true, null, undefined, NaN, Infinity); 42',
                    'console.warn({hello:"world"}); console.error([1,2]); "done"',
                    'new Promise(resolve => setTimeout(() => { console.info("awaited"); resolve(7); }, 30))',
                ):
                    equivalent('js_evaluate', '--expression', expression)
                equivalent('page_wait_ready', '--idle-time', '0')
                equivalent('page_wait_ready', '--timeout', '0', '--idle-time', '0')
                equivalent('page_wait_url', '--timeout', '0', omit=('elapsed',))
                equivalent('page_wait_url', '--exact', url, omit=('elapsed',))
                equivalent('page_wait_url', '--pattern', '/fixt.*', omit=('elapsed',))
                equivalent('page_wait_url', '--exact', '', '--pattern', '/fixture', omit=('elapsed',))
                # A negative deadline is deterministic; Python wall-clock resolution
                # can make a zero deadline either match once or expire first.
                equivalent('page_wait_url', '--exact', url, '--timeout=-1', omit=('elapsed',))
                equivalent('page_wait_url', '--exact', url + '/absent', '--timeout', '0', omit=('elapsed',))
                equivalent('page_reload')
                equivalent('page_wait_ready', '--idle-time', '0')
                equivalent('page_reload', '--no-ignore-cache')
                equivalent('page_wait_ready', '--idle-time', '0')
                for html_id in ('field', 'hidden', 'missing', 'child-field', 'child-button'):
                    equivalent('find_by_html_id', '--html-id', html_id)
                for xpath in ('//*[@id="field"]', '//*[@id="hidden"]', '//*[@id="missing"]', '//*[@id="child-button"]'):
                    equivalent('find_by_xpath', '--xpath', xpath)
                for text in ('Fixture', 'Name', 'Hidden', 'absent', 'Child action'):
                    equivalent('find_by_text', '--text', text)
                for label, flags in [
                    ('viewport', []), ('full-page', ['--full-page']),
                    ('raw-pixels', ['--no-css-scale']),
                    ('long-edge', ['--max-long-edge', '320']),
                    ('pixel-budget', ['--max-total-pixels', '60000']),
                ]:
                    path = Path(temporary) / f'{label}.png'
                    outcomes = []
                    for implementation in (python, rust):
                        result = implementation('page_screenshot', *connection, '--path', path, *flags)
                        assert Path(result['path']) == path and result['size'] == path.stat().st_size, result
                        with Image.open(path) as image:
                            image.load()
                            assert image.size == (result['width'], result['height']), result
                            metadata = json.loads(image.info['ai_dev_browser'])
                            assert metadata['image_width'] == image.width and metadata['image_height'] == image.height, metadata
                            assert metadata['scale_factor'] == result['scale_factor'], (metadata, result)
                        if label == 'long-edge':
                            assert max(result['width'], result['height']) <= 320
                        if label == 'pixel-budget':
                            assert result['width'] * result['height'] <= 60000
                        result.pop('size')  # Encoder byte sizes differ; each was checked against disk.
                        outcomes.append((result, metadata))
                        path.unlink()
                    assert outcomes[0] == outcomes[1], (label, outcomes)
                    print(f'PASS screenshot {label}: dimensions, cap and embedded coordinate metadata', flush=True)
                for label, flags in [('portrait', []), ('landscape', ['--landscape'])]:
                    path = Path(temporary) / f'{label}.pdf'
                    outcomes = []
                    for implementation in (python, rust):
                        result = implementation('page_pdf', *connection, '--path', path, *flags)
                        content = path.read_bytes()
                        assert content.startswith(b'%PDF-') and b'%%EOF' in content, result
                        assert result['size'] == len(content) and result['pages'] == 1, result
                        result.pop('size')  # PDF timestamps can change byte length between calls.
                        outcomes.append(result)
                        path.unlink()
                    assert outcomes[0] == outcomes[1], (label, outcomes)
                    print(f'PASS PDF {label}: file signature, byte count and result contract', flush=True)
                folder = Path(temporary) / 'downloads'
                folder.mkdir()
                outcomes = []
                for implementation in (python, rust):
                    result = implementation('download', *connection, '--url', url.rsplit('/', 1)[0] + '/fixture.bin', '--path', folder)
                    file = folder / 'fixture.bin'
                    deadline = time.monotonic() + 5
                    while not file.exists() and time.monotonic() < deadline:
                        time.sleep(.05)
                    assert file.read_bytes() == b'fixture download \x00\xff', result
                    outcomes.append(result)
                    file.unlink()
                assert outcomes[0] == outcomes[1], outcomes
                print('PASS download: matching result and exact binary file contents', flush=True)
            finally:
                assert rust('browser_stop', *connection).get('stopped')
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


if __name__ == '__main__':
    main()
