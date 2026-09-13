# ai-dev-browser compatibility

Reference: `sudoprivacy/ai-dev-browser` commit
`94170d23f65f5f9140792d1de4a158c086a39325` (2026-09-13).
Starting sudohand revision: `c62b244a43d53bb87e1f14e97c5858ce41480745`.

**Scope: browser capability parity against the pinned reference, with the
intentional compatibility choices below.** This does not promise identical
Python imports, every Python regex construct, or identical behavior on every
website. Command declarations alone do not establish working behavior.

## Verified coverage

| Area | Evidence |
| --- | --- |
| CLI surface | All 59 reference operation names and 382 long-option declarations; 33 displayed defaults and 74 parsed non-null scalar/boolean defaults (`scripts/cli_parity.py`, CLI `parity_defaults` tests). Existing suh aliases remain available. |
| Browser startup and identity | Stealth defaults, explicit timezone/geolocation/locale, persistence across independent CLI calls and new tabs. Local proxy fixture verifies geo lookup travels through Chrome; no public geo service is required. |
| Cookies | Full live/offline arrays, long values, Unicode, HttpOnly, domain/session/expiry semantics; Windows synthetic DPAPI/AES-GCM decryption passed hosted CI. Legacy pickle protocols 2/4/5 import into real Chrome with equivalent fields. |
| Cookie filtering | Actual saved-file comparison for empty patterns, quoted dictionary fields, booleans, lookbehind and numbered/named backreferences. Files remain JSON. |
| Inventory and cleanup | Native process inventory, validated launch registry, dry-run, scoped orphan cleanup, idempotency and preservation of external Chrome. Stop-all is restricted to validated managed browsers. |
| Page and locators | URL/HTML/JS/console contracts, readiness/URL deadlines, HTML-id/XPath/text locators in top and same-origin frames, cross-origin iframe evaluation, seven discovery option combinations. CSS/text element waits check visibility, deadlines and usable element refs. |
| Ref operations and artifacts | Focus, hover, HTML, Home key, selection and multi-file upload checked against DOM state. Row clicks, checkbox/double-click effects, container/element scrolling, storage/window/tab lifecycle. Screenshot dimensions, caps and coordinate metadata; PDF file structure/size; binary download and download-link file/event contents. |
| Input | Twelve verified-typing cases through two locators; trusted/synthetic/JS click fallback and explicit OS opt-out. Real native mouse passed on local macOS, hosted Linux/Xvfb and hosted Windows (run 34787941181). |
| Extension | Independently implemented extension and WebSocket bridge, actual Chrome handshake, target routing, separate CLI reuse, clicks/screenshots, popup adoption, preservation of unrelated tabs, bridge shutdown without killing Chrome and reconnection after bridge restart. Transport tests cover rejecting a second profile and accepting it after the owner disconnects. Hosted Linux, macOS and Windows extension tests have passed. |
| Rust SDK pool | Job/result/state schemas compared to reference-generated fixtures; 12 scheduler tests for retries, scaling, held jobs, shared progress, selection, cancellation, recovery and interrupted shutdown. Real two-Chrome test covers concurrent execution, per-worker cookies, restoration and cleanup. See [the pool guide](browser-pool.md). |
| CLI lifecycle | Flattened parser groups and boxed command futures avoid Windows/Tokio stack overflow. A real pipe-EOF regression requires Chrome to remain alive after the launching CLI exits; passed on Windows. |

The main Rust browser integration suite has 27 real Chrome tests. The complete
workspace, formatting and strict workspace Clippy passed locally at `69cf4d5`;
the browser crate also passed Windows cross-compilation. A local synthetic login
smoke test exercised both implementations through visible launch, persistent
cookie creation, browser close and headless export. This does not test a real
website's interactive authentication or MFA.

## Cross-platform acceptance

Last completed run:
[34789303637](https://github.com/sudoprivacy/sudohand/actions/runs/34789303637)
(`0ec1b96`): Linux and macOS passed completely. Windows passed every
CLI differential stage, including downloads, extension cleanup and native input;
one of 26 Rust browser integration tests failed while launching Chrome, before
exercising its page behavior. The browser never published DevTools within 60s.
The SDK tests later in that Rust command did not run after the failure.

Earlier runs passed complete Linux and macOS jobs and Windows browser/SDK,
pipe-EOF, cookie/proxy, extension, typing and native mouse behavior. Windows
runtime exposed a silent download failure with canonical verbatim paths; the
Windows runtime now passes after normalizing drive/UNC paths before Chrome. Separate
post-test Windows file-lock failures led to fixture cleanup that waits for its
owned Chrome descendants. The startup investigation separately reproduced an unread-stderr pipe deadlock:
a noisy Chrome wrapper failed before the fix and started successfully after it.
Startup now drains stderr continuously, retains only a 16 KiB diagnostic tail,
and includes it on timeout. A real subprocess writes 1 MiB through the pipe in
a regression test. This is not proof of the original Windows timeout's cause;
the new revision still requires cross-platform validation. A second reproduced
startup defect is also fixed: `--no-startup-window` supplied through Chrome
argument overrides now skips the initial-page requirement, just like the same
flag in raw extra arguments. Its real-browser regression verifies an empty
initial tab list and subsequent on-demand tab creation. Neither superseded
cancellations nor earlier-head passes count as final-candidate acceptance.

The running Python scheduler differential covers eight scenarios: both queue
policies and retry budgets 0/1/2/unlimited, with exact call order, terminal
results, error ancestry, business outcomes, statistics and close counts. CI
regenerates the fixture; Rust executes and compares the same cases.

## Compatibility choices

- Keep suh's structured error envelope and nonzero failure exits. Recovery hints
  are independently worded; tests compare their presence/usefulness rather than
  copying the reference documentation verbatim.
- `page_goto` reports the live destination URL. The reference returns a stale
  pre-navigation target snapshot even when navigation succeeds.
- `tab_close` closes the page target and reports the actual remaining count.
  The reference only disconnects its Tab WebSocket, leaving the page open;
  the test explicitly records that bug with a fresh subsequent tab listing.
- `storage_get/storage_set` use working Tab storage methods. The pinned reference
  CLI calls nonexistent `get_local_storage/set_local_storage` methods.
- `browser_connect` honors the transport environment variable; an explicit flag
  wins. The reference's standalone CLI currently forces its default CDP value.
- One extension profile owns a live bridge at a time. A second profile is
  rejected until that connection closes, preventing silent account replacement;
  the reference instead selects the most recently connected extension. Account
  status and subsequent reconnection are covered by the bridge transport test.
- The old `cookies_list` preview remains available; `cookies_extract_live` returns
  complete values. Legacy pickle import parses data without executing Python
  globals/constructors. New files use interoperable JSON.
- User regexes support lookaround and backreferences through fancy-regex, with a
  bounded backtracking budget; this is not a promise that every Python `re`
  construct has identical semantics. Cookie filters search dictionary-style text.
- Inventory identifies its native backend as `sysinfo`, rather than claiming
  Python `psutil` is present. Process cleanup validates profile ownership and
  protects live debug listeners, including temporarily unresponsive ones.
- Pool recovery deduplicates reference checkpoints containing the same active job
  twice. Graceful worker removal finishes its current task. These intentionally
  correct reference behavior rather than duplicating lost/duplicate work.
- The Rust SDK supplies equivalent capabilities through Rust traits and futures;
  it does not provide Python source/import compatibility.
- ai-dev-browser is AGPL-3.0; sudohand is MIT. The extension and new implementations
  are independent, not copied reference assets relabeled as MIT.

## Reproduction

Install Chrome and a Python environment with `websockets`, `websocket-client`,
`Pillow`, `deprecated`, `psutil`, `cryptography` and `rapidfuzz`. Check out the
pinned reference revision, then run from this repository:

```sh
cargo build -p sudohand-cli
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -- --test-threads=4
python scripts/test_parity_process.py
python scripts/stdio_parity.py --suh target/debug/suh
python scripts/cli_parity.py --suh target/debug/suh --reference /path/to/ai-dev-browser --check-defaults sudohand-cli/tests/fixtures/browser-cli-defaults.json
python scripts/pool_parity.py --reference /path/to/ai-dev-browser --fixture crates/sudohand-browser/tests/fixtures/pool-scheduler.json
python scripts/browser_parity.py --suh target/debug/suh --reference /path/to/ai-dev-browser
python scripts/page_parity.py --suh target/debug/suh --reference /path/to/ai-dev-browser
python scripts/click_parity.py --suh target/debug/suh --reference /path/to/ai-dev-browser
python scripts/typing_parity.py --suh target/debug/suh --reference /path/to/ai-dev-browser
python scripts/extension_parity.py --suh target/debug/suh --chrome /path/to/chrome --reference /path/to/ai-dev-browser
```

On Windows use `target/debug/suh.exe`. `AI_DEV_BROWSER_CHROME` selects Chrome for
other scripts. The native mouse test additionally needs `pyautogui` and a visible
desktop (Linux CI uses Xvfb); pass `--native` to `click_parity.py`. It restores the
cursor and closes its disposable browser. All cookie/profile fixtures are
synthetic and cleanup is scoped to processes owned by the tests.

The workflow preserves the macOS `.app` layout required by Chrome for Testing.
The extension fixture uses a mock keychain, as the regular launcher already does.
Bounded subprocess capture uses temporary files so inherited descendant streams
cannot defeat a timeout; the separate pipe-EOF test checks production behavior.

The latest local native mouse rerun could not exercise input because the Mac
was locked (confirmed via the session API). Earlier local macOS native input
passed. Native tests now check that precondition explicitly; they require an
unlocked, foreground fixture window. Hosted Linux and Windows exercise real
native input; hosted macOS exercises the CDP and extension paths.
