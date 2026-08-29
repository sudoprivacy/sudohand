---
name: suh
description: Drive the local computer — Chrome, macOS desktop apps (incl. WeChat), the filesystem, and shell — through the `suh` CLI. Use whenever a task means operating an app or the machine rather than editing code: read or send WeChat messages, click through a web page, screenshot or automate a desktop app, run a file/shell op. `suh` is the "hands"; you are the brain that plans and calls it.
---

# suh — computer-control CLI for an agent

`suh` is a stateless actuator CLI. You plan; `suh` executes one action per
call. Everything is line-oriented and non-interactive — call it from the
shell like `git`.

## Orientation (do this first)

- `suh describe` — the whole command tree as TSV + the exit-code contract.
  Grep it instead of guessing: `suh describe | grep wx`.
- `suh <domain> --help` / `suh <domain> <action> --help` — flags for one action.
- Domains: `browser` (Chrome/CDP), `desktop` (macOS apps: windows, clicks,
  screenshots, VLM locate/ask), `fs`, `shell`. Extensions: `suh <name> …`
  runs a `suh-<name>` binary (e.g. `suh wx …` for WeChat); `suh ext list`.

## Output & exit codes (how to react)

- Success → JSON on stdout (exit 0). Extension **list** commands default to
  **TSV** (`contacts`, `sessions`, `context`, …); add `--format json` when a
  program/workflow must parse the structure.
- Failure → `{"error":{"kind","message"}}` on stderr, and the exit code tells
  you what to do without parsing it:
  `2`=bad args (fix them) · `4`=not found (target absent) ·
  `7`=permission (grant Accessibility/Screen-Recording, or sudo) ·
  `9`=io/transient (retry) · `1`=internal (a bug — don't retry).

## Two levels: primitives vs workflows

- **Primitives** — atomic actions you sequence yourself when no workflow fits:
  `suh desktop locate --bundle B --find "..."` (VLM → a clickable point),
  `suh desktop click --x .. --y ..`, `suh desktop ask --question "..."`
  (VLM yes/no), `suh browser page_goto/locate/ask`, `suh fs read`, …
  Cheap deterministic checks: `suh desktop find-window --title X`,
  `suh desktop ax-find --role AXButton --text "OK"` (no VLM — use these over
  `ask` when the app exposes the state).
- **Workflows** — pre-built, self-healing multi-step flows (VLM + branch +
  retry). `suh <ext> workflows` lists them, `suh <ext> flow <name> --var k=v`
  runs one. Prefer these over hand-sequencing primitives: fewer calls, more
  reliable, and they fail safe.

## 5 most-used (WeChat extension, real output)

```
$ suh wx context -n 3            # current WeChat snapshot for planning
sticky	name	last_seen	unread	type	summary	username
yes	Kai	3 分钟前	0	text	hi Kai — …	wxid_geuqap9ud37z22
	黄凯	36 分钟前	0	text	黄凯 北京 …	wxid_ie6plxw8186t22

$ suh wx contacts --search 黄凯   # resolve a name → unique wx-id (微信号)
name	微信号	wx_id	last_seen	messages
黄凯	kaihuang1987	wxid_ie6plxw8186t22	36 分钟前

$ suh wx messages 黄凯 -n 5       # read a conversation (TSV)
$ suh wx send --to 黄凯 --message 收到    # send (workflow verifies recipient, fails safe)
$ suh wx logout / login / restart        # react workflows over desktop+shell
```

Other domains: `suh desktop status`, `suh desktop screenshot --bundle B --out f.png`,
`suh browser browser_start && suh browser page_goto --url …`, `suh fs read --path …`.

## Error recovery (3 common)

1. `no wx config; run 'suh wx capture'` (exit 4) → the WeChat DB key isn't
   captured on this machine. Run `suh wx capture` (interactive, human-driven).
2. `attach … failed … permission` (exit 7) → grant Accessibility +
   Screen-Recording to the terminal in System Settings, or run with sudo.
3. `no DASHSCOPE_API_KEY` on a `locate`/`ask` (exit 2) → the VLM needs a key:
   set `DASHSCOPE_API_KEY` or `bl auth login`. Deterministic checks
   (`find-window`, `ax-find`) need no key.

## Rules

- Never assume state across turns — start with `suh wx context` /
  `suh desktop status` / `suh browser page_info` to see where things are.
- Prefer a workflow or an extension command over raw clicks; drop to
  primitives only when nothing fits.
- Ambiguity fails safe: `suh wx send --to Kai` may error "matches several
  contacts" — resolve via `suh wx contacts --search Kai` and send by the
  unique 微信号/wx-id.
