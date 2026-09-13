# ai-dev-browser parity work

Reference: ai-dev-browser default branch commit
`94170d23f65f5f9140792d1de4a158c086a39325` (2026-09-13).
Starting sudohand revision: `c62b244a43d53bb87e1f14e97c5858ce41480745`.

This is an implementation and verification checklist, not a claim of parity.
CLI discovery alone is not sufficient evidence. Preserve existing suh aliases
while adding the reference names, flags, defaults, and result contracts.

## Required evidence

- [ ] All 59 reference CLI operations present; compare signatures/defaults and outputs.
- [ ] Existing page, element, screenshot/PDF, mouse, tabs, storage, download and dialog behavior compared against local deterministic fixtures.
- [x] Live/offline cookie extraction returns complete values and the reference array schema; empty-domain filtering and session expiry match.
- [ ] Windows offline decryption implemented and tested using synthetic DPAPI/AES fixtures (no personal cookies).
- [ ] Browser startup stealth default, explicit timezone/geolocation/locale and proxy-egress auto-match; identity persists across CLI calls and new targets.
- [ ] CDP connect + extension bridge connect/disconnect, setup diagnostics, real-profile selection and popup/tab lifecycle.
- [ ] Managed-browser inventory, orphan-only cleanup, scope validation and dry-run; external Chrome is preserved.
- [ ] OS-input click fallback and configuration match, including clear unsupported/missing-backend errors.
- [ ] Python SDK public capabilities inventoried; corresponding Rust browser/pool/job/persistence behavior covered, without promising Python import compatibility.
- [ ] CLI errors, environment variables, coordinate scaling, iframe refs and session lifecycle compared.
- [ ] Real Chrome end-to-end and differential tests pass; extension exercised in Chrome, not just a mock bridge.
- [ ] Workspace fmt, strict Clippy, tests and relevant cross-platform CI pass.
- [ ] PR opened with actual evidence and any compatibility notes; all requirements above verified before claiming completion.

## Evidence log

- Initial audit: 54 identical command names; missing reference names include
  browser_connect, browser_disconnect, browser_cleanup, cookies_extract_live,
  cookies_extract_offline. Existing cookies_list truncates values at 50 characters
  and wraps them in an object, so it is not an alias for cookies_extract_live.
- Local baseline probe used a synthetic 80-character cookie on `.example.test`:
  ai-dev-browser returned 80 characters; suh cookies_list returned 50 + ellipsis.

- Cookie differential suite passed locally against the pinned reference: full values, Unicode offline values, session/persistent expiry, HttpOnly, empty/missing/domain filters and legacy preview preservation. Windows crypto implementation is pending Windows validation.

- Real Chrome regression after stealth/registry changes: 26 integration tests passed.
- Identity differential passed for independent Rust/Python CLI calls and a new tab.
  A local fake forward proxy served a deliberately unresolvable geo hostname:
  both implementations queried through Chrome, parsed ipinfo-style location,
  persisted Europe/Paris, and left locale unchanged. No public geo service used.
- macOS strict Clippy passed for browser + CLI; Windows target check passed for
  identity and cookie changes. These cross-compiles do not validate Windows runtime behavior.

## Intentional compatibility details under review

- Inventory uses native Rust `sysinfo`, with `process_inventory: "sysinfo"`
  instead of claiming that Python's optional `psutil` is installed. Browser rows
  and `by_origin` follow the reference full-inventory schema; temporary profiles
  have a null workspace slug and external rows can have a null debugging port.
- Cleanup protects any process with a listening debugging port, even if its CDP
  endpoint is temporarily unresponsive. Managed profile checks use path components,
  and temporary ownership also requires the OS temporary directory. This avoids
  prefix collisions and preserves external browser processes.

## Reproduce the differential suite

Use a Python environment with the pinned reference's dependencies (including
`psutil`; on Windows also `cryptography`) and an installed Chrome. Then:

```sh
cargo build -p sudohand-cli
python scripts/browser_parity.py --suh target/debug/suh --reference /path/to/ai-dev-browser
```

The script isolates HOME/USERPROFILE, creates only synthetic cookie databases,
uses a local proxy fixture, and scopes real orphan cleanup to its own named profile.

- Scoped cleanup differential passed locally: synthetic orphan and external Chrome
  inventory, Python/Rust dry-run agreement, actual named-profile orphan termination,
  external process survival, and idempotent second cleanup. No unrelated processes killed.
- All 26 real Chrome integration tests passed again after inventory/cleanup changes.
  Strict Windows-target Clippy also passed with sysinfo; Windows runtime remains untested.
- Remaining major implementation work: CDP/extension connect/disconnect, extension
  transport and real Chrome extension tests, native OS click fallback, complete command
  contract comparison, SDK pool/job/persistence inventory, and actual cross-platform CI.
- Reference extension source is AGPL-3.0 while sudohand declares MIT. Do not copy
  reference extension assets into this repository as MIT; implement protocol behavior
  independently and retain accurate attribution for any separately licensed material.

- Rust bridge synthetic transport test passed: colliding request IDs across
  drivers, target routing, events, pending-command failure on disconnect,
  shutdown/idempotency, and rejection of website WebSocket origins.
- Real Chrome extension tests passed using a disposable profile and the assets
  extracted from the built binary: bridge handshake, Python/Rust connect and JS
  result agreement, independent calls reuse a dedicated tab, actual click state
  change, PNG screenshot, popup adoption and URL routing, untouched pre-existing
  tab, and bridge shutdown while Chrome stays alive. Input dispatch activates its
  owned tab to avoid inactive-tab mouse acknowledgement timeouts.
- Blanket-stop test passed with a separately launched unregistered debugging
  Chrome in the discovery band: only the registered fixture was stopped.
- Three-platform GitHub Actions parity workflow added, pinned to the reference
  revision. Actual hosted Windows/Linux/macOS results are still pending.

- Whole-workspace strict Clippy passed. Whole-workspace tests passed with
  `--test-threads=4`. A preceding unrestricted run had one Chrome launch timeout
  before its test reached tab operations (25 other browser tests passed); the
  exact startup cause is unproven. CI bounds browser-test concurrency to four.

- CLI declaration audit: all 59 reference command names are present. Remaining
  declared flag gaps were `click_by_ref/text --os-click` and
  `type_by_text --no-human-like`; these are now implemented. Declaration coverage
  does not yet prove all defaults and output contracts.
- Click differential passed for trusted, synthetic, JS-click, unchanged-page,
  by-ref and explicit OS opt-out cases; typing with no-human-like matched actual
  input contents. The 26 Chrome regression tests passed with the verified-click
  implementation. The coordinate fixture now exposes its counter through the title
  so successful clicks have observable feedback rather than triggering fallback.
- Native mouse dispatch implemented with Enigo (macOS/Windows) and x11rb/XTEST (Linux); Windows target
  compilation passed. Real native input comparison is pending Linux Xvfb CI.
- First Linux hosted run passed 48 unit, bridge, 26 real-browser and cookie/identity/
  proxy tests, then found that sysinfo included Chrome threads as cleanup candidates.
  Disabled task enumeration and sorted process IDs to match main-process inventory.

- Windows hosted run passed 25 real-browser tests but observed an extra blank
  tab in tabs_list_and_switch. Startup previously allowed the first page list to
  be empty; strengthened readiness to require its initial page (except explicit
  --no-startup-window). Kept exact tab-count assertions. All 26 tests then passed
  locally with the new readiness gate.
- Native input uses system APIs on macOS/Windows and pure-Rust X11/XTEST on Linux;
  Linux and Windows cross-target checks passed. CI now includes actual native
  mouse comparisons on Xvfb and Windows. JSON subprocess decoding is explicit
  UTF-8 so Unicode fixtures do not depend on Windows' locale code page.
