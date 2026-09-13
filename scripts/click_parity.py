#!/usr/bin/env python3
"""Compare observable click fallback stages using only a disposable fixture.
--native requires a visible desktop (CI uses Xvfb), and moves its real cursor.
"""
import argparse
from parity_process import run_capture
import json
import os
from pathlib import Path
import socket
import sys
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suh', type=Path, required=True)
    parser.add_argument('--reference', type=Path, required=True)
    parser.add_argument('--native', action='store_true')
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix='suh-click-parity-') as temporary:
        env = dict(os.environ, PYTHONIOENCODING='utf-8', AI_DEV_BROWSER_TRANSPORT='cdp', AI_DEV_BROWSER_OS_CLICK='false', HOME=temporary, USERPROFILE=temporary, PYTHONPATH=str(args.reference.resolve()))
        original_cursor = None
        if args.native:
            import pyautogui
            original_cursor = pyautogui.position()
            env['AI_DEV_BROWSER_VIEWPORT'] = 'native'
            env['AI_DEV_BROWSER_HEADLESS'] = '0'
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
        chrome_log = Path(temporary) / 'chrome.log'
        overrides = {'--no-sandbox': '', '--enable-logging': '', '--log-file': str(chrome_log)}
        started = rust('browser_start', *connection, *([] if args.native else ['--headless']), '--silent-stderr', '--override-default-args', json.dumps(overrides))
        print(f'CLICK START native={args.native}: {started}', flush=True)
        if 'error' in started or not started.get('pid'):
            if chrome_log.exists():
                print(chrome_log.read_text(encoding='utf-8', errors='replace')[-20000:], flush=True)
            raise AssertionError(started)
        assert not started.get('reused'), started
        try:
            def evaluate(expression):
                return rust('js_evaluate', *connection, '--expression', expression)['result']
            cases = [
                ('trusted', 'true', 'trusted'),
                ('synthetic', '!event.isTrusted', 'synthetic'),
                ('js_click', '!event.isTrusted && event instanceof PointerEvent', 'js_click'),
                ('unchanged', 'false', None),
            ]
            if args.native:
                cases.append(('native', 'event.isTrusted && ++window.trustedCount === 2', 'os'))
            for label, predicate, expected_method in cases:
                outcomes = []
                for implementation in [python, rust]:
                    evaluate('document.title="Fixture"; document.body.innerHTML=`<button id="choice" style="margin:80px;width:200px;height:80px">Choose</button><label>Username<input id="username"></label>`; window.trustedCount=0; document.activeElement?.blur(); true')
                    evaluate(f'document.querySelector("#choice").addEventListener("mousedown", event=>event.preventDefault()); document.querySelector("#choice").addEventListener("click", event=>{{if({predicate}) document.title="Accepted";}}); true')
                    result = implementation('click_by_text', *connection, '--text', 'Choose', '--os-click', str(label == 'native').lower())
                    outcome = {key: result[key] for key in ['clicked', 'verified', 'method', 'target']}
                    assert outcome == {'clicked': True, 'verified': expected_method is not None, 'method': expected_method, 'target': 'button'}, (label, result)
                    assert evaluate('document.title') == ('Accepted' if expected_method else 'Fixture'), result
                    if label == 'native':
                        assert evaluate('window.trustedCount') == 2, result
                    outcomes.append(outcome)
                assert outcomes[0] == outcomes[1], (label, outcomes)
                print(f'PASS {label}: Python/Rust stage and actual page effect match', flush=True)
            # Public by-ref path and explicit false overriding environment opt-in.
            env['AI_DEV_BROWSER_OS_CLICK'] = 'true'
            evaluate('document.body.innerHTML=`<button id="choice">By ref</button>`; document.querySelector("button").addEventListener("click",()=>document.title="By ref accepted"); true')
            elements = rust('page_discover', *connection, '--text', 'By ref')
            reference = next(element['ref'] for element in elements if element.get('role') == 'button')
            result = rust('click_by_ref', *connection, '--ref', reference, '--os-click', 'false')
            assert result['verified'] is True and result['method'] == 'trusted', result
            evaluate('document.title="Unchanged"; document.body.innerHTML=`<button id="quiet">Quiet</button>`; document.querySelector("button").addEventListener("mousedown",event=>event.preventDefault()); true')
            quiet = next(element['ref'] for element in rust('page_discover', *connection, '--text', 'Quiet') if element.get('role') == 'button')
            result = rust('click_by_ref', *connection, '--ref', quiet, '--os-click', 'false')
            assert result['verified'] is False and 'hint' not in result, result
            # An explicit no-human-like option must be accepted by both CLIs.
            evaluate('document.body.innerHTML=`<label>Username<input id="username"></label>`; true')
            for implementation in [python, rust]:
                result = implementation('type_by_text', *connection, '--name', 'Username', '--text', 'fixture', '--clear', '--no-human-like')
                assert result['typed'] is True, result
                assert evaluate('document.querySelector("input").value') == 'fixture'
            print('PASS by-ref click, explicit OS opt-out, no-human-like typing', flush=True)
        finally:
            try:
                assert rust('browser_stop', *connection).get('stopped')
            finally:
                if original_cursor is not None:
                    pyautogui.moveTo(*original_cursor)


if __name__ == '__main__':
    main()
