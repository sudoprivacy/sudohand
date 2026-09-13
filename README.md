# sudohand

**AI computer-control actuators** — the layer that turns an agent's
intent into real actions on a computer: **browser · desktop ·
filesystem · shell**, as one Rust workspace.

*sudohand*: the hands of the sudo agent stack — the counterpart to the
thinking layer. Sibling to [`apeiron`](https://github.com/joezhoujinjing/apeiron)
(the service/brain layer); sudohand is what actually touches the machine.
The CLI binary is `suh` (**s**uper **u**ser **h**and).

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
└─ sudohand-cli           # the `suh` binary: `sudohand <browser|desktop|fs|shell> …`
```

## CLI

```
suh browser ...     # was `adb`
suh desktop ...     # was `adc`
suh fs ...
suh shell ...       # off by default at the integrator
suh ext list        # installed extensions
suh <name> ...      # extension: execs `suh-<name>` (e.g. `suh wx send …`)
```

One binary with domain subcommands — self-describing, and no clash
with Android's `adb`.

## Status

`sudohand-desktop` is ported from `ai-desktop-control` (backend trait,
value types, `MacBackend`, `FakeBackend`) and wired up as
`suh desktop status|apps|screenshot|ax-tree|activate|click|type|paste-file|key`;
(+ `locate|workflows|flow` via the `agent` feature);
`sudohand-core` carries the shared `Error`, JSON-CLI contract, base64 and
permission probing. `sudohand-browser` exposes browser operations as
`suh browser <tool>`, with compatibility work against `ai-dev-browser` tracked
in [the parity checklist](docs/browser-parity.md), plus
`workflows|flow` via the `flow` feature (built-ins `form-signup`,
`page-extract`). `sudohand-fs` (`suh fs read|write|ls|stat|mkdir|rm|mv|cp|exists`)
and `sudohand-shell` (`suh shell run`) are thin over `std::fs` /
`std::process::Command`, each with a fake backend. See [DESIGN.md](DESIGN.md).

## Browser connection modes

Use `suh browser browser_start --headless` for a disposable CDP browser, then
pass its `--port` to browser commands. `browser_connect --port PORT` checks an
existing connection. Startup supports `--timezone`, `--geo`, `--locale`, and
proxy location matching; overrides persist across separate CLI calls.

To control a running Chrome profile through an extension, run:

```sh
suh browser browser_connect --transport extension
```

This starts the local Rust bridge and extracts the bundled extension. Follow the
returned `setup_instructions` to load it into the intended Chrome profile once.
Subsequent commands accept `--transport extension` (or set
`AI_DEV_BROWSER_TRANSPORT=extension`). The extension uses dedicated automation
tabs, follows popups they open, and reports the signed-in profile account when
available. `browser_disconnect` stops the bridge without closing Chrome.

Reference/text clicks report `verified` and `method` in addition to `clicked`.
They check for an observable page change before trying another browser click
method. `--os-click true` (or `AI_DEV_BROWSER_OS_CLICK=true`) enables a final
native mouse fallback; it requires a visible window and OS input permissions,
and moves the desktop cursor. `--os-click false` overrides an environment opt-in.

`browser_list` classifies managed, orphaned, and external Chrome processes.
Preview orphan cleanup with `browser_cleanup --scope profile --profile NAME
--dry-run`; remove `--dry-run` to apply it. `browser_stop --stop-all` only stops
registered browser instances. `cookies_extract_live` and
`cookies_extract_offline` return complete cookie values; `cookies_list` retains
its value previews.

## Extensions (`suh <name> …`)

App-specific operations are packaged as **extensions**, git-style: any
`<name>` that is not a built-in domain makes `suh` exec the executable
`suh-<name>` with the remaining arguments. Search order (`suh ext path`):
`$SUH_EXT_PATH`, `~/.suh/extensions/[<name>/]suh-<name>`, the directory
holding `suh`, then `$PATH`.

### The contract (any language)

| | |
|---|---|
| argv | `suh wx send --to A` → `suh-wx send --to A`; one level of verb subcommands |
| env | inherited, plus `SUH_BIN` (the calling `suh`) and `SUH_EXT_NAME`; `SUH_FAKE=1` = don't touch the machine |
| `--manifest` | print the manifest JSON (name, version, platforms, requires, commands + args), exit 0, no side effects |
| `--help` / `--version` | exit 0 |
| `workflows` / `flow <name> --var k=v` | list / run the registered workflows (Rust graphs over the fundamental actions) |
| success | one JSON object on stdout, exit 0 |
| failure | `{"error":{"kind","message"}}` on stderr, empty stdout, exit 1 — including bad arguments (`invalid_input`) |
| stdin | never read; never prompt |

`suh ext check <name>` verifies all of that black-box; `suh ext info <name>`
prints the manifest; `suh ext list` / `which` show what would run.

### Installing

```sh
suh ext install sudoprivacy/suh-wx            # GitHub owner/repo[@ref] (or any git URL)
suh ext install ./suh-wx                      # local cargo project → cargo build --release
suh ext install ./target/release/suh-wx       # an executable (script or binary)
suh ext update wx                             # re-fetch / rebuild from the recorded source
suh ext uninstall wx
```

Installs land in `~/.suh/extensions/<name>/suh-<name>` (git checkouts in
`~/.suh/src/<repo>`), named from the manifest, and only after passing
`suh ext check` (`--force` overrides). `install.json` beside the binary
records the source, ref and commit; `suh ext list` shows the version.
Needs `git` / `cargo` on `PATH` for those source kinds.

### Writing one in Rust: `sudohand-ext`

Implement the [`Extension`](crates/sudohand-ext/src/lib.rs) trait and the
crate does the rest (parsing, `--manifest`, error envelopes, platform
gate, real/fake backends):

```rust
use sudohand_ext::prelude::*;

#[derive(Subcommand)]
enum Cmd {
    /// Is WeChat running?
    Status,
    /// Send one message.
    Send { #[arg(long)] to: String, #[arg(long)] message: String },
}

struct Wx;
impl Extension for Wx {
    type Cmd = Cmd;
    const NAME: &'static str = "wx";
    const ABOUT: &'static str = "WeChat operations";
    const PLATFORMS: &'static [Platform] = &[Platform::MacOs];
    fn requires() -> Requires { Requires::new().apps(["com.tencent.xinWeChat"]) }
    fn readonly() -> &'static [&'static str] { &["status"] }
    fn workflows(r: &mut Registry) {
        // Rust graphs over Step::{Click,Type,Key,Wait,Verify} + VLM asks,
        // built with sudohand_desktop::flow::{StepTask, AskTask, GraphBuilder}.
        r.register_fn("wechat-send", "Send one message", &["contact", "message"], "wx", flows::send);
    }
    fn run(ctx: &Ctx, cmd: Cmd) -> Result<Value> {
        match cmd {
            Cmd::Status => { let d = ctx.desktop()?; /* also ctx.fs(), ctx.shell() */ … }
            Cmd::Send { to, message } =>
                ctx.flow::<Wx>("wechat-send", vars([("contact", to), ("message", message)]), None, None),
        }
    }
}
sudohand_ext::main!(Wx);
```

Registered workflows are listed by `suh wx workflows`, runnable as
`suh wx flow wechat-send --var contact=… --var message=…`, and appear in
the manifest — no wrapper command needed to try one.

`Ctx::fake(..)` + `sudohand_ext::test_run::<Wx>(&ctx, &["status"])` drive
it in-process on fake backends; `sudohand_ext::check::assert_conformant`
is the one-line conformance test. Extensions live in their own repos:
[`sudoprivacy/suh-wx`](https://github.com/sudoprivacy/suh-wx) (WeChat) is
the reference — copy it to start a new one. Checklist:

1. `cargo new --bin suh-<name>`; depend on `sudohand-ext`, `clap`,
   `serde_json` (plus the actuator crates whose types you name).
2. One `#[derive(Subcommand)] enum Cmd`, one `impl Extension`,
   `sudohand_ext::main!(...)`. Fill in `NAME` (= file suffix), `ABOUT`,
   `PLATFORMS`, `VERSION`, `requires()` (permissions / env / apps —
   declare, don't fail silently), `readonly()` (pure queries).
3. Multi-step operations are **workflows**: Rust graphs over the
   fundamental actions (`Step::Click/Type/Key/Wait/Verify`, VLM asks),
   registered in `workflows(&mut Registry)` with `source = Self::NAME`
   and run with `ctx.flow::<Self>(name, vars, ..)`. Typed commands are
   thin wrappers over them. A workflow is code, not config.
4. Get backends from `Ctx` only (`ctx.desktop()`, `ctx.fs()`,
   `ctx.shell()`, `ctx.runner(..)`) — that is what makes `SUH_FAKE=1`
   and the tests work.
5. Tests: unit tests with `Ctx::fake(..)` + `test_run`, and one
   conformance test calling
   `assert_conformant(env!("CARGO_BIN_EXE_suh-<name>"))`. CI never
   touches a real app.
6. Conventions: verbs as subcommands (`status`, `open`, `send`), one
   level, kebab-case; long flags; output says what happened, snake_case
   keys; no policy / prompts / sessions (the integrator wraps extensions
   like it wraps the actuators); platform limits in `PLATFORMS`, not
   `cfg` panics.

### Writing one as a script

```sh
#!/bin/sh
# ~/.suh/extensions/notes/suh-notes  →  `suh notes new "hello"`
case "$1" in
  --manifest) echo '{"schema":1,"name":"notes","description":"Apple Notes","version":"0.1.0",
                     "platforms":["macos"],"commands":[{"name":"new","mutates":true,
                     "args":[{"name":"text","kind":"positional","type":"string","required":true}]}]}';;
  --help) echo "suh notes new <text>";;
  new) "$SUH_BIN" desktop activate com.apple.Notes >/dev/null &&
       "$SUH_BIN" desktop key cmd+n >/dev/null && "$SUH_BIN" desktop type "$2";;
  *) echo '{"error":{"kind":"invalid_input","message":"unknown command"}}' >&2; exit 1;;
esac
```
