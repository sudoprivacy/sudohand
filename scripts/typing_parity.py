#!/usr/bin/env python3
"""Compare verified typing and page effects on isolated, deterministic fields."""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suh', type=Path, required=True)
    parser.add_argument('--reference', type=Path, required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix='suh-typing-parity-') as temporary:
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
        try:
            def evaluate(expression):
                return rust('js_evaluate', *connection, '--expression', expression)['result']
            # label, element, handlers, requested text, options, method, actual value
            cases = [
                ('cli-default', '<input>', '', 'default', [], 'insertText', 'default'),
                ('replace-unicode', '<input value="previous">', '', '测试é🙂', [], 'insertText', '测试é🙂'),
                ('empty', '<input value="previous">', '', '', [], 'insertText', ''),
                ('keys-preferred', '<input>', '', 'Az9', ['--keystrokes'], 'keys', 'Az9'),
                ('human-preferred', '<textarea></textarea>', '', 'ok', ['--human-like'], 'human', 'ok'),
                ('keys-fallback', '<input>', "field.addEventListener('beforeinput', e => {if (!window.keyAllowed) e.preventDefault();}); field.addEventListener('keydown', e => window.keyAllowed = e.code === 'Digit7' && e.keyCode === 55);", '777', [], 'keys', '777'),
                ('native-fallback', '<textarea></textarea>', "field.addEventListener('beforeinput', e => e.preventDefault());", 'native', [], 'native', 'native'),
                ('reject-all', '<input>', "field.addEventListener('input', () => field.value = '');", 'rejected', [], None, ''),
                ('partial', '<input>', "field.addEventListener('input', () => field.value = field.value.slice(0, 1));", 'partial', [], None, 'p'),
                ('contenteditable', '<div contenteditable="true" role="textbox">old</div>', '', 'editable', [], 'insertText', 'editable'),
                ('readonly-native', '<input readonly>', '', 'readonly', [], 'native', 'readonly'),
                ('enter', '<input>', '', 'submit', ['--enter'], 'insertText', 'submit'),
            ]
            for label, html, handlers, text, options, method, actual in cases:
                for tool in ['type_by_ref', 'type_by_text']:
                    outcomes = []
                    effective_options = list(options)
                    expected_method = method
                    if tool == 'type_by_text':
                        if label == 'cli-default':
                            expected_method = 'human'
                        elif '--human-like' in effective_options:
                            effective_options.remove('--human-like')
                        else:
                            effective_options.append('--no-human-like')
                    for implementation in [python, rust]:
                        evaluate(f'document.body.innerHTML={json.dumps(html)}; window.keyAllowed=false; window.entered=0; true')
                        evaluate("{ const field=document.body.firstElementChild; field.id='field'; field.setAttribute('aria-label','Fixture'); field.addEventListener('keydown', e=>{if(e.key==='Enter') window.entered++}); " + handlers + '; } true')
                        if tool == 'type_by_ref':
                            located = rust('page_discover', *connection, '--text', 'Fixture')
                            locator = ['--ref', next(el['ref'] for el in located if el.get('role') == 'textbox')]
                        else:
                            locator = ['--name', 'Fixture']
                        result = implementation(tool, *connection, *locator, '--text', text, *effective_options)
                        outcome = {key: result[key] for key in ['typed', 'verified', 'method', 'methods_tried']}
                        assert outcome['method'] == expected_method, (label, tool, result)
                        assert outcome['verified'] == (method is not None), (label, tool, result)
                        assert outcome['typed'] == (method is not None or actual != ''), (label, tool, result)
                        assert evaluate('document.body.firstElementChild.value ?? document.body.firstElementChild.textContent') == actual, (label, tool, result)
                        if '--enter' in options:
                            assert result['entered'] is True and evaluate('window.entered') == 1, result
                        outcomes.append(outcome)
                    assert outcomes[0] == outcomes[1], (label, tool, outcomes)
                print(f'PASS {label}: both locators match Python results and actual field contents', flush=True)
        finally:
            assert rust('browser_stop', *connection).get('stopped')


if __name__ == '__main__':
    main()
