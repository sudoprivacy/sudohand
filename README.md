# sudohand

**AI computer-control actuators** — the layer that turns an agent's
intent into real actions on a computer: **browser · desktop ·
filesystem · shell**, as one Rust workspace.

*sudohand*: the hands of the sudo agent stack — the counterpart to the
thinking layer. Sibling to [`apeiron`](https://github.com/joezhoujinjing/apeiron)
(the service/brain layer); sudohand is what actually touches the machine.

Each actuator is a **library plus a thin CLI** and contains **no
policy, sessions, auditing, confirmation prompts, or transport** — an
integrator (e.g. `apeiron-bridge`) links the crates and wraps them
with those concerns. Success prints JSON to stdout; failure prints
`{"error":{...}}` to stderr and exits 1.

## Layout

```
sudohand/
├─ crates/
│  ├─ sudohand-core       # Error model, value types, JSON-CLI conventions
│  ├─ sudohand-browser    # Chrome over CDP        (absorbs adb / ai-dev-browser)
│  ├─ sudohand-desktop    # windows/AX/input/shots (absorbs adc / ai-desktop-control, macOS)
│  ├─ sudohand-fs         # filesystem ops
│  └─ sudohand-shell      # command runner  (see DESIGN.md — gated by integrators)
└─ sudohand-cli           # the `sudohand` binary: `sudohand <browser|desktop|fs|shell> …`
```

## CLI

```
sudohand browser ...     # was `adb`
sudohand desktop ...     # was `adc`
sudohand fs ...
sudohand shell ...       # off by default at the integrator
```

One binary with domain subcommands — self-describing, and no clash
with Android's `adb`.

## Status

`sudohand-desktop` is ported from `ai-desktop-control` (backend trait,
value types, `MacBackend`, `FakeBackend`) and wired up as
`sudohand desktop status|apps|screenshot|ax-tree|activate|click|type|paste-file|key`;
(+ `locate|workflows|flow` via the `agent` feature);
`sudohand-core` carries the shared `Error`, JSON-CLI contract, base64 and
permission probing. `sudohand-browser` is ported from `ai-dev-browser` — all 56 tools as
`sudohand browser <tool>` with adb's names/flags/JSON, plus
`workflows|flow` via the `flow` feature (built-ins `form-signup`,
`page-extract`). `sudohand-fs` (`sudohand fs read|write|ls|stat|mkdir|rm|mv|cp|exists`)
and `sudohand-shell` (`sudohand shell run`) are thin over `std::fs` /
`std::process::Command`, each with a fake backend. See [DESIGN.md](DESIGN.md).
