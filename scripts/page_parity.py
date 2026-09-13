#!/usr/bin/env python3
"""Compare page/navigation contracts on a local, deterministic HTTP fixture."""
import argparse
import http.server
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suh', type=Path, required=True)
    parser.add_argument('--reference', type=Path, required=True)
    args = parser.parse_args()
    html = '<!doctype html><html><head><meta charset="utf-8"><title>Parity 页面</title></head><body><h1>Fixture 🙂</h1><input id="field" value="hello"><p>Deterministic content</p></body></html>'
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
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
                run = subprocess.run(command, env=env, capture_output=True, text=True,
                                     encoding='utf-8', timeout=45)
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
                    results = [implementation(name, *connection, *flags) for implementation in (python, rust)]
                    for result in results:
                        for key in omit:
                            assert isinstance(result[key], (int, float)) and result[key] >= 0, result
                            result.pop(key)
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
                for expression in ('({nested:[1,true,null,"中文🙂"]})', 'null', 'undefined', 'document.title'):
                    equivalent('js_evaluate', '--expression', expression)
                equivalent('page_wait_ready', '--idle-time', '0')
                equivalent('page_wait_ready', '--timeout', '0', '--idle-time', '0')
                equivalent('page_wait_url', '--timeout', '0', omit=('elapsed',))
                equivalent('page_wait_url', '--exact', url, omit=('elapsed',))
                equivalent('page_wait_url', '--pattern', '/fixt.*', omit=('elapsed',))
                equivalent('page_wait_url', '--exact', '', '--pattern', '/fixture', omit=('elapsed',))
                equivalent('page_wait_url', '--exact', url, '--timeout', '0', omit=('elapsed',))
                equivalent('page_wait_url', '--exact', url + '/absent', '--timeout', '0', omit=('elapsed',))
                equivalent('page_reload')
                equivalent('page_wait_ready', '--idle-time', '0')
                equivalent('page_reload', '--no-ignore-cache')
                equivalent('page_wait_ready', '--idle-time', '0')
            finally:
                assert rust('browser_stop', *connection).get('stopped')
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


if __name__ == '__main__':
    main()
