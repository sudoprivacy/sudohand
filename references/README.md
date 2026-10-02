# Reference sources

## CLI Steering Engineering

The recurring audit uses the separately maintained
[CLI Steering Engineering skill](https://github.com/sudoprivacy/cli-steering-engineering/blob/5f1491743b1c70c639fc5d6933fd0800b407bda1/SKILL.md),
pinned in [cli-steering-engineering.json](cli-steering-engineering.json).
The upstream is currently private. It is an optional reference checkout, so
public clones, CI and product packages do not require private Git credentials.

```sh
# Reuse an authenticated local checkout (also accepts directory symlinks):
python scripts/steering_reference.py --source /path/to/cli-steering-engineering
# Or use your existing Git credentials to fetch the exact pin:
python scripts/steering_reference.py --fetch
```

The original local entry is
`C:/Users/songym/cursor-projects/document-ai/.claude/skills/cli-steering-engineering`.
The verifier prints the resolved SKILL.md path to read. An alternative is the
`CLI_STEERING_ENGINEERING_SOURCE` environment variable. No credential is stored
in this repository, and the private skill content is not redistributed.

To update: review the upstream commit diff, update the manifest pin, load that
revision, then repeat [the audit](../docs/cli-steering-audit.md). Updates are
deliberate; CI never follows the upstream branch automatically.

## ai-dev-browser

`ai-dev-browser/` is a Git submodule pinned to **v0.51.1**,
`c349d347251779357136685b6a698e3d2c59ce6d`. It preserves the source and
behavior baseline for the [browser migration](../docs/migrations/ai-dev-browser-to-sudohand.md).
It is reference material, outside the Cargo workspace and product packaging.
The source retains its original license and history.

## Fetch the baseline

```sh
git submodule update --init references/ai-dev-browser
git -C references/ai-dev-browser rev-parse HEAD
```

Compare the result with `commit` in
[ai-dev-browser-baseline.json](ai-dev-browser-baseline.json). The manifest also
lists the 61 public CLI tools. A matching command name is not proof of matching
behavior; the migration plan tracks the live acceptance gates.

The existing parity PR #27 originally used adb v0.38.1. Pinning this newer
reference does not certify or silently update that PR's tests.

The [history and behavior audit](../docs/migrations/history-audit.md) indexes
all 305 commits reachable from this pin and maps the current CLI, SDK, state,
tests and release contracts into 27 migration units. Regenerate the inventory
with `python scripts/audit_adb_reference.py --write` after reviewing a baseline
change; CI uses `--check` to detect drift. This check does not certify Rust behavior.

## Update deliberately

1. Fetch an explicit upstream tag in the submodule and review changes since the
   recorded commit, including tools, defaults, return values and failures.
2. Check out that tag in detached mode. Update this manifest and the submodule
   gitlink together in a dedicated PR; retain the prior SHA in Git history.
3. Update contract checks and real browser workflows for each changed behavior.
   Run them against both implementations and record unresolved differences.
4. Update affected consumers in the migration ledger before accepting the new
   baseline. Do not use `git submodule update --remote` as an unattended update.

Archiving the upstream repository later does not remove this reference. Existing
tags and published packages must remain available for audits and rollback.
