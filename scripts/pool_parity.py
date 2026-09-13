#!/usr/bin/env python3
"""Execute the pinned Python scheduler and check its shared Rust test fixture."""
import argparse
import asyncio
import json
import logging
from pathlib import Path
import sys


async def scenario(pool_type, position, budget):
    calls, closed = [], []
    attempts = {}

    class FixtureError(Exception):
        pass

    class BusinessFailure:
        success = False

        def to_dict(self):
            return {'label': 'business', 'success': False}

    class Client:
        def __init__(self, *, port, headless, **kwargs):
            assert port > 0 and headless and 'cookies_file' not in kwargs

        async def __aenter__(self):
            return self

        async def __aexit__(self, *_):
            closed.append(True)

        async def perform(self, label):
            attempt = attempts.get(label, 0) + 1
            attempts[label] = attempt
            calls.append([label, attempt])
            if label == 'terminal' or label == 'retry' and attempt < 3:
                raise FixtureError(label)
            if label == 'business':
                return BusinessFailure()
            return {'label': label, 'attempt': attempt}

    labels = ['retry', 'ok', 'business'] + ([] if budget == -1 else ['terminal'])
    async with pool_type(Client, workers=1, max_retries=budget, profile='temp',
                         headless=True, close_browsers=False, requeue_position=position) as pool:
        ids = {label: await pool.run('perform', label, _hold=True) for label in labels}
        results = await pool.wait(list(ids.values()), timeout=5)
        outcomes = {}
        for label, job_id in ids.items():
            result = results[job_id]
            outcomes[label] = {key: getattr(result, key) for key in
                               ('success', 'data', 'error', 'error_type', 'error_bases', 'worker_id')}
            outcomes[label]['error_bases'] = outcomes[label]['error_bases'] or []
        stats = pool.worker_stats[0]
        statistics = {'success': stats.success, 'fail': stats.fail, 'total': stats.total}
    return {'position': position, 'budget': budget, 'calls': calls, 'results': outcomes,
            'stats': statistics, 'closed': len(closed)}


async def generate(pool_type):
    return [await scenario(pool_type, position, budget)
            for position in ('front', 'back') for budget in (0, 1, 2, -1)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--reference', type=Path, required=True)
    parser.add_argument('--fixture', type=Path, required=True)
    parser.add_argument('--write', action='store_true')
    args = parser.parse_args()
    sys.path.insert(0, str(args.reference.resolve()))
    from ai_dev_browser.pool import BrowserPool
    logging.disable(logging.CRITICAL)  # The scenarios intentionally raise errors.
    observed = asyncio.run(generate(BrowserPool))
    if args.write:
        args.fixture.write_text(json.dumps(observed, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    else:
        expected = json.loads(args.fixture.read_text(encoding='utf-8'))
        assert observed == expected, (observed, expected)
    print('PASS running Python scheduler: eight queue/retry/business/error/cleanup scenarios')


if __name__ == '__main__':
    main()
