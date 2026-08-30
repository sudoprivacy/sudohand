# `suh serve` — the WebSocket bridge

`suh serve` turns this machine into an actuator any agent can drive over a
WebSocket. The bridge lives inside sudohand; there is no separate process.
Both callers connect the same way:

- a **remote** apeiron pod, and
- a **local** sudocode

`suh` still executes here (the machine with the apps); the socket only carries
the call in and the result out. `shell` and the meta commands (`ext`,
`describe`, `serve`, `mcp`) are refused — this is not a remote shell.

```sh
suh serve                                  # ws://127.0.0.1:8787, no auth
suh serve --bind 0.0.0.0:8787 --token XYZ  # reachable off-box, token-gated
```

Bind to loopback unless you have a reason not to; anything that can reach the
socket can drive the machine. Expose it beyond localhost only behind a tunnel
(e.g. tailscale) and always with `--token`.

## Framing

Newline is irrelevant — every WebSocket **text** frame is one JSON object.
The server speaks first; the client then sends one `call` per frame and gets
one `result` back, correlated by `id`.

### 1. (optional) auth

If the server was started with `--token`, the client's **first** frame must be:

```json
{ "type": "auth", "token": "XYZ" }
```

A wrong or missing token gets `{"type":"error","message":"unauthorized"}` and
the connection closes. With no `--token`, skip this step.

### 2. ready — server → client

Sent once, immediately (after auth). The capability catalog:

```json
{
  "type": "ready",
  "server": "suh",
  "version": "0.0.0",
  "methods": [
    { "method": "wx.context",    "description": "…", "readOnly": true  },
    { "method": "desktop.click", "description": "…", "readOnly": false },
    { "method": "fs.read",       "description": "…", "readOnly": true  }
  ]
}
```

`readOnly` grades consent: a `readOnly:false` method changes the machine and
should carry a stricter gate than a read. (Built-in actuators are listed;
installed extensions like `wx` aren't enumerated yet but are still callable.)

### 3. call — client → server

```json
{
  "type": "call",
  "id": "any-unique-string",
  "method": "desktop.click",
  "params": { "x": 448, "y": 725 },
  "args": []
}
```

- `method` is `"<domain>.<action>"` — exactly a `suh` subcommand
  (`desktop.click`, `wx.send`, `browser.page_goto`, `fs.read`, …).
- `params` are the **named flags**. Encoding by JSON type:
  - boolean `true` → the bare flag (`--verbose`); `false` → omitted;
  - array → the flag repeated once per element (`--bundle a --bundle b`);
  - anything else → `--flag <value>`.
- `args` (optional) are **positional** tokens, in order (e.g. the contact for
  `wx.messages`): `{"method":"wx.messages","args":["黄凯"],"params":{"n":5}}`.

### 4. result — server → client

```json
{ "type": "result", "id": "…", "ok": true,  "result": { … }, "exit": 0 }
{ "type": "result", "id": "…", "ok": false, "error": { "kind": "not_found", "message": "…" }, "exit": 4 }
```

- `ok` mirrors the process exit: `exit == 0`.
- On success `result` is the parsed JSON stdout; a TSV-emitting command comes
  back as `{ "text": "<tsv>" }`.
- On failure `error` is the CLI's own error envelope and `exit` is the
  classified code — the same contract as the CLI:
  `2` invalid_input · `4` not_found · `7` permission_denied · `9` io/transient ·
  `1` internal. Branch on it without parsing the message.

Calls on one connection are answered in order. Open more connections for
concurrency (they contend on the one machine, so serialize acting calls).

## Minimal client (pseudocode)

```
ws = connect("ws://host:8787")
if token: ws.send({type:"auth", token})
ready = ws.recv()                       # capability catalog
ws.send({type:"call", id:"1", method:"wx.context", params:{n:3}})
res = ws.recv()                         # {ok, result|error, exit}
```
