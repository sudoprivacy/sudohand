#!/usr/bin/env python3
"""Differential browser checks against an explicit ai-dev-browser checkout.

Only synthetic local fixtures are used. No personal cookies or profiles are read.
This suite is one parity gate, not evidence for features it does not exercise.
"""
import argparse
from parity_process import run_capture
from contextlib import closing
import base64
import faulthandler
import hashlib
import http.server
import json
import os
import pickle
from pathlib import Path
import socket
import shlex
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time


def main():
    # Preserve a useful location if a platform API or fixture shutdown stalls.
    faulthandler.dump_traceback_later(120, repeat=True)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suh', type=Path, required=True)
    parser.add_argument('--reference', type=Path, required=True)
    args = parser.parse_args()
    suh = str(args.suh.resolve())
    reference = str(args.reference.resolve())
    with tempfile.TemporaryDirectory(prefix='suh-parity-') as temporary:
        root = Path(temporary)
        env = dict(os.environ, PYTHONIOENCODING='utf-8', AI_DEV_BROWSER_TRANSPORT='cdp', AI_DEV_BROWSER_OS_CLICK='false', HOME=str(root), USERPROFILE=str(root), PYTHONPATH=reference)
        def invoke(command):
            print(f'RUN {command[0:4]}', flush=True)
            if any(str(item).endswith('browser_start') for item in command) and env.get('ADB_TEST_CHROME_ARGS'):
                overrides = {}
                for flag in shlex.split(env['ADB_TEST_CHROME_ARGS']):
                    key, _, value = flag.partition('=')
                    overrides[key] = value
                command = [*command, '--override-default-args', json.dumps(overrides)]
            completed = run_capture(command, env=env, timeout=45)
            if completed.returncode:
                raise AssertionError(f'{command[0:4]} exited {completed.returncode}: {completed.stderr}')
            return json.loads(completed.stdout)
        def rust(name, *flags):
            return invoke([suh, 'browser', name, *map(str, flags)])
        def python(name, *flags):
            return invoke([sys.executable, '-m', f'ai_dev_browser.tools.{name}', *map(str, flags)])
        def equivalent(name, flags):
            expected = python(name, *flags)
            actual = rust(name, *flags)
            assert actual == expected, f'{name}: Rust {actual!r} != Python {expected!r}'
            return actual

        # Plaintext rows require no OS key store. Use only our own SQLite DB.
        profile = root / 'cookie-source' / 'Default' / 'Network'
        profile.mkdir(parents=True)
        with closing(sqlite3.connect(profile / 'Cookies')) as db, db:
            db.execute('CREATE TABLE cookies (host_key TEXT, name TEXT, value TEXT, encrypted_value BLOB, path TEXT, is_secure INTEGER, is_httponly INTEGER, expires_utc INTEGER)')
            db.executemany('INSERT INTO cookies VALUES (?, ?, ?, ?, ?, ?, ?, ?)', [
                ('.example.test', 'long', 'x' * 80, b'', '/', 1, 1, 0),
                ('.example.test', 'unicode', '浏览器', b'', '/path', 0, 0, 13348540800000000),
                ('.other.test', 'other', 'other', b'', '/', 0, 0, 0),
            ])
        equivalent('cookies_extract_offline', ['--domain', 'example.test', '--user-data-dir', profile.parent.parent])
        print('PASS plaintext offline cookie fixture', flush=True)
        if sys.platform == 'win32':
            from cryptography.hazmat.primitives.ciphers.aead import AESGCM
            def protect(data):
                program = "$ErrorActionPreference='Stop'; Add-Type -AssemblyName System.Security; $bytes=[Convert]::FromBase64String([Console]::In.ReadToEnd()); [Console]::Out.Write([Convert]::ToBase64String([Security.Cryptography.ProtectedData]::Protect($bytes,$null,[Security.Cryptography.DataProtectionScope]::CurrentUser)))"
                result = subprocess.run(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', program], input=base64.b64encode(data).decode(), capture_output=True, text=True, encoding='utf-8', check=True, timeout=20)
                return base64.b64decode(result.stdout)
            key = bytes(range(32))
            state = {'os_crypt': {'encrypted_key': base64.b64encode(b'DPAPI' + protect(key)).decode()}}
            (profile.parent.parent / 'Local State').write_text(json.dumps(state))
            nonce = bytes(range(12))
            plain = hashlib.sha256(b'.example.test').digest() + b'windows-fixture-' + b'x' * 80
            encrypted = b'v10' + nonce + AESGCM(key).encrypt(nonce, plain, None)
            with closing(sqlite3.connect(profile / 'Cookies')) as db, db:
                db.executemany('INSERT INTO cookies VALUES (?, ?, ?, ?, ?, ?, ?, ?)', [
                    ('.example.test', 'gcm', '', encrypted, '/', 1, 1, 0),
                    ('.example.test', 'legacy', '', protect('legacy 浏览器'.encode()), '/', 0, 0, 0),
                    ('.example.test', 'app-bound', '', b'v20' + b'x' * 60, '/', 0, 0, 0),
                    ('.example.test', 'invalid-tag', '', encrypted[:-1] + bytes([encrypted[-1] ^ 1]), '/', 0, 0, 0),
                ])
        for domain in ['example.test', '', 'missing.test']:
            equivalent('cookies_extract_offline', ['--domain', domain, '--user-data-dir', profile.parent.parent])
        print('PASS offline: full values, Unicode, domain filters, expiry, schema')

        with socket.socket() as reservation:
            # Discovery intentionally scans the documented preferred band.
            for port in range(9350, 9450):
                try:
                    reservation.bind(('127.0.0.1', port))
                    break
                except OSError:
                    continue
            else:
                raise AssertionError('No free browser port in the discovery band')
        flags = ['--port', str(port)]
        started = rust('browser_start', *flags, '--headless', '--silent-stderr', '--timezone', 'Asia/Tokyo', '--geo', '35.68,139.69', '--locale', 'ja-JP')
        assert started.get('pid') and not started.get('reused'), started
        try:
            instance_path = root / '.ai-dev-browser' / 'instances' / f'{port}.json'
            instance = json.loads(instance_path.read_text())
            assert '--enable-automation' not in instance['argv']
            assert '--disable-blink-features=AutomationControlled' not in instance['argv']
            # Workspace discovery must survive removal of CDP command-line readback.
            listing = rust('browser_list', '--all-workspaces')
            assert any(browser['port'] == port for browser in listing['browsers']), listing
            print('PASS stealth flags and registered browser discovery')
            assert started['identity_consistent'] is True, started
            assert started['geolocation'] == [35.68, 139.69], started
            expression = "({timezone: Intl.DateTimeFormat().resolvedOptions().timeZone, locale: Intl.DateTimeFormat().resolvedOptions().locale})"
            for _ in range(2):
                identity = equivalent('js_evaluate', flags + ['--expression', expression])
                assert identity['result'] == {'timezone': 'Asia/Tokyo', 'locale': 'ja-JP'}, identity
            rust('tab_new', *flags, '--url', 'about:blank')
            identity = equivalent('js_evaluate', flags + ['--expression', expression])
            assert identity['result']['timezone'] == 'Asia/Tokyo', identity
            print('PASS identity re-applied across independent Rust/Python calls and new tab')
            cookies = [
                {'name': 'long', 'value': 'x' * 80, 'domain': '.example.test', 'path': '/', 'secure': False, 'httpOnly': True},
                {'name': 'persistent', 'value': 'persist', 'domain': '.example.test', 'path': '/', 'expires': 2000000000},
                {'name': 'other', 'value': 'other', 'domain': '.other.test', 'path': '/'},
            ]
            rust('cdp_send', *flags, '--method', 'Storage.setCookies', '--params', json.dumps({'cookies': cookies}))
            for domain in ['example.test', '', 'missing.test']:
                equivalent('cookies_extract_live', flags + ['--domain', domain])
            # The legacy command intentionally retains its documented preview.
            old = rust('cookies_list', *flags, '--domain', 'example.test')
            assert old['cookies'][0]['value'] == 'x' * 50 + '...'
            print('PASS live: full values, HttpOnly, expiry, domain filters, schema; legacy preview preserved')
            fixtures = Path(__file__).resolve().parents[1] / 'crates' / 'sudohand-browser' / 'tests' / 'fixtures'
            sys.path.insert(0, reference)
            from ai_dev_browser.cdp.network import Cookie
            expected_cookie = json.loads((fixtures / 'legacy-cookies.json').read_text(encoding='utf-8'))[0]
            # Keep the fixture unexpired without triggering Chrome's lifetime cap.
            expected_cookie['expires'] = int(time.time()) + 3600
            for protocol in (2, 4, 5):
                source = root / f'legacy-cookies-p{protocol}.pickle'
                source.write_bytes(pickle.dumps([Cookie.from_json(expected_cookie)], protocol=protocol))
                outcomes = []
                for implementation in (python, rust):
                    rust('cdp_send', *flags, '--method', 'Storage.clearCookies')
                    loaded = implementation('cookies_load', *flags, '--path', source)
                    assert loaded == {'path': str(source), 'loaded': True}, loaded
                    live = rust('cdp_send', *flags, '--method', 'Storage.getCookies')['result']['cookies']
                    assert len(live) == 1 and live[0]['value'] == '中文-value', live
                    assert live[0]['sameSite'] == 'Lax' and live[0]['priority'] == 'High', live
                    assert live[0]['httpOnly'] and live[0]['secure'] and live[0]['expires'] == expected_cookie['expires'], live
                    outcomes.append(live)
                assert outcomes[0] == outcomes[1], (protocol, outcomes)
            print('PASS legacy cookie migration: protocols 2/4/5 load identical live Chrome cookies')
        finally:
            result = rust('browser_stop', *flags)
            assert result.get('stopped'), result


        # A fake local forward proxy: DNS for this host cannot resolve, so a
        # successful lookup proves Chrome used the proxy rather than host HTTP.
        requests = []
        class Proxy(http.server.BaseHTTPRequestHandler):
            def handle(self):
                try:
                    super().handle()
                except (BrokenPipeError, ConnectionResetError):
                    pass  # Chrome can abandon its unrelated background requests.
            def do_CONNECT(self):
                self.close_connection = True  # Never forward external HTTPS traffic.
            def do_GET(self):
                requests.append(self.path)
                data = json.dumps({'timezone': 'Europe/Paris', 'loc': '48.85,2.35', 'ip': '192.0.2.7'}).encode()
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                self.wfile.write(data)
            def log_message(self, *_):
                pass
        proxy = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Proxy)
        worker = threading.Thread(target=proxy.serve_forever, daemon=True)
        worker.start()
        env['AI_DEV_BROWSER_GEO_ENDPOINT'] = 'http://geo.parity.test/location'
        try:
            outcomes = []
            for implementation in [python, rust]:
                with socket.socket() as reservation:
                    reservation.bind(('127.0.0.1', 0))
                    port = reservation.getsockname()[1]
                connection = ['--port', str(port)]
                proxy_flag = f'--extra-args=--proxy-server=http://127.0.0.1:{proxy.server_port}'
                before = len(requests)
                start = implementation('browser_start', *connection, '--headless', 'new', proxy_flag)
                assert start.get('pid') and not start.get('reused'), start
                try:
                    assert 'http://geo.parity.test/location' in requests[before:], requests
                    effective = {key: start[key] for key in ['identity_consistent', 'timezone', 'geolocation', 'egress_ip']}
                    assert effective == {'identity_consistent': True, 'timezone': 'Europe/Paris', 'geolocation': [48.85, 2.35], 'egress_ip': '192.0.2.7'}, effective
                    assert 'locale' not in start, start
                    observed = implementation('js_evaluate', *connection, '--expression', 'Intl.DateTimeFormat().resolvedOptions().timeZone')
                    assert observed['result'] == 'Europe/Paris', observed
                    outcomes.append(effective)
                finally:
                    assert implementation('browser_stop', *connection).get('stopped')
            assert outcomes[0] == outcomes[1]
            print('PASS proxy: lookup traverses Chrome proxy; ipinfo schema; persisted timezone; locale unchanged')
        finally:
            proxy.shutdown()
            proxy.server_close()
            worker.join(timeout=2)


        # Start only synthetic profiles. Real cleanup is restricted to our unique
        # named profile, never the global temp/workspace scope on a developer host.
        metadata = invoke([sys.executable, '-c', "import json; from ai_dev_browser.core.chrome import find_chrome; from ai_dev_browser.core.config import get_workspace_profile_dir; print(json.dumps({'chrome':find_chrome(),'profile':str(get_workspace_profile_dir('parity-orphan'))}))"])
        orphan_dir = Path(metadata['profile'])
        external_dir = root / 'external-chrome'
        children = []
        managed = None
        with socket.socket() as reservation:
            for external_port in range(9350, 9450):
                try:
                    reservation.bind(('127.0.0.1', external_port))
                    break
                except OSError:
                    continue
            else:
                raise AssertionError('No free external fixture port')
        try:
            for directory in [orphan_dir, external_dir]:
                directory.mkdir(parents=True, exist_ok=True)
                child = subprocess.Popen([metadata['chrome'], '--headless=new', '--no-first-run', '--no-default-browser-check', '--no-sandbox', f'--user-data-dir={directory}', *([f'--remote-debugging-port={external_port}'] if directory == external_dir else []), 'about:blank'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                children.append(child)
            time.sleep(2)
            assert all(child.poll() is None for child in children)
            orphan, external = children
            inventory = rust('browser_list', '--all-workspaces')
            mine = {row['pid']: row for row in inventory['browsers'] if row['pid'] in [orphan.pid, external.pid]}
            assert mine[orphan.pid]['origin'] == 'adb-orphan', mine
            assert mine[external.pid]['origin'] == 'external', mine
            dry = equivalent('browser_cleanup', ['--scope', 'profile', '--profile', 'parity-orphan', '--dry-run'])
            assert dry['count'] == 1 and dry['would_kill'][0]['pid'] == orphan.pid, dry
            assert orphan.poll() is None and external.poll() is None
            result = rust('browser_cleanup', '--scope', 'profile', '--profile', 'parity-orphan')
            assert result['killed'] == [orphan.pid], result
            orphan.wait(timeout=10)
            assert external.poll() is None, 'external browser was killed'
            assert rust('browser_cleanup', '--scope', 'profile', '--profile', 'parity-orphan')['count'] == 0
            print('PASS cleanup: managed orphan inventory; differential dry-run; scoped kill; external preserved; idempotent')
            managed = rust('browser_start', '--headless', '--silent-stderr')
            assert managed.get('pid') and not managed.get('reused'), managed
            stopped = rust('browser_stop', '--stop-all')
            assert stopped['count'] == 1 and stopped['browsers'][0]['port'] == managed['port'], stopped
            assert external.poll() is None, 'blanket stop killed an unregistered debug Chrome'
            managed = None
            print('PASS stop-all: only registered Chrome stopped; unregistered debug Chrome preserved')
        finally:
            if managed and managed.get('port'):
                rust('browser_stop', '--port', managed['port'])
            for child in children:
                if child.poll() is None:
                    child.terminate()
                    try:
                        child.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait(timeout=5)


if __name__ == '__main__':
    main()
