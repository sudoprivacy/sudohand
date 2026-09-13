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

- Local macOS visible-window native input passed against Python's pyautogui
  path: the fixture ignores the first trusted click and all synthetic clicks,
  then accepts the actual native fallback. Both implementations reported method=os
  and exactly two trusted clicks. The fixture closes its browser and restores the cursor.
- Full source audit additionally found verified input filling in the reference
  (typed/verified/method/methods_tried with value readback and fallback). Existing
  Rust typing still needs this behavioral parity; accepting no-human-like alone
  is insufficient. BrowserPool/Job/Worker/persistence also remain to implement.
- Old macOS hosted run (Chrome for Testing 153.0.8010.36) failed page attachment
  in 24 tests with WebSocket listener stopped. Local installed Chrome passes;
  exact CfT build downloaded for reproduction. Cause is not yet established.

- Shared verified filling implemented for both input locators. Differential suite
  `scripts/typing_parity.py` passed all 12 scenarios through both Python/Rust CLIs:
  default timing, Unicode replacement, empty replacement, preferred keys/human,
  key-code-gated fallback, native setter fallback, total rejection, partial input,
  contenteditable, readonly setter, and Enter. Results and actual page values match.
  The reference CLI defaults type_by_text humanization to true even though its SDK
  uses the false-by-default config; the Rust CLI now preserves that distinction.
- CfT 153 macOS attachment failure reproduced locally by removing the outer
  application directory's .app suffix: WebSocket listener stopped after 90 seconds.
  The same executable in the intact .app passed all 26 real Chrome regression tests.
  CI now copies setup-chrome's relocated directory into a proper .app bundle.
- Second hosted Linux run passed cookie/identity/proxy/cleanup, then failed extension
  connect-result equality; diagnostics now print both results. Windows passed its
  real browser regression but its offline-cookie CLI overflowed the main stack;
  the synthetic suite now isolates the plaintext baseline and closes SQLite handles.
  Independent differential stages continue after sibling failures to reveal coverage.
- Actual CfT macOS extension handshake still fails locally (ordinary installed
  Chrome previously passed). Added isolated worker diagnostics; cause remains open.
- Workspace strict Clippy passed after verified filling. Hosted cross-platform checks,
  extension gaps, Windows offline runtime and SDK pool/profile/job/persistence remain.

- SDK foundations added: Job/JobResult/JobStatus, version-one PoolState, atomic
  replacement with file sync, recovery of pending/interrupted jobs, structured
  failure identity and retry budgets, and shared/per-worker/temp cookie paths.
  Four tests passed, including exact JSON roundtrip of a fixture produced by the
  pinned Python API, malformed-file handling, overwrite and profile isolation.
  This is not yet BrowserPool scheduling: dynamic workers, execution, queue policy,
  shared progress, wait/selection/cancellation and client lifecycle remain to implement.
- CfT extension diagnostics show its service worker exists but its WebSocket remains
  CONNECTING; a no-proxy-server trial did not resolve it. Root cause remains open.

- Run 34785206445 on commit 3b3274a: Linux job 103799308688 passed every stage,
  including actual extension, all typing cases, and native XTEST input under Xvfb.
  macOS basic browser regression now passes with the bundle-layout correction.
  Windows browser library regression passes, but multiple CLI differential stages
  fail; inspect job 103799308624 logs before assuming the fault is cookie-specific.
- Worker status/statistics snapshots now match a fixture emitted by Python's Worker
  API. Five SDK model/profile/persistence tests pass; scheduling still outstanding.
- Browser crate Windows cross-check passed with the new pool models. Full CLI
  cross-check on this macOS host needs a MinGW C compiler for existing ring; actual
  Windows CI remains the runtime gate. Local CfT extension handshake also remained
  pending after test-only browser network permission grants, ruling out that simple
  permission workaround. No user profile permissions were changed.

- Windows run 34785206445 failed before any cookie crypto: even plaintext offline
  extraction and browser_start/browser_connect overflowed the CLI main stack.
  Native debug assembly identified two independent large frames: ~946 KiB for
  the async command match and ~1.89 MiB for Clap's single browser enum parser.
  A local suh serve regression also reproduced stack overflow on its Tokio worker.
- Split the parser into flattened command groups and boxed each command's own
  future. CLI names/flags stay flat. The actual CLI error-envelope regression,
  both serve tests, all 59 reference --help calls, and a 1 MiB stack CLI probe pass.
  Whole-workspace tests and strict Clippy pass after the refactor; the CLI stack
  regression is now included in CI before real-browser differential tests.
- The post-dispatch cookie/identity/proxy/cleanup differential suite passed locally.
  macOS third CI passed browser, cookies and input suites but failed the CfT
  extension handshake, matching the local reproducer. That issue remains open.
