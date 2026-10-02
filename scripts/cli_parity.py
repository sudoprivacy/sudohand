#!/usr/bin/env python3
"""Check public CLI command/flag coverage against the pinned Python parser.
This checks declarations only; the browser differential suites verify behavior.
"""
import argparse
import hashlib
import importlib
import inspect
import json
from pathlib import Path
import re
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suh', type=Path, required=True)
    parser.add_argument('--reference', type=Path, required=True)
    parser.add_argument('--write-defaults', type=Path, help='Explicitly regenerate the pinned parser-default fixture')
    parser.add_argument('--check-defaults', type=Path, help='Verify the committed fixture against the reference parser')
    parser.add_argument('--report', type=Path, help='Write observations even when parity fails')
    parser.add_argument('--known-gaps', type=Path, help='Check an exact, reviewed gap ledger; does not establish full parity')
    args = parser.parse_args()
    reference_commit = subprocess.check_output(['git', '-C', str(args.reference), 'rev-parse', 'HEAD'], text=True).strip()
    assert not subprocess.check_output(['git', '-C', str(args.reference), 'status', '--porcelain', '--untracked-files=no'], text=True).strip(), 'Reference has tracked edits'
    sys.path.insert(0, str(args.reference.resolve()))
    from ai_dev_browser._cli import _generate_parser, INJECTED_FIRST_PARAMS
    failures = []
    observations = []
    default_fixture = []
    commands = 0
    flags = 0
    defaults = 0
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
        for action in reference._actions:
            if action.default is None or action.dest == 'help' or not isinstance(action.default, (str, int, float, bool)):
                continue
            for flag in action.option_strings:
                if flag.startswith('--'):
                    default_fixture.append({'command': source.stem, 'flag': flag[2:], 'default': action.default})
        expected = {flag for action in reference._actions for flag in action.option_strings if flag.startswith('--')}
        observation = {'command': source.stem, 'reference_flags': sorted(expected),
                       'checked_defaults': [], 'unchecked_defaults': []}
        observations.append(observation)
        result = subprocess.run([str(args.suh.resolve()), 'browser', source.stem, '--help'], capture_output=True, text=True, encoding='utf-8', timeout=10)
        if result.returncode:
            # A failed executable must not be accepted as a missing command.
            error = json.loads(result.stderr)['error']
            assert result.returncode == 2 and error['kind'] == 'invalid_input' and 'unrecognized subcommand' in error['message'], result
            failures.append({'command': source.stem, 'kind': 'missing_command'})
            observation['available'] = False
            continue
        observation['available'] = True
        actual = set(re.findall(r'(?<![\w-])--[a-z][a-z0-9-]*', result.stdout))
        missing = sorted(expected - actual)
        observation['target_flags'] = sorted(actual)
        if missing:
            failures.append({'command': source.stem, 'kind': 'missing_flags', 'flags': missing})
        # Clap displays scalar defaults; compare them to argparse's effective
        # defaults, including values transformed by the Python CLI decorator.
        sections = {}
        current = None
        for line in result.stdout.splitlines():
            match = re.match(r'\s+(?:-\w, )?(--[a-z][a-z0-9-]*)\b', line)
            if match:
                current = match[1]
                sections[current] = line
            elif current:
                sections[current] += ' ' + line
        for action in reference._actions:
            if action.default is None or isinstance(action.default, bool):
                continue
            for flag in action.option_strings:
                displayed = re.search(r'\[default: (.*?)\]', sections.get(flag, ''))
                if not displayed:
                    if flag.startswith('--'):
                        observation['unchecked_defaults'].append(flag)
                    continue
                actual_default = displayed[1]
                try:
                    if isinstance(action.default, (int, float)):
                        matches = float(actual_default) == action.default
                    else:
                        matches = actual_default.strip('"') == str(action.default)
                except ValueError:
                    matches = False
                if not matches:
                    failures.append({'command': source.stem, 'kind': 'default', 'flag': flag,
                                     'reference': action.default, 'target': actual_default})
                observation['checked_defaults'].append(flag)
                defaults += 1
        commands += 1
        flags += len(expected)
    assert commands > 0, 'No reference CLI commands discovered'
    report = {'schema': 1, 'reference_commit': reference_commit,
              'binary_sha256': hashlib.sha256(args.suh.read_bytes()).hexdigest(),
              'declaration_parity': 'failed' if failures else 'passed',
              'full_migration': 'not_assessed',
              'scope': 'Command names, long flags and displayed scalar defaults only. Types, choices, boolean defaults, unshown defaults, results and runtime behavior require separate checks.',
              'reference_commands': len(observations), 'available_commands': commands,
              'displayed_defaults_checked': defaults, 'gaps': failures, 'commands': observations}
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    if args.known_gaps:
        ledger = json.loads(args.known_gaps.read_text(encoding='utf-8'))
        assert ledger['reference_commit'] == reference_commit, 'Gap ledger refers to a different baseline'
        assert ledger['reference_commands'] == len(observations), 'Reference command discovery changed'
        assert ledger['gaps'] == failures, f'Gap ledger drift: review additions AND removals; observed {failures}'
        print(f'GAP LEDGER MATCH: {len(failures)} open declarations across {len(observations)} reference tools; full parity is NOT established')
    else:
        assert not failures, failures
    if args.check_defaults:
        assert json.loads(args.check_defaults.read_text(encoding='utf-8')) == default_fixture, 'Pinned parser defaults fixture is stale'
    if args.write_defaults:
        args.write_defaults.parent.mkdir(parents=True, exist_ok=True)
        args.write_defaults.write_text(json.dumps(default_fixture, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    if not failures:
        print(f'PASS {commands} reference CLI commands and {flags} long-flag declarations, {defaults} displayed scalar defaults; behavior checked separately')


if __name__ == '__main__':
    main()
