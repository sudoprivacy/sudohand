# sudohand — design & consolidation plan

## Why this repo
`ai-dev-browser` (adb) and `ai-desktop-control` (adc) are already Rust,
already share the same DNA — OS-neutral backend trait + real backend +
fake backend, library + thin CLI, `{"error":{}}` / exit-1 contract, "no
policy/session/transport; integrators wrap it." Filesystem and shell are
the other two local-computer-control primitives. Consolidating all four
under one workspace gives integrators (apeiron-bridge) one dependency,
aligned versioning, shared CI, and shared value types / error model /
encoding / permission probing.

## Shape: a Cargo workspace of crates, NOT one merged crate
- `sudohand-core` — shared `Error`, value types, JSON-CLI helpers, permissions.
- `sudohand-browser` / `sudohand-desktop` / `sudohand-fs` / `sudohand-shell` —
  each a lib (+ behavior via the umbrella CLI).
- `sudohand-cli` — the `sudohand` binary, dispatches domain subcommands.

Separate crates keep them independently usable/testable/versioned and let
per-crate platform `cfg` stay clean (desktop is macOS-only via AX/CG;
browser/fs/shell are cross-platform). Integrators link only what they need.

## Naming decisions (2026-08-29)
- Umbrella = **sudohand** (the "hands" of the sudo agent stack; pairs with
  apeiron; distinctive). Considered: `computer-control` (plain but generic),
  `genie` (rejected — saturated in AI: Google Genie / Netflix Genie / WSL
  `genie`; and it connotes the assistant/brain layer, not the actuator
  layer), a Greek "action/doing" word (fine but obscure).
- **Drop `adb`/`adc`/`ad*`**: `adb` collides with Android Debug Bridge on
  PATH, and the `ad` prefix already expands inconsistently (ai-**dev**-browser
  vs ai-**desktop**-control). Use domain subcommands: `sudohand browser|desktop|fs|shell`.

## RED LINE: shell
Per the apeiron-bridge capability model, **shell is not exposed to agents
by default** (file/browser/desktop are the three authorized domains).
Resolution: the library layer stays neutral and *may* contain
`sudohand-shell`, but the bridge enforces the red line by simply not
linking/registering it. A `shell` crate existing ≠ an agent getting a
shell. If shell is ever exposed, it needs its own authz model
(command allow-list, cwd fence, timeout, dry-run), separate from the others.

## Sequencing
1. **core**: extract shared `Error` / value types / JSON-CLI / permission
   probing from adc & adb into `sudohand-core`.
2. Re-home **adc → sudohand-desktop** and **adb → sudohand-browser** as member
   crates depending on core (public interface unchanged; integrators unaffected).
3. Add **sudohand-fs** (thin over `std::fs`) and **sudohand-shell**
   (thin over `std::process::Command`), each with fake backends for
   side-effect-free tests.
4. Fill in the `sudohand` CLI subcommand trees.

Do the port after adb/adc each stabilized (both just landed their current
state) — this is the right moment, before they grow more divergent conventions.

## Flows stay inside one actuator (2026-08-29)
adc's agent layer (VLM locate, workflow DSL, graph-flow orchestration,
registry) lives in `sudohand-desktop` behind the `agent` feature (off by
default for the library; on in `sudohand-cli`). `sudohand-browser` has its own
(`flow` feature: step DSL over the tool locators, graph-flow orchestration,
registry with `form-signup` / `page-extract`). **No cross-actuator
workflows** — a flow is desktop-only or browser-only; composing domains is
the integrator's job.

## Conventions to keep
- JSON on stdout; `{"error":{kind,message}}` on stderr (adc's exact shape;
  `kind` ∈ permission_denied/not_found/invalid_input/io/internal); exit 1 on failure.
- No policy/session/transport/confirmation in any crate.
- CI: `cargo build` + `cargo clippy --all-targets -- -D warnings` +
  `cargo test` + `cargo fmt --check`. Pin one toolchain across the
  workspace so local clippy matches CI (avoid the "local green, CI red"
  drift from mismatched clippy versions).
