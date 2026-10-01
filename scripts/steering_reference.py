#!/usr/bin/env python3
"""Verify or fetch the pinned CLI steering skill without a mandatory private submodule."""
import argparse
import json
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def git(path, *args):
    return subprocess.check_output(
        ['git', '-C', str(path), *args], text=True, encoding='utf-8', timeout=120
    ).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, help='existing skill checkout (symlinks accepted)')
    parser.add_argument('--fetch', action='store_true', help='fetch the pin into the ignored reference directory')
    args = parser.parse_args()
    manifest = json.loads((ROOT / 'references/cli-steering-engineering.json').read_text())
    source = args.source or os.environ.get('CLI_STEERING_ENGINEERING_SOURCE')
    if args.fetch and source:
        parser.error('--fetch cannot be combined with an existing source')
    checkout = Path(source or ROOT / manifest['optional_checkout']).resolve()
    if args.fetch:
        if not checkout.exists():
            subprocess.run(['git', 'clone', manifest['repository'], str(checkout)], check=True, timeout=120)
        if git(checkout, 'status', '--porcelain'):
            parser.error(f'refusing to change a dirty checkout: {checkout}')
        git(checkout, 'fetch', 'origin', manifest['commit'])
        git(checkout, 'checkout', '--detach', manifest['commit'])
    if not (checkout / manifest['skill']).is_file():
        parser.error('skill unavailable; pass --source, set CLI_STEERING_ENGINEERING_SOURCE, or use --fetch with Git access')
    if git(checkout, 'rev-parse', 'HEAD') != manifest['commit']:
        parser.error('skill revision differs from the manifest; review the upstream diff before updating the pin')
    if Path(git(checkout, 'rev-parse', '--show-toplevel')).resolve() != checkout:
        parser.error('source must resolve to the skill repository root')
    if git(checkout, 'status', '--porcelain'):
        parser.error('skill checkout has local changes; audit the recorded revision from a clean checkout')
    print(json.dumps({'status': 'verified', 'commit': manifest['commit'], 'skill': str(checkout / manifest['skill'])}))


if __name__ == '__main__':
    main()
