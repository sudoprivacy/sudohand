# Working on sudohand

## CLI steering is a recurring acceptance gate

When adding or changing a public tool, help text, parameters, result shape,
error handling, or a consumer's tool-discovery surface, apply
[CLI Steering Engineering](https://github.com/sudoprivacy/cli-steering-engineering/blob/5f1491743b1c70c639fc5d6933fd0800b407bda1/SKILL.md).
Its pinned source and loading instructions are in
[references/README.md](references/README.md#cli-steering-engineering).
Read the verified SKILL.md before the audit. If access is unavailable, say so
and run the public checks; do not report that as a full skill audit.

Use [the audit procedure](docs/cli-steering-audit.md). Check what the LLM
actually sees, its first tool choice, the returned state, and recovery after
failure. A passing help-text check does not establish model behavior.

Run actual browser/CLI workflows for behavior changes and check in the tests.
Prefer live PTY acceptance with a real authenticated model for steering changes.
Missing credentials or Chrome are failures to run, never successful skips.
Keep model traces and screenshots free of credentials and personal browser data.

## Migration

Preserve the pinned adb reference and update the migration evidence when a
capability is accepted. Do not mark the full migration complete from command
coverage or synthetic checks. Sudowork, other consumers, packaging, and release
gates are tracked in docs/migrations/ai-dev-browser-to-sudohand.md.
