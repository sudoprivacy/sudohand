#!/usr/bin/env python3
"""Real model selection -> real Chrome action -> observed result -> model recovery.

The model sees the executable's own listing/help, never the expected answer.
The runner executes only fixture-scoped browser calls. No coding agents are spawned.
Authentication failures fail the run. Deterministic browser checks are a separate suite.
"""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

from browser_fixture import BrowserFixture
from live_download import BODY, DownloadServer, check_saved

SYSTEM = '''Choose the next browser CLI call for the user's task. You are a text-only
test subject: do not use your own built-in tools, files, network or shell. The test
runner executes your chosen call and returns the real result. Reply with exactly one
JSON object: {"tool":"name","args":["--flag","value"]}, {"help":"name"}, or
{"done":true,"summary":"outcome"}. The browser is already running on the fixture
page. The runner supplies --port. Use the actual catalog/help, and use tool results
to decide whether the task is complete. Do not invent targets or parameters.'''
ALLOWED = {'click_by_html_id', 'click_by_xpath', 'click_by_text', 'click_by_ref',
           'find_by_html_id', 'find_by_xpath', 'find_by_text', 'page_discover',
           'js_evaluate', 'window_set', 'mouse_drag', 'page_info', 'download', 'download_link'}


def parse_reply(text):
    text = text.strip()
    if text.startswith('```') and text.endswith('```'):
        text = '\n'.join(text.splitlines()[1:-1])
    value = json.loads(text)
    assert isinstance(value, dict), value
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suh', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--client', choices=['claude'], default='claude')
    args = parser.parse_args()
    suh = str(args.suh.resolve())
    executable = shutil.which(args.client)
    assert executable, f'{args.client} is required; authenticate it before running this live test'
    catalog = subprocess.check_output([suh, 'describe', '--domain', 'browser', '--with-args'], encoding='utf-8', timeout=20)
    report = {'schema': 1, 'status': 'failed', 'client': args.client,
              'binary_sha256': hashlib.sha256(args.suh.read_bytes()).hexdigest(), 'scenarios': []}
    args.output.mkdir(parents=True, exist_ok=True)
    count = 0
    with tempfile.TemporaryDirectory(prefix='suh-model-context-') as context:
        def choose(task, history):
            nonlocal count
            count += 1
            assert count <= 25, 'model-call budget exhausted'
            prompt = SYSTEM + '\n\nAvailable CLI catalog:\n' + catalog + '\nTask:\n' + task + '\nHistory:\n' + json.dumps(history, ensure_ascii=False)
            command = [executable, '--print', '--safe-mode', '--no-session-persistence', '--tools', '',
                       '--system-prompt', SYSTEM, '--output-format', 'json', '--max-budget-usd', '1', prompt]
            raw = subprocess.run(command, cwd=context, capture_output=True, text=True, encoding='utf-8', timeout=120, check=False)
            assert raw.returncode == 0, f'model request failed ({raw.returncode}): {raw.stderr or raw.stdout}'
            response = json.loads(raw.stdout)
            assert not response.get('is_error') and not response.get('tool_uses'), response
            reply = parse_reply(response['result'])
            print(f'model choice: {json.dumps(reply)}', flush=True)
            return reply, {'model': response.get('model') or list(response.get('modelUsage', {})) or os.environ.get('ANTHROPIC_MODEL'), 'usage': response.get('usage'),
                           'cost': response.get('estimated_cost', response.get('total_cost_usd'))}

        def scenario(browser, name, task, first_tools, verify, *, initial=None, max_calls=3):
            history = list(initial or [])
            entry = {'name': name, 'task': task, 'turns': [], 'calls': 0, 'status': 'running'}
            report['scenarios'].append(entry)
            for turn in range(max_calls + 4):
                reply, metadata = choose(task, history)
                entry['turns'].append({'reply': reply, **metadata})
                if reply.get('done') is True:
                    assert entry['calls'] > 0, 'model declared success without acting'
                    verify(browser)
                    entry['status'] = 'passed'
                    return
                if 'help' in reply:
                    assert reply['help'] in ALLOWED, reply
                    result = subprocess.check_output([suh, 'browser', reply['help'], '--help'], encoding='utf-8', timeout=20)
                else:
                    tool, flags = reply['tool'], reply['args']
                    assert tool in ALLOWED and isinstance(flags, list) and all(isinstance(v, str) for v in flags), reply
                    assert not any(v.split('=')[0] in {'--port', '-p', '--transport', '--tab-url', '--profile'} for v in flags), reply
                    if entry['calls'] == 0:
                        assert tool in first_tools, f'{name}: wrong first tool: {tool}'
                    entry['calls'] += 1
                    assert entry['calls'] <= max_calls, f'{name}: unnecessary calls or failed recovery'
                    code, payload = browser.call_raw(tool, *flags)
                    result = {'exit': code, 'result': payload}
                    entry['turns'][-1]['observed'] = result
                history.append({'choice': reply, 'result': result})
            raise AssertionError(f'{name}: model did not finish within the turn budget')

        def reserved(browser):
            actual = browser.js('({state:acceptance,receipt:document.querySelector("#receipt").textContent})')
            assert actual['state']['reservations'] == 1 and actual['state']['untrusted'] == 0, actual
            assert actual['receipt'] == 'Reserved: 1', actual

        try:
            with BrowserFixture(suh, args.output) as browser:
                scenario(browser, 'known HTML id', 'Click the reservation button. Its HTML id is reserve-now.',
                         {'click_by_html_id'}, reserved, max_calls=1)
                browser.reset()
                code, failure = browser.call_raw('click_by_html_id', '--html-id', 'retired-reservation-id')
                assert code == 4 and failure['error']['retryable'] is False, failure
                scenario(browser, 'recover stale locator', 'Reserve one seat. A previous attempt used an outdated id and failed; recover from the result below.',
                         {'page_discover', 'find_by_text'}, reserved, max_calls=2,
                         initial=[{'choice': {'tool': 'click_by_html_id', 'args': ['--html-id', 'retired-reservation-id']},
                                   'result': {'exit': code, 'result': failure}}])
                browser.reset()
                def mobile(b):
                    assert b.js('[innerWidth,innerHeight]') == [390, 844]
                scenario(browser, 'responsive layout', 'Set this browser render viewport to 390 by 844 pixels for a mobile layout check.',
                         {'window_set'}, mobile, max_calls=1)
                browser.screenshot('model-mobile.png')
                browser.call('window_set', '--width', 1200, '--height', 900)
                browser.reset()
                coordinates = browser.js('(()=>{const r=document.querySelector("#knob").getBoundingClientRect();return {x:r.x+20,y:r.y+20}})()')
                def dragged(b):
                    actual = b.js('({left:document.querySelector("#knob").offsetLeft,buttons:acceptance.dragButtons})')
                    assert actual['left'] == 220 and set(actual['buttons']) == {1}, actual
                scenario(browser, 'trusted coordinate drag',
                         f'Drag the slider from CSS coordinates ({coordinates["x"]}, {coordinates["y"]}) to ({coordinates["x"] + 220}, {coordinates["y"]}).',
                         {'mouse_drag'}, dragged, max_calls=2)
                browser.screenshot('model-drag.png')
                with DownloadServer() as downloads:
                    browser.call('page_goto', '--url', downloads.url)
                    destination = (args.output / 'model-downloads').resolve()
                    def downloaded(b):
                        latest = next(c for c in reversed(b.calls) if c['tool'] == 'download')
                        check_saved(latest['result'], destination, BODY)
                    scenario(browser, 'save URL for immediate reuse',
                             f'Save the export at {downloads.url}/slow.bin into directory {destination}. Report the saved file location for the next step.',
                             {'download'}, downloaded, max_calls=1)
                    assert downloads.requests.count('/slow.bin') == 1, downloads.requests
                    code, failure = browser.call_raw('download', '--url', downloads.url + '/missing.bin', '--path', destination)
                    assert code == 9 and failure['error']['retryable'] is False, failure
                    scenario(browser, 'recover missing export URL',
                             f'Save the export into directory {destination}. The earlier URL failed. The corrected URL is {downloads.url}/slow.bin?corrected=1.',
                             {'download'}, downloaded, max_calls=1,
                             initial=[{'choice': {'tool': 'download', 'args': ['--url', downloads.url + '/missing.bin', '--path', str(destination)]},
                                       'result': {'exit': code, 'result': failure}}])
                    assert downloads.requests.count('/missing.bin') == 1, downloads.requests
                    assert downloads.requests.count('/slow.bin?corrected=1') == 1, downloads.requests
            report['status'] = 'passed'
        finally:
            report['model_requests'] = count
            (args.output / 'model-steering.json').write_text(json.dumps(report, indent=2, ensure_ascii=False), encoding='utf-8')
    print(json.dumps({'status': report['status'], 'scenarios': len(report['scenarios']), 'model_requests': count}))


if __name__ == '__main__':
    main()
