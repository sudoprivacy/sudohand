#!/usr/bin/env python3
"""Regenerate synthetic legacy Cookie fixtures using the pinned Python model."""
import argparse
import json
from pathlib import Path
import pickle
import sys

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--reference', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
sys.path.insert(0, str(args.reference.resolve()))
from ai_dev_browser.cdp.network import Cookie, CookiePriority, CookieSameSite, CookieSourceScheme

cookie = Cookie(name='legacy', value='中文-value', domain='.example.test', path='/',
                size=12, http_only=True, secure=True, session=False,
                priority=CookiePriority.HIGH, source_scheme=CookieSourceScheme.SECURE,
                source_port=443, expires=2000000000, same_site=CookieSameSite.LAX)
args.output.mkdir(parents=True, exist_ok=True)
for protocol in (2, 4, 5):
    (args.output / f'legacy-cookies-p{protocol}.pickle').write_bytes(pickle.dumps([cookie], protocol=protocol))
(args.output / 'legacy-cookies.json').write_text(json.dumps([cookie.to_json()], ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
