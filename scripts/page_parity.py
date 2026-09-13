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
    html = '<!doctype html><html><head><meta charset="utf-8"><title>Parity 页面</title></head><body><h1>Fixture 🙂</h1><input id="field" aria-label="Name" value="hello"><button id="hidden" style="display:none">Hidden</button><iframe src="/frame" title="child"></iframe><select id="choice" aria-label="Choice" size="2"><option value="a">Alpha</option><option value="b">Beta</option></select><input id="upload" type="file" multiple aria-label="Upload fixture"><p>Deterministic content</p></body></html>'
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path == '/fixture.bin':
                body = b'fixture download \x00\xff'
            elif self.path in ('/frame', '/crossframe'):
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
                           '--override-default-args', json.dumps({'--no-sandbox': '', '--site-per-process': ''}))
            assert 'error' not in started and started.get('pid') and not started.get('reused'), started
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
                    print(f'PASS {name} {flags}: {len(results[0])} result fields/items', flush=True)
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
                for pattern in (r'(?<=/)fixture$', r'fixture(?=$)', r'(?P<octet>0)\.(?P=octet)', r'(0)\.\1'):
                    assert equivalent('page_wait_url', '--pattern', pattern, '--timeout', '1', omit=('elapsed',))['matched']
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
                for discovery_flags in ([], ['--no-interactable-only'], ['--no-include-coordinates'], ['--no-include-iframes'], ['--no-dom-scan'], ['--dom-limit', '1'], ['--text', 'CHILD']):
                    equivalent('page_discover', *discovery_flags)
                for wait_flags in (['--selector', '#field'], ['--text', 'Fixture'], ['--selector', '#missing', '--timeout', '.1'], ['--selector', '#hidden', '--timeout', '.1']):
                    outcomes = []
                    for implementation in (python, rust):
                        result = implementation('page_wait_element', *connection, *wait_flags)
                        assert result.pop('elapsed') >= 0, result
                        if not result['found']:
                            assert isinstance(result.pop('hint'), str), result
                        outcomes.append(result)
                    assert outcomes[0] == outcomes[1], (wait_flags, outcomes)
                    assert outcomes[0]['found'] == ('--timeout' not in wait_flags), outcomes
                    print(f'PASS page_wait_element {wait_flags}: visible controls and hidden/missing deadlines', flush=True)
                result = equivalent('select_text', '--text', 'Deterministic', '--to-text', 'content')
                assert result['selected'] and rust('js_evaluate', *connection, '--expression', 'getSelection().toString()')['result'] == 'Deterministic content', result
                elements = rust('page_discover', *connection, '--no-interactable-only')
                def named_ref(name):
                    matches = [element['ref'] for element in elements if element.get('name') == name]
                    assert len(matches) == 1, (name, elements)
                    return matches[0]
                field_ref = named_ref('Name')
                equivalent('focus_by_ref', '--ref', field_ref)
                assert rust('js_evaluate', *connection, '--expression', 'document.activeElement.id')['result'] == 'field'
                equivalent('html_by_ref', '--ref', field_ref)
                equivalent('hover_by_ref', '--ref', field_ref)
                assert rust('js_evaluate', *connection, '--expression', 'document.querySelector("#field").matches(":hover")')['result'] is True
                equivalent('press_key', '--ref', field_ref, '--key', 'Home')
                assert rust('js_evaluate', *connection, '--expression', 'document.querySelector("#field").selectionStart')['result'] == 0
                beta_ref = named_ref('Beta')
                upload_ref = named_ref('Upload fixture')
                uploads = [Path(temporary) / 'first.txt', Path(temporary) / 'second.txt']
                for file in uploads:
                    file.write_text(file.name, encoding='utf-8')
                for tool, reference, extra, reset, observation, expected in [
                    ('select_by_ref', beta_ref, [], 'document.querySelector("#choice").value="a"', 'document.querySelector("#choice").value', 'b'),
                    ('upload_by_ref', upload_ref, ['--paths', ','.join(map(str, uploads))], 'document.querySelector("#upload").value=""', 'Array.from(document.querySelector("#upload").files, f => ({name:f.name,size:f.size}))', [{'name':f.name,'size':f.stat().st_size} for f in uploads]),
                ]:
                    results = []
                    for implementation in (python, rust):
                        rust('js_evaluate', *connection, '--expression', reset)
                        results.append(implementation(tool, *connection, '--ref', reference, *extra))
                        actual = rust('js_evaluate', *connection, '--expression', observation)['result']
                        assert actual == expected, (tool, results[-1], actual)
                    assert results[0] == results[1], (tool, results)
                    print(f'PASS {tool}: equal result and verified page state', flush=True)
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
                    print(f'DOWNLOAD {implementation.__name__}: {result}', flush=True)
                    file = folder / 'fixture.bin'
                    deadline = time.monotonic() + 5
                    while not file.exists() and time.monotonic() < deadline:
                        time.sleep(.05)
                    assert file.exists(), (implementation.__name__, result, list(folder.iterdir()), rust('js_evaluate', *connection, '--expression', '({url:location.href,links:Array.from(document.querySelectorAll("a"), a=>({href:a.href,download:a.download}))})'))
                    assert file.read_bytes() == b'fixture download \x00\xff', result
                    outcomes.append(result)
                    file.unlink()
                assert outcomes[0] == outcomes[1], outcomes
                print('PASS download: matching result and exact binary file contents', flush=True)
                rust('js_evaluate', *connection, '--expression', 'document.body.insertAdjacentHTML("beforeend", \'<a id="download-link" href="/fixture.bin" download="linked.txt">Download fixture</a>\')')
                outcomes = []
                for implementation in (python, rust):
                    result = implementation('download_link', *connection, '--xpath', '//*[@id="download-link"]', '--download-dir', folder, '--timeout', '5')
                    assert result['downloaded'] and result['filename'] == 'linked.txt', (implementation.__name__, result)
                    saved = Path(result['path'])
                    assert saved.read_bytes() == b'fixture download \x00\xff' and result['bytes'] == saved.stat().st_size, result
                    outcomes.append(result)
                    saved.unlink()
                assert outcomes[0] == outcomes[1], outcomes
                print('PASS download_link: event completion, path, byte count and actual file contents', flush=True)
                # The pinned reference's CLI calls nonexistent Tab methods.
                # Record that bug, then verify suh against actual browser state.
                for tool, extra, missing_method in [('storage_get', [], 'get_local_storage'), ('storage_set', ['--key','parity','--value','value'], 'set_local_storage')]:
                    broken = python(tool, *connection, *extra)
                    assert missing_method in broken.get('error', ''), (tool, broken)
                assert rust('storage_get', *connection, '--key', 'missing') == {'key':'missing','value':None}
                assert rust('storage_set', *connection, '--key', 'parity', '--value', '中文 value') == {'key':'parity','value':'中文 value'}
                assert python('js_evaluate', *connection, '--expression', 'localStorage.getItem("parity")')['result'] == '中文 value'
                assert rust('storage_get', *connection, '--key', 'parity') == {'key':'parity','value':'中文 value'}
                assert rust('storage_set', *connection, '--items', '{"number":7,"enabled":true,"text":"fixture"}') == {'set':3}
                stored = python('js_evaluate', *connection, '--expression', 'Object.assign({}, localStorage)')['result']
                assert rust('storage_get', *connection) == {'items':stored,'count':4}
                print('PASS storage: reference wrapper bug recorded; suh read/write verified against browser state', flush=True)
                # Disable per-command default viewport enforcement before
                # observing the custom window size on another CLI connection.
                env['AI_DEV_BROWSER_VIEWPORT'] = 'native'
                equivalent('window_set', '--width', '900', '--height', '600')
                viewport = equivalent('js_evaluate', '--expression', '[innerWidth,innerHeight]')
                assert viewport['result'] == [900,600], viewport
                equivalent('dialog_respond')
                for implementation in (python, rust):
                    created = implementation('tab_new', *connection)
                    assert created['url'] == 'about:blank', created
                    listing = implementation('tab_list', *connection)
                    assert listing['count'] == 2, listing
                    blank = next(tab for tab in listing['tabs'] if tab['url'] == 'about:blank')
                    switched = implementation('tab_switch', *connection, '--tab-id', blank['id'])
                    assert switched == {'url':'about:blank','title':blank['title']}, switched
                    closed = implementation('tab_close', *connection, '--tab-id', blank['id'])
                    # The reference only disconnects the Tab WebSocket; its
                    # page remains open. suh actually closes the page target.
                    expected = {'remaining':2} if implementation is python else {'closed':True,'remaining':1}
                    assert closed == expected, closed
                    refreshed = implementation('tab_list', *connection)
                    assert refreshed['count'] == (2 if implementation is python else 1), refreshed
                    if implementation is python:
                        leftover = next(tab['id'] for tab in refreshed['tabs'] if tab['url'] == 'about:blank')
                        assert rust('tab_close', *connection, '--tab-id', leftover) == {'closed':True,'remaining':1}
                print('PASS tabs: create/list/switch; reference close bug recorded; suh closes the page target', flush=True)
                cross_url = f'http://localhost:{server.server_port}/crossframe'
                rust('js_evaluate', *connection, '--expression', 'const frame=document.createElement("iframe");frame.id="cross-frame";frame.src=' + json.dumps(cross_url) + ';document.body.appendChild(frame); true')
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    targets = rust('cdp_send', *connection, '--method', 'Target.getTargets')['result']['targetInfos']
                    if any(target['type'] == 'iframe' and target['url'] == cross_url for target in targets):
                        break
                    time.sleep(.05)
                else:
                    raise AssertionError(('cross-origin iframe target was not published', targets))
                cross = equivalent('js_evaluate', '--frame', 'localhost', '--expression', '({url:location.href,field:!!document.querySelector("#child-field")})')
                assert cross['result'] == {'url':cross_url,'field':True}, cross
                print('PASS cross-origin frame: explicit session evaluates the child document', flush=True)
                fixture_dom = '<div role="grid"><div role="row">Alpha row<input type="checkbox"></div><div role="row" id="beta">Beta row<input type="checkbox" id="picked"></div></div><div id="scroller" style="height:100px;overflow:auto"><div style="height:1000px;position:relative"><button style="position:absolute;bottom:0">End target</button></div></div>'
                rust('js_evaluate', *connection, '--expression', 'document.body.innerHTML=' + json.dumps(fixture_dom) + '; window.rowClicks=0; window.rowDoubles=0; document.querySelector("#beta").addEventListener("click",()=>window.rowClicks++); document.querySelector("#beta").addEventListener("dblclick",()=>window.rowDoubles++); true')
                for extra, expected in [([], [1,0,False]), (['--double'], [2,1,False]), (['--checkbox'], [1,0,True])]:
                    outcomes = []
                    for implementation in (python, rust):
                        rust('js_evaluate', *connection, '--expression', 'window.rowClicks=0;window.rowDoubles=0;document.querySelector("#picked").checked=false; true')
                        result = implementation('click_row_by_text', *connection, '--text', 'Beta row', *extra)
                        actual = rust('js_evaluate', *connection, '--expression', '[rowClicks,rowDoubles,document.querySelector("#picked").checked]')['result']
                        assert result['clicked'] and actual == expected, (extra, result, actual)
                        outcomes.append(result)
                    assert outcomes[0] == outcomes[1], (extra, outcomes)
                print('PASS row clicks: single, double and checkbox state match', flush=True)
                for flag, before, expected in [('--to-bottom', 0, 900), ('--to-top', 900, 0), ('--to-element', 0, None)]:
                    outcomes = []
                    for implementation in (python, rust):
                        rust('js_evaluate', *connection, '--expression', f'document.querySelector("#scroller").scrollTop={before}')
                        result = implementation('page_scroll', *connection, flag, *(['End target'] if flag == '--to-element' else []))
                        actual = rust('js_evaluate', *connection, '--expression', 'document.querySelector("#scroller").scrollTop')['result']
                        assert result['scrolled'] and (actual == expected if expected is not None else actual > 0), (flag, result, actual)
                        outcomes.append(result)
                    assert outcomes[0] == outcomes[1], (flag, outcomes)
                print('PASS scrolling: container edges and element visibility move the actual scroller', flush=True)
            finally:
                assert rust('browser_stop', *connection).get('stopped')
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


if __name__ == '__main__':
    main()
