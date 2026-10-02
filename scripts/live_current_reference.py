#!/usr/bin/env python3
"""Run current-reference user journeys through independent CLI processes.

Both implementations operate real disposable Chrome through CDP. Browser startup
and cleanup use suh; this does not compare Python startup, extension transport,
SDK consumers or releases. The navigation result gap stays explicitly open.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys

from PIL import Image

from browser_fixture import BrowserFixture
from parity_process import run_capture

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suh', required=True, type=Path)
    parser.add_argument('--reference', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    baseline = json.loads((ROOT / 'references/ai-dev-browser-baseline.json').read_text(encoding='utf-8'))
    commit = subprocess.check_output(['git', '-C', str(args.reference), 'rev-parse', 'HEAD'], text=True).strip()
    assert commit == baseline['commit'], (commit, baseline['commit'])
    assert not subprocess.check_output(['git', '-C', str(args.reference), 'status', '--porcelain', '--untracked-files=no'], text=True).strip(), 'Reference has tracked edits'
    args.output.mkdir(parents=True, exist_ok=True)
    report = {'schema': 1, 'status': 'failed', 'reference_commit': commit,
              'binary_sha256': hashlib.sha256(args.suh.read_bytes()).hexdigest(),
              'full_parity': False, 'model_tested': False, 'backends': [], 'result_gaps': []}
    try:
        for backend in ('python', 'rust'):
            entry = {'backend': backend, 'checks': [], 'calls': []}
            report['backends'].append(entry)
            with BrowserFixture(args.suh, args.output / backend) as browser:
                browser.env['PYTHONPATH'] = str(args.reference.resolve())
                browser.env['PYTHONIOENCODING'] = 'utf-8'
                # Confirm subprocess imports the pinned checkout, not an installed adb.
                if backend == 'python':
                    imported = run_capture([sys.executable, '-c',
                        'import ai_dev_browser; print(ai_dev_browser.__file__)'], env=browser.env, timeout=15)
                    assert imported.returncode == 0, imported.stderr
                    assert Path(imported.stdout.strip()).resolve().parent == args.reference.resolve() / 'ai_dev_browser'

                def call(tool, *flags):
                    prefix = ([sys.executable, '-m', f'ai_dev_browser.tools.{tool}'] if backend == 'python'
                              else [str(args.suh.resolve()), 'browser', tool])
                    command = [*prefix, '--port', str(browser.port), *map(str, flags)]
                    result = run_capture(command, env=browser.env, timeout=70)
                    entry['calls'].append({'tool': tool, 'args': list(map(str, flags)), 'exit': result.returncode})
                    assert result.returncode == 0, (backend, tool, result.stdout, result.stderr)
                    payload = json.loads(result.stdout)
                    entry['calls'][-1]['result'] = payload
                    assert not (isinstance(payload, dict) and payload.get('error')), payload
                    print(f'{backend} {tool}: exit=0', flush=True)
                    return payload

                # Load -> discover actual ref -> submit -> observe exactly-once receipt.
                navigation = call('page_goto', '--url', browser.url)
                expected = {'url': browser.url, 'title': 'Browser migration acceptance', 'success': True}
                actual_page = call('js_evaluate', '--expression', '({url:location.href,title:document.title,ready:document.readyState})')['result']
                assert actual_page == {'url': browser.url, 'title': expected['title'], 'ready': 'complete'}, actual_page
                if backend == 'python':
                    assert navigation == {**expected, 'ready': True}, navigation
                else:
                    assert navigation == expected, navigation
                    report['result_gaps'].append({'capability': 'C06', 'tool': 'page_goto',
                        'field': 'ready', 'reference': True, 'target': 'missing',
                        'impact': 'Navigation success cannot tell the caller that load completed.'})
                targets = call('page_discover', '--text', 'Reserve seat')
                buttons = [target for target in targets if target.get('name') == 'Reserve seat']
                assert len(buttons) == 1, targets
                assert call('click_by_ref', '--ref', buttons[0]['ref'])['clicked'] is True
                state = call('js_evaluate', '--expression', '({state:acceptance,receipt:document.querySelector("#receipt").textContent,url:location.href})')['result']
                assert state['state']['reservations'] == 1 and state['state']['untrusted'] == 0, state
                assert state['receipt'] == 'Reserved: 1', state
                receipt_url = state['url']
                assert receipt_url == browser.url + '#receipt', state
                entry['checks'].append('discover returned ref -> trusted click -> exactly one visible reservation')

                # Persist a mobile viewport across independent calls and a new tab.
                changed = call('window_set', '--width', 390, '--height', 844)
                assert changed['width'] == 390 and changed['height'] == 844, changed
                assert call('js_evaluate', '--expression', '[innerWidth,innerHeight]')['result'] == [390, 844]
                second_url = browser.url + '?second=1'
                assert call('tab_new', '--url', second_url)['url'] == second_url
                target = ['--tab-url', '?second=1']
                assert call('page_wait_ready', *target)['ready'] is True
                dimensions = call('js_evaluate', *target, '--expression', '({url:location.href,width:innerWidth,height:innerHeight})')['result']
                assert dimensions == {'url': second_url, 'width': 390, 'height': 844}, dimensions
                # A draft must survive reload in the selected second tab. This
                # exercises browser persistence through JS; storage_* wrappers
                # retain their own open baseline defect and are not accepted here.
                call('js_evaluate', *target, '--expression', 'localStorage.setItem("draft","current baseline draft");true')
                call('page_reload', *target)
                assert call('page_wait_ready', *target)['ready'] is True
                restored = call('js_evaluate', *target, '--expression', '({draft:document.querySelector("#draft").textContent,url:location.href,width:innerWidth,height:innerHeight})')['result']
                assert restored == {'draft': 'current baseline draft', 'url': second_url, 'width': 390, 'height': 844}, restored
                visible = call('js_evaluate', *target, '--expression', '(()=>{const e=document.querySelector("#draft");e.scrollIntoView({block:"end"});const r=e.getBoundingClientRect();return {text:e.textContent,top:r.top,bottom:r.bottom,height:innerHeight}})()')['result']
                assert visible['text'] == 'current baseline draft' and 0 <= visible['top'] < visible['bottom'] <= visible['height'], visible
                screenshot = (args.output / backend / 'mobile-draft.png').resolve()
                saved = call('page_screenshot', *target, '--path', screenshot)
                assert Path(saved['path']).resolve() == screenshot and saved['size'] == screenshot.stat().st_size, saved
                with Image.open(screenshot) as picture:
                    picture.load()
                    assert picture.size == (390, 844) == (saved['width'], saved['height']), saved
                    assert len(picture.convert('RGB').getcolors(maxcolors=10) or []) != 1, 'blank screenshot'
                    metadata = json.loads(picture.info['ai_dev_browser'])
                    assert metadata['image_width'] == 390 and metadata['image_height'] == 844, metadata
                entry['screenshot_sha256'] = hashlib.sha256(screenshot.read_bytes()).hexdigest()
                entry['checks'].append('mobile viewport -> second tab -> save draft -> reload -> visible draft and decoded screenshot')

                # Close the tab obtained from the live list, then re-list targets.
                listing = call('tab_list')
                assert listing['count'] == 2, listing
                second = next(tab for tab in listing['tabs'] if tab['url'] == second_url)
                assert call('tab_close', '--tab-id', second['id']) == {'closed': True, 'remaining': 1}
                remaining = call('tab_list')
                assert remaining['count'] == 1 and remaining['tabs'][0]['url'] == receipt_url, remaining
                assert call('js_evaluate', '--tab-url', receipt_url, '--expression', 'document.querySelector("#receipt").textContent')['result'] == 'Reserved: 1'
                entry['checks'].append('live tab list -> close selected target -> verify actual disappearance and original receipt')
            entry['cleanup'] = 'owned browser stopped'
        report['status'] = 'journeys_passed_with_open_result_gap'
    finally:
        (args.output / 'live.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({'status': report['status'], 'full_parity': False,
                      'calls': sum(len(entry['calls']) for entry in report['backends']),
                      'result_gaps': report['result_gaps']}, indent=2))


if __name__ == '__main__':
    main()
