#!/usr/bin/env python3
"""Real Chrome extension checks in a disposable profile. Requires websockets.
Never loads an extension into, attaches to, or closes a personal browser.
"""
import argparse
from parity_process import run_capture, capture_process_tree, finish_process_tree
import asyncio
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import urllib.request
import websockets


def free_port(requested=0):
    with socket.socket() as reservation:
        if os.name != 'nt':
            reservation.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        reservation.bind(('127.0.0.1', requested))
        return reservation.getsockname()[1]


async def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suh', type=Path, required=True)
    parser.add_argument('--chrome', required=True)
    parser.add_argument('--reference', type=Path, required=True)
    args = parser.parse_args()
    suh = str(args.suh.resolve())
    bridge_port = free_port(9522)  # fail rather than disturb an existing bridge
    chrome_port = free_port()
    children = []
    with tempfile.TemporaryDirectory(prefix='suh-extension-test-') as temporary:
        root = Path(temporary)
        env = dict(os.environ, PYTHONIOENCODING='utf-8', AI_DEV_BROWSER_TRANSPORT='cdp', AI_DEV_BROWSER_OS_CLICK='false', HOME=str(root), USERPROFILE=str(root), PYTHONPATH=str(args.reference.resolve()))
        def cli(name, *flags):
            result = run_capture([suh, 'browser', name, *map(str, flags)], env=env, timeout=40)
            assert result.returncode == 0, (name, result.stderr)
            return json.loads(result.stdout)
        def reference(name, *flags):
            result = run_capture([sys.executable, '-m', f'ai_dev_browser.tools.{name}', *map(str, flags)], env=env, timeout=40)
            assert result.returncode == 0, (name, result.stderr)
            return json.loads(result.stdout)
        async def command(ws, method, params=None):
            command.sequence += 1
            mid = command.sequence
            await ws.send(json.dumps({'id': mid, 'method': method, 'params': params or {}}))
            while True:
                response = json.loads(await asyncio.wait_for(ws.recv(), 10))
                if response.get('id') == mid:
                    assert 'error' not in response, (method, response)
                    return response.get('result', {})
        command.sequence = 0
        try:
            bridge = subprocess.Popen([suh, 'browser', 'bridge-serve', '--port', str(bridge_port)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            children.append(bridge)
            # Match the regular launcher: a disposable macOS test browser must
            # not wait for interactive access to the user's system keychain.
            chrome = subprocess.Popen([args.chrome, '--headless=new', '--use-mock-keychain', '--no-first-run', '--no-default-browser-check', '--no-sandbox', '--enable-unsafe-extension-debugging', f'--remote-debugging-port={chrome_port}', f'--user-data-dir={root / "chrome"}', 'about:blank'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            children.append(chrome)
            version = None
            for _ in range(100):
                assert chrome.poll() is None and bridge.poll() is None
                try:
                    with urllib.request.urlopen(f'http://127.0.0.1:{chrome_port}/json/version', timeout=1) as response:
                        version = json.load(response)
                    break
                except Exception:
                    await asyncio.sleep(.1)
            assert version, 'Chrome debugging endpoint did not start'
            # First CLI call extracts assets from the actual built binary.
            setup = cli('browser_connect', '--transport', 'extension')
            assert setup['connected'] is False and setup['bridge_running'] is True, setup
            extension_dir = setup['extension_dir']
            async with websockets.connect(version['webSocketDebuggerUrl']) as cdp:
                before = await command(cdp, 'Target.getTargets')
                personal = next(t for t in before['targetInfos'] if t['type'] == 'page')
                loaded = await command(cdp, 'Extensions.loadUnpacked', {'path': extension_dir})
                assert loaded.get('id'), loaded
                connected = None
                for _ in range(10):
                    connected = cli('browser_connect', '--transport', 'extension')
                    if connected['connected']:
                        break
                    await asyncio.sleep(.5)
                if not connected['connected']:
                    targets = await command(cdp, 'Target.getTargets')
                    print('Extension connection diagnostics:', targets, flush=True)
                    for target in targets['targetInfos']:
                        if target['type'] == 'service_worker' and target['url'].startswith(f"chrome-extension://{loaded['id']}/"):
                            attached = await command(cdp, 'Target.attachToTarget', {'targetId': target['targetId'], 'flatten': True})
                            command.sequence += 1
                            await cdp.send(json.dumps({'id': command.sequence, 'sessionId': attached['sessionId'], 'method': 'Runtime.evaluate', 'params': {'expression': '({socketState: socket?.readyState, connecting, extensionId: chrome.runtime.id})', 'returnByValue': True}}))
                            while True:
                                response = json.loads(await asyncio.wait_for(cdp.recv(), 10))
                                if response.get('id') == command.sequence:
                                    print('Extension worker diagnostics:', response, flush=True)
                                    break
                assert connected['connected'], connected
                assert connected['tabs'] == ['about:blank'], connected
                print('PASS actual Chrome extension loaded; bridge handshake; CLI connection')
                flags = ['--transport', 'extension']
                reference_connected = reference('browser_connect', *flags)
                assert reference_connected == connected, (reference_connected, connected)

                result = cli('js_evaluate', *flags, '--expression', '({value: 42, url: location.href})')
                assert result['result']['value'] == 42, result
                own = cli('cdp_send', *flags, '--method', 'AiDevBrowser.debugState')['result']
                assert len(own['autoTabs']) == 1, own
                cli('js_evaluate', *flags, '--expression', 'window.name = "suh-owned"; window.name')
                result = cli('js_evaluate', *flags, '--expression', 'window.name')
                assert result['result'] == 'suh-owned', result
                assert reference('js_evaluate', *flags, '--expression', 'window.name') == result
                cli('js_evaluate', *flags, '--expression', 'document.body.innerHTML = `<button>Activate</button>`; document.querySelector("button").addEventListener("click", () => window.clicked = true); true')
                action = cli('click_by_text', *flags, '--text', 'Activate')
                clicked = cli('js_evaluate', *flags, '--expression', 'window.clicked')
                assert clicked['result'] is True, (action, clicked)
                assert reference('js_evaluate', *flags, '--expression', 'window.clicked') == clicked
                screenshot = root / 'extension.png'
                cli('page_screenshot', *flags, '--path', screenshot)
                assert screenshot.read_bytes().startswith(b'\x89PNG\r\n\x1a\n')
                print('PASS Python/Rust extension contract; click effect; real screenshot')

                after = await command(cdp, 'Target.getTargets')
                untouched = next(t for t in after['targetInfos'] if t['targetId'] == personal['targetId'])
                assert untouched['url'] == personal['url'], untouched
                print('PASS independent CLI calls reuse owned tab; pre-existing tab untouched')
                cli('js_evaluate', *flags, '--expression', 'window.open("about:blank#popup"); true')
                await asyncio.sleep(.5)
                own = cli('cdp_send', *flags, '--method', 'AiDevBrowser.debugState')['result']
                assert len(own['autoTabs']) == 2, own
                popup = cli('js_evaluate', *flags, '--tab-url', '#popup', '--expression', 'location.hash')
                assert popup['result'] == '#popup', popup
                print('PASS popup adoption and --tab-url routing')
                stop = cli('browser_disconnect')
                assert stop['stopped'] is True and stop['was_running'] is True, stop
                bridge.wait(timeout=5)
                assert chrome.poll() is None
                await command(cdp, 'Browser.getVersion')
                print('PASS disconnect stops owned bridge and preserves Chrome')
                bridge = subprocess.Popen([suh, 'browser', 'bridge-serve', '--port', str(bridge_port)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
                children.append(bridge)
                reconnected = None
                for _ in range(30):
                    reconnected = cli('browser_connect', *flags)
                    if reconnected['connected']:
                        break
                    await asyncio.sleep(.5)
                assert reconnected['connected'], reconnected
                own = cli('cdp_send', *flags, '--method', 'AiDevBrowser.debugState')['result']
                assert len(own['autoTabs']) == 2, own
                popup = cli('js_evaluate', *flags, '--tab-url', '#popup', '--expression', 'location.hash')
                assert popup['result'] == '#popup', popup
                after_restart = await command(cdp, 'Target.getTargets')
                untouched = next(t for t in after_restart['targetInfos'] if t['targetId'] == personal['targetId'])
                assert untouched['url'] == personal['url'] and chrome.poll() is None
                assert cli('browser_disconnect')['stopped']
                bridge.wait(timeout=5)
                print('PASS bridge restart: extension reconnects, owned tabs and unrelated tab survive')
        finally:
            for child in reversed(children):
                if child.poll() is None:
                    processes = capture_process_tree(child.pid)
                    finish_process_tree(processes, terminate=True)
                    child.wait(timeout=5)


if __name__ == '__main__':
    asyncio.run(main())
