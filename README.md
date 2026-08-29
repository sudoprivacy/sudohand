# praxis

**AI computer-control actuators** — the layer that turns an agent's
intent into real actions on a computer: **browser · desktop ·
filesystem · shell**, as one Rust workspace.

*praxis* (πρᾶξις, Greek): "action / doing" — the counterpart to
thinking. Sibling to [`apeiron`](https://github.com/joezhoujinjing/apeiron)
(the service/brain layer); praxis is the hands.

Each actuator is a **library plus a thin CLI** and contains **no
policy, sessions, auditing, confirmation prompts, or transport** — an
integrator (e.g. `apeiron-bridge`) links the crates and wraps them
with those concerns. Success prints JSON to stdout; failure prints
`{"error":{...}}` to stderr and exits 1.

## Layout

```
praxis/
├─ crates/
│  ├─ praxis-core       # Error model, value types, JSON-CLI conventions
│  ├─ praxis-browser    # Chrome over CDP        (absorbs adb / ai-dev-browser)
│  ├─ praxis-desktop    # windows/AX/input/shots (absorbs adc / ai-desktop-control, macOS)
│  ├─ praxis-fs         # filesystem ops
│  └─ praxis-shell      # command runner  (see DESIGN.md — gated by integrators)
└─ praxis-cli           # the `praxis` binary: `praxis <browser|desktop|fs|shell> …`
```

## CLI

```
praxis browser ...     # was `adb`
praxis desktop ...     # was `adc`
praxis fs ...
praxis shell ...       # off by default at the integrator
```

One binary with domain subcommands — self-describing, and no clash
with Android's `adb`.

## Status

`praxis-desktop` is ported from `ai-desktop-control` (backend trait,
value types, `MacBackend`, `FakeBackend`) and wired up as
`praxis desktop status|apps|screenshot|ax-tree|activate|click|type|paste-file|key`;
`praxis-core` carries the shared `Error`, JSON-CLI contract, base64 and
permission probing. `praxis-fs` (`praxis fs read|write|ls|stat|mkdir|rm|mv|cp|exists`) and
`praxis-shell` (`praxis shell run`) are thin over `std::fs` /
`std::process::Command`, each with a fake backend. `praxis-browser` is
still to be ported from `ai-dev-browser`. See [DESIGN.md](DESIGN.md).
