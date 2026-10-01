# sudohand — design & consolidation plan

## Why this repo
This Rust workspace consolidates browser capabilities from
`ai-dev-browser` (adb), desktop capabilities from `ai-desktop-control`
(adc), and filesystem and shell primitives. The adb reference is Python;
its Rust replacement and downstream migration are tracked in the
[migration plan](docs/migrations/ai-dev-browser-to-sudohand.md).
Consolidating the actuator crates gives integrators one workspace,
aligned versioning, shared CI, and shared value types / error model /
encoding / permission probing.

## Shape: a Cargo workspace of crates, NOT one merged crate
- `sudohand-core` — shared `Error`, value types, JSON-CLI helpers, permissions.
- `sudohand-browser` / `sudohand-desktop` / `sudohand-fs` / `sudohand-shell` —
  each a lib (+ behavior via the umbrella CLI).
- `sudohand-cli` — the `suh` binary, dispatches domain subcommands.

Separate crates keep them independently usable/testable/versioned and let
per-crate platform `cfg` stay clean (desktop is macOS-only via AX/CG;
browser/fs/shell are cross-platform). Integrators link only what they need.

## Naming decisions (2026-08-29)
- Umbrella = **sudohand** (the "hands" of the sudo agent stack).
  Considered: `computer-control` (plain but generic),
  `genie` (rejected — saturated in AI: Google Genie / Netflix Genie / WSL
  `genie`; and it connotes the assistant/brain layer, not the actuator
  layer), a Greek "action/doing" word (fine but obscure).
- **Drop `adb`/`adc`/`ad*`**: `adb` collides with Android Debug Bridge on
  PATH, and the `ad` prefix already expands inconsistently (ai-**dev**-browser
  vs ai-**desktop**-control). Use domain subcommands: `suh browser|desktop|fs|shell`.

## RED LINE: shell
**`suh serve` does not expose shell operations.** The library layer
contains `sudohand-shell`; other integrators choose which capabilities
to link and register. A `shell` crate existing does not grant an agent
access to it. Exposing shell requires an explicit authorization model
(command allow-list, cwd fence, timeout, dry-run), separate from the others.

## Sequencing
1. **core**: extract shared `Error` / value types / JSON-CLI / permission
   probing from adc & adb into `sudohand-core`.
2. Port **adc → sudohand-desktop** and **adb → sudohand-browser** as member
   crates depending on core. Verify the CLI contract and migrate SDK consumers
   explicitly; matching command names does not preserve Python object APIs.
3. Add **sudohand-fs** (thin over `std::fs`) and **sudohand-shell**
   (thin over `std::process::Command`), each with fake backends for
   side-effect-free tests.
4. Fill in the `suh` CLI subcommand trees.

Use the migration plan's pinned reference and acceptance gates to track
browser coverage, releases and consumer cutovers.

## One workflow engine: sudohand-flow (2026-08-30, revised)
There is a single workflow engine, `sudohand-flow`: a `Workflow` is a Rust
`Step` sequence compiled to a `graph_flow` graph, each step running one
basic action — any `suh` subcommand (fs/shell/desktop/browser or an
extension's) via a `Dispatch` (`CliDispatch` execs `$SUH_BIN`) — with
results bound into `{{var}}`s and routing by id (`on_ok`/`on_fail`,
`Branch` on a `Cond`, `Goto` loops) plus interactive confirm/prompt/select.
It links no actuator crate; VLM element location/judgement are the
`desktop locate`/`ask` and `browser locate`/`ask` actions (both over the
shared `sudohand-vlm` crate), chained by variable. The earlier per-actuator flow engines (`sudohand-desktop::flow`
and `sudohand-browser::flow`, graph_flow + VLM self-heal, domain-bound)
have been **removed** — desktop/browser keep only their VLM actions and
geometry helpers; graph_flow lives only in sudohand-flow. Extensions
register workflows into a `sudohand_flow::Registry` and run them via
`Ctx::run_workflow` / `Ctx::flow`.


## Extensions = external executables (2026-08-29)
`suh <name> …` for a non-built-in `<name>` execs `suh-<name>` (git / cargo /
kubectl convention; clap `external_subcommand`). Chosen over in-tree
feature-gated modules (not extensible without rebuilding `suh`) and over a
declarative manifest DSL (too weak the moment an app needs a loop or a
VLM check). Extensions compose the actuators either by shelling out to
`$SUH_BIN` (any language) or by linking the `sudohand-*` crates (Rust —
`sudoprivacy/suh-wx` is the reference). This is also where cross-actuator
composition lives: a flow stays inside one actuator, an extension may
chain several. Search path: `$SUH_EXT_PATH`, `~/.suh/extensions`, the
binary's directory, `$PATH`.

The contract is enforced, not just documented: `sudohand-ext` is the
only sanctioned way to write a Rust extension (the `Extension` trait +
`Ctx`; parsing, `--manifest`, error envelopes and the platform gate come
from the crate, so extensions cannot drift), and `suh ext check` /
`sudohand_ext::check::assert_conformant` verify any executable — script
or binary — black-box. The manifest is derived from the clap tree, never
hand-written, so it is always what the binary accepts; agents build tool
definitions from it.

## Conventions to keep
- JSON on stdout; `{"error":{kind,message}}` on stderr (adc's exact shape;
  `kind` ∈ permission_denied/not_found/invalid_input/io/internal); exit 1 on failure.
- No policy/session/transport/confirmation in any crate.
- CI: `cargo build` + `cargo clippy --all-targets -- -D warnings` +
  `cargo test` + `cargo fmt --check`. Pin one toolchain across the
  workspace so local clippy matches CI (avoid the "local green, CI red"
  drift from mismatched clippy versions).
