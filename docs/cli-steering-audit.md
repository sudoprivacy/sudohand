# CLI steering audit

An agent must discover the right operation, supply valid arguments, understand
the outcome, and recover when it fails. Repeat this audit whenever that surface
changes, including wrappers and consumer skills.

## Source and ownership

The upstream guidance is
[CLI Steering Engineering](https://github.com/sudoprivacy/cli-steering-engineering/blob/8e597fc872ee0653790960cd96e67f59b5c38a8f/SKILL.md).
Its [manifest](../references/cli-steering-engineering.json) pins the reviewed
revision. [Load it locally](../references/README.md#cli-steering-engineering)
with existing Git access. It is currently private; public CI checks the local
implementation without fetching or redistributing the skill.

Update the pin only after reviewing its diff. Record new patterns upstream when
authorized. Do not silently turn an inaccessible skill or unavailable API into
a passing audit.

## Routine review

1. Read the verified skill and the exact surfaces an agent receives: `suh
   describe`, domain help, command help, consumer wrapper listings and errors.
2. Check the first sentence: when to choose this operation, what the result
   provides, and any sibling scope differences. Check actual iframe/input
   behavior before promising it in help.
3. Follow names, arguments/defaults, returned values and errors through SDK,
   CLI and consumers. Lists should remain lists. Keep decision text in one
   source; the initial six browser tools use `crates/sudohand-browser/help/`
   for SDK docs, Clap help and the generated command listing.
4. Exercise successful outcomes and failures. Distinguish an unsuccessful
   search from a failed action. Report uncertain side effects without inviting
   automatic replay. Put recovery hints in the actual error response.
5. Run a real model without giving it the expected command name. Inspect its
   first choice, arguments, extra calls, recovery, and the final application
   state. Fix a broken primitive when the model chose correctly.
6. Add the observed journey to live regression coverage, run it locally, and
   record the executable hash, client/model, command trace, screenshots and
   outcome. Add consumer acceptance before claiming downstream migration.

## Executable checks

```sh
cargo build --locked -p sudohand-cli
python scripts/audit_cli_steering.py --suh target/debug/suh
suh describe --domain browser --with-args
python scripts/live_browser_foundation.py --suh target/debug/suh --output artifacts/foundation
# Claude Code with a real API key or a working local login:
python scripts/live_cli_steering.py --client claude --suh target/debug/suh --output artifacts/model
```

On Windows use `target/debug/suh.exe`. The browser scripts need `psutil` and an
installed Chrome; `AI_DEV_BROWSER_CHROME` selects the executable. They launch
disposable profiles and a local HTTP page. Missing prerequisites fail the run.
Run them from a live PTY for local acceptance and inspect the saved screenshots.

The mechanical audit checks real help/describe output and structured argument
failures. It currently requires shared decision text for six tools; it is not a
certification of every command. The browser suite verifies stale-locator
recovery, trusted linear/human dragging, JavaScript failure without replay,
mobile viewport persistence, and a visible saved draft after reload.

The model suite supplies `describe --domain browser --with-args` and help on demand. It allows
bounded fixture calls, feeds the actual results back, and checks first choice,
call count, completion and page state. This isolates steering from a coding
agent's other tools. Claude Code runs in safe mode with built-in tools disabled;
global coding-agent skills/MCP tools must not interfere with this benchmark.
It does not replace a full sudowork/Electron session test.
Model authentication stays in the existing client; private keys are never
written into the test report.

## Automation and evidence

[Browser parity CI](../.github/workflows/browser-parity.yml) runs the mechanical
and real-Chrome checks on Linux, macOS and Windows for pull requests and main.
The manually dispatched `live_model` option additionally uses the
`ANTHROPIC_API_KEY` repository secret on Linux. An explicitly requested live run
with missing credentials fails. Ordinary CI has no model-acceptance claim.
Artifacts retain JSON traces and screenshots. Public pull requests never run
paid model requests or fetch the private skill.

Review live evidence before committing steering changes. Rerun model cases when
tool descriptions, defaults, result shape, failure guidance or the model/client
change. A green wording check alone is insufficient.

## Current boundaries

The [2026-10-02 acceptance record](migrations/browser-foundation-acceptance.json)
contains the actual model choices, results, binary hash and local browser checks.
Four model scenarios passed using real API credentials; this is limited evidence,
not a claim that every command or consumer has passed.

- This batch shares help for six tools; other SDK/CLI descriptions and runtime
  locator hints still need consolidation by capability family.
- Missing HTML-id/XPath click targets now raise a core error, mapped to CLI
  `not_found`, exit 4, `retryable: false`, and a recovery hint. Search
  `found: false` remains a valid result. Other action failure dictionaries need
  the same deliberate audit; do not convert every false field into an error.
- JavaScript exceptions map to `evaluation`, exit 1, with guidance about
  preceding side effects. IO exit 9 no longer promises that replay is safe.
- `window_set` adds `viewport_persisted`. Managed CDP browsers retain explicit
  sizes across calls/new tabs; an external browser without a registry entry
  reports false. Extension tab acquisition leaves the user's viewport alone.
- Full consumer acceptance, recording migration, source-license provenance,
  platform packaging, releases and archive gates remain in the
  [migration plan](migrations/ai-dev-browser-to-sudohand.md).
