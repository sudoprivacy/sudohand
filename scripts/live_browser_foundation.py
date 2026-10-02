#!/usr/bin/env python3
"""Real independent CLI calls: recover, drag, keep mobile layout and reload a draft."""
import argparse
import hashlib
import json
from pathlib import Path

from audit_cli_steering import audit
from browser_fixture import BrowserFixture


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suh', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    report = {'schema': 1, 'status': 'failed', 'binary_sha256': hashlib.sha256(args.suh.read_bytes()).hexdigest(),
              'checks': [], 'mechanical': audit(args.suh.resolve()), 'live_model': False}
    args.output.mkdir(parents=True, exist_ok=True)
    try:
        with BrowserFixture(args.suh, args.output) as browser:
            report['calls'] = browser.calls
            code, occupied = browser.call_raw('browser_start', '--headless')
            assert code == 2 and occupied['error']['retryable'] is False, occupied
            assert browser.js('1+1') == 2, 'failed startup must leave the existing browser usable'
            report['checks'].append('occupied-port launch fails with nonzero status and preserves the existing browser')
            code, missing = browser.call_raw('click_by_html_id', '--html-id', 'retired-reservation-id')
            assert code == 4 and missing['error']['kind'] == 'not_found', missing
            assert missing['error']['retryable'] is False and 'page_discover' in missing['error']['hint'], missing
            targets = browser.call('page_discover', '--text', 'Reserve seat')
            button = next(target for target in targets if target.get('name') == 'Reserve seat')
            clicked = browser.call('click_by_ref', '--ref', button['ref'])
            assert clicked['clicked'] is True, clicked
            state = browser.js('({state:acceptance,receipt:document.querySelector("#receipt").textContent})')
            assert state['state']['reservations'] == 1 and state['state']['untrusted'] == 0, state
            assert state['receipt'] == 'Reserved: 1', state
            report['checks'].append('stale locator -> discovery -> returned ref -> trusted reservation exactly once')

            code, failed = browser.call_raw('js_evaluate', '--expression',
                'window.beforeThrow=(window.beforeThrow||0)+1;throw new Error("fixture-evaluation-failure")')
            assert code == 1 and failed['error']['kind'] == 'evaluation', failed
            assert failed['error']['retryable'] is False and failed['error']['hint'], failed
            assert browser.js('beforeThrow') == 1
            report['checks'].append('JavaScript throw is nonretryable, with recovery hint and no repeated side effect')

            for human in (False, True):
                browser.reset()
                rect = browser.js('(()=>{const r=document.querySelector("#knob").getBoundingClientRect();return {x:r.x+20,y:r.y+20}})()')
                flags = ['--from-x', rect['x'], '--from-y', rect['y'], '--to-x', rect['x'] + 220, '--to-y', rect['y']]
                if human:
                    flags.append('--human-like')
                assert browser.call('mouse_drag', *flags)['dragged'] is True
                actual = browser.js('({left:document.querySelector("#knob").offsetLeft,state:acceptance})')
                assert actual['left'] == 220 and actual['state']['moves'] > 0, actual
                assert set(actual['state']['dragButtons']) == {1} and actual['state']['releases'] == [0], actual
                report['checks'].append(f'{"human" if human else "linear"} drag moves slider 220px with buttons=1, then releases')
            browser.screenshot('desktop-drag.png')

            viewport = browser.call('window_set', '--width', 390, '--height', 844)
            assert viewport['viewport_persisted'] is True, viewport
            browser.call('page_reload')
            assert browser.call('page_wait_ready')['ready'] is True
            dimensions = browser.js('({width:innerWidth,height:innerHeight,mobile:getComputedStyle(document.querySelector("#mobile")).display})')
            assert dimensions == {'width': 390, 'height': 844, 'mobile': 'block'}, dimensions
            browser.call('window_set', '--height', 780)
            assert browser.js('[innerWidth,innerHeight]') == [390, 780]
            browser.screenshot('mobile.png')
            browser.call('tab_new', '--url', browser.url + '?second=1')
            dimensions = browser.call('js_evaluate', '--tab-url', '?second=1', '--expression', '[innerWidth,innerHeight]')['result']
            assert dimensions == [390, 780], dimensions
            report['checks'].append('mobile dimensions survive independent processes, reload, partial update and a new tab')

            # Independent calls choose a page target afresh. Once two pages
            # exist, pin the whole workflow and wait for reload completion.
            target = ['--tab-url', '?second=1']
            before = browser.call('js_evaluate', *target, '--expression', 'document.querySelector("#draft").textContent')
            assert before['result'] == 'EMPTY', before
            browser.call('storage_set', *target, '--key', 'draft', '--value', 'migration draft 2026')
            browser.call('page_reload', *target)
            assert browser.call('page_wait_ready', *target)['ready'] is True
            restored = browser.call('js_evaluate', *target, '--expression', 'document.querySelector("#draft").textContent')
            assert restored['result'] == 'migration draft 2026', restored
            assert restored['url_after'] == browser.url + '?second=1', restored
            report['checks'].append('saved draft is visible after reload on the same explicitly selected tab')
        report['status'] = 'passed'
    finally:
        (args.output / 'foundation.json').write_text(json.dumps(report, indent=2, ensure_ascii=False), encoding='utf-8')
    print(json.dumps({'status': report['status'], 'checks': report['checks']}, indent=2))


if __name__ == '__main__':
    main()
