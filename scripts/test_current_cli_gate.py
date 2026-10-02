#!/usr/bin/env python3
"""Use the real CLI/parser to ensure a green gap-ledger check cannot hide drift."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suh', type=Path, required=True)
    parser.add_argument('--reference', type=Path, required=True)
    args = parser.parse_args()
    command = [sys.executable, str(ROOT / 'scripts/cli_parity.py'), '--suh', str(args.suh.resolve()),
               '--reference', str(args.reference.resolve())]
    ledger = json.loads((ROOT / 'docs/migrations/current-cli-gaps.json').read_text(encoding='utf-8'))
    with tempfile.TemporaryDirectory(prefix='suh-current-gate-') as temporary:
        report = Path(temporary) / 'report.json'
        strict = subprocess.run([*command, '--report', str(report)], capture_output=True, text=True, encoding='utf-8', timeout=120)
        observed = json.loads(report.read_text(encoding='utf-8'))
        assert observed['gaps'] == ledger['gaps'] and observed['reference_commands'] == 61, observed
        if ledger['gaps']:
            assert strict.returncode == 1 and observed['declaration_parity'] == 'failed', strict
        else:
            assert strict.returncode == 0 and observed['declaration_parity'] == 'passed', strict
        alterations = [('extra gap', lambda d: d['gaps'].append({'command': 'nonexistent', 'kind': 'missing_command'}), 'Gap ledger drift'),
                       ('wrong pin', lambda d: d.update(reference_commit='0' * 40), 'different baseline')]
        if ledger['gaps']:
            alterations.append(('removed gap', lambda d: d['gaps'].pop(), 'Gap ledger drift'))
        for label, change, expected in alterations:
            altered = json.loads(json.dumps(ledger))
            change(altered)
            path = Path(temporary) / 'altered.json'
            path.write_text(json.dumps(altered), encoding='utf-8')
            result = subprocess.run([*command, '--known-gaps', str(path)], capture_output=True, text=True, encoding='utf-8', timeout=120)
            assert result.returncode == 1 and expected in result.stderr, (label, result)
            print(f'PASS gate rejects {label}', flush=True)
    print('PASS strict parity and exact gap-ledger failure behavior', flush=True)


if __name__ == '__main__':
    main()
