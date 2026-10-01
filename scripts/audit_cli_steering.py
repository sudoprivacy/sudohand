#!/usr/bin/env python3
"""Check the executable's discovery/help/error surfaces. Does not grade LLM behavior."""
import argparse
import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def audit(suh):
    def run(*args):
        return subprocess.run([str(suh), *args], capture_output=True, text=True, encoding='utf-8', timeout=20, check=False)

    for args in [('--help',), ('browser', '--help'), ('serve', '--help')]:
        output = run(*args)
        assert output.returncode == 0 and 'Usage:' in output.stdout, output
    output = run('describe')
    assert output.returncode == 0, output.stderr
    listing = {}
    for line in output.stdout.splitlines():
        if line.startswith('action\tbrowser\t'):
            _, _, name, description = line.split('\t', 3)
            assert name not in listing and description.strip(), line
            listing[name] = description
    assert listing and 'bridge-serve' not in listing, listing
    detailed = run('describe', '--domain', 'browser', '--with-args')
    assert detailed.returncode == 0, detailed.stderr
    assert 'action\tdesktop\t' not in detailed.stdout
    parameters = {parts[2]: parts[3] for line in detailed.stdout.splitlines()
                  if line.startswith('args\tbrowser\t') and len(parts := line.split('\t', 3)) == 4}
    assert set(parameters) == set(listing), 'arguments missing from discovery'
    assert '--html-id <value> required' in parameters['click_by_html_id']
    assert '--from-x <value> required' in parameters['mouse_drag']
    assert '--steps <value> default=10' in parameters['mouse_drag']
    shared = []
    for source in sorted((ROOT / 'crates/sudohand-browser/help').glob('*.md')):
        summary = source.read_text(encoding='utf-8').splitlines()[0]
        assert summary.startswith('Use '), source
        assert listing.get(source.stem) == summary, f'{source.stem}: listing lost the decision signal'
        help_output = run('browser', source.stem, '--help')
        assert help_output.returncode == 0, help_output.stderr
        # Clap may wrap lines to fit the terminal width.
        assert ' '.join(summary.split()) in ' '.join(help_output.stdout.split()), source
        shared.append(source.stem)
    invalid = run('browser', 'click_by_html_id', '--definitely-invalid')
    assert invalid.returncode == 2 and not invalid.stdout, invalid
    error = json.loads(invalid.stderr)['error']
    assert error['kind'] == 'invalid_input' and error['retryable'] is False and error['hint'], error
    return {'schema': 1, 'mechanical_checks': 'passed', 'browser_tools_listed': len(listing),
            'shared_help_tools': shared, 'live_model_behavior': 'not tested by this check'}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suh', type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(audit(args.suh.resolve()), indent=2))
