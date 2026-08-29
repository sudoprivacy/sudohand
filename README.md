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

Workspace skeleton. `praxis-browser` and `praxis-desktop` will be
ported in from the existing `ai-dev-browser` and `ai-desktop-control`
repos; `praxis-fs` / `praxis-shell` are new. See [DESIGN.md](DESIGN.md).
