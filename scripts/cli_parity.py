#!/usr/bin/env python3
"""Check public CLI command/flag coverage against the pinned Python parser.
This checks declarations only; the browser differential suites verify behavior.
"""
import argparse
import importlib
import inspect
from pathlib import Path
import re
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suh', type=Path, required=True)
    parser.add_argument('--reference', type=Path, required=True)
    args = parser.parse_args()
    sys.path.insert(0, str(args.reference.resolve()))
    from ai_dev_browser._cli import _generate_parser, INJECTED_FIRST_PARAMS
    failures = []
    commands = 0
    flags = 0
    for source in sorted((args.reference / 'ai_dev_browser' / 'tools').glob('*.py')):
        if source.stem.startswith('_'):
            continue
        module = importlib.import_module(f'ai_dev_browser.tools.{source.stem}')
        function = getattr(module, source.stem, None)
        if function is None or not hasattr(function, 'cli_main'):
            continue
        parameters = list(inspect.signature(function).parameters)
        requires_tab = bool(parameters and parameters[0] in INJECTED_FIRST_PARAMS)
        reference = _generate_parser(function, requires_tab=requires_tab)
        expected = {flag for action in reference._actions for flag in action.option_strings if flag.startswith('--')}
        result = subprocess.run([str(args.suh.resolve()), 'browser', source.stem, '--help'], capture_output=True, text=True, encoding='utf-8', timeout=10)
        if result.returncode:
            failures.append((source.stem, 'command missing', result.stderr))
            continue
        actual = set(re.findall(r'(?<![\w-])--[a-z][a-z0-9-]*', result.stdout))
        missing = sorted(expected - actual)
        if missing:
            failures.append((source.stem, missing))
        commands += 1
        flags += len(expected)
    assert commands > 0, 'No reference CLI commands discovered'
    assert not failures, failures
    print(f'PASS {commands} reference CLI commands and {flags} long-flag declarations; behavior checked separately')


if __name__ == '__main__':
    main()
