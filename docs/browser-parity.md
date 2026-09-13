# ai-dev-browser compatibility

Reference: `sudoprivacy/ai-dev-browser` commit
`94170d23f65f5f9140792d1de4a158c086a39325` (2026-09-13).
Starting sudohand revision: `c62b244a43d53bb87e1f14e97c5858ce41480745`.

**Status: implementation and verification in progress. This is not a claim of
complete parity.** Command declarations alone do not establish working behavior.

## Verified coverage

| Area | Evidence |
| --- | --- |
| CLI surface | All 59 reference operation names and 382 long-option declarations; 33 displayed defaults and 74 parsed non-null scalar/boolean defaults (`scripts/cli_parity.py`, CLI `parity_defaults` tests). Existing suh aliases remain available. |
| Browser startup and identity | Stealth defaults, explicit timezone/geolocation/locale, persistence across independent CLI calls and new tabs. Local proxy fixture verifies geo lookup travels through Chrome; no public geo service is required. |
| Cookies | Full live/offline arrays, long values, Unicode, HttpOnly, domain/session/expiry semantics; Windows synthetic DPAPI/AES-GCM decryption passed hosted CI. Legacy pickle protocols 2/4/5 import into real Chrome with equivalent fields. |
| Cookie filtering | Actual saved-file comparison for empty patterns, quoted dictionary fields, booleans, lookbehind and numbered/named backreferences. Files remain JSON. |
| Inventory and cleanup | Native process inventory, validated launch registry, dry-run, scoped orphan cleanup, idempotency and preservation of external Chrome. Stop-all is restricted to validated managed browsers. |
| Page and locators | URL/HTML/JS/console contracts, readiness/URL deadlines, HTML-id/XPath/text locators in top and same-origin frames, seven discovery option combinations. CSS/text element waits check visibility, deadlines and usable element refs. |
| Ref operations and artifacts | Focus, hover, HTML, Home key, selection and multi-file upload checked against DOM state. Screenshot dimensions, caps and coordinate metadata; PDF file structure/size; binary download file contents. |
| Input | Twelve verified-typing cases through two locators; trusted/synthetic/JS click fallback and explicit OS opt-out. Real native mouse passed on local macOS, hosted Linux/Xvfb and hosted Windows (run 34787941181). An intermittent Windows visible-browser launch failure remains under diagnosis. |
| Extension | Independently implemented extension and WebSocket bridge, actual Chrome handshake, target routing, separate CLI reuse, clicks/screenshots, popup adoption, preservation of unrelated tabs and bridge shutdown without killing Chrome. Hosted Linux, macOS and Windows extension tests have passed. |
| Rust SDK pool | Job/result/state schemas compared to reference-generated fixtures; 12 scheduler tests for retries, scaling, held jobs, shared progress, selection, cancellation, recovery and interrupted shutdown. Real two-Chrome test covers concurrent execution, per-worker cookies, restoration and cleanup. See [the pool guide](browser-pool.md). |
| CLI lifecycle | Flattened parser groups and boxed command futures avoid Windows/Tokio stack overflow. A real pipe-EOF regression requires Chrome to remain alive after the launching CLI exits; passed on Windows. |

The main Rust browser integration suite has 26 real Chrome tests. The complete
workspace passed locally after the regex/text-ref changes; the subsequent pool
shutdown change passed all 12 scheduler tests and strict workspace Clippy.

## Remaining acceptance work

- [ ] Resolve Windows download and visible-browser/native-input failures on the
  current branch; diagnose the intermittent reference proxy-start failure.
- [ ] Finish remaining operation/default/error/iframe/session comparisons,
  including row clicks, scrolling, download-link and extension restart/account
  lifecycle. Existing Rust tests cover portions of this surface but are not a
  substitute for the remaining differential checks.
- [x] Compare SDK scheduling against a running Python reference: eight scenarios
  cover both queue policies and retry budgets 0/1/2/unlimited, with exact call
  order, terminal results, error ancestry, business outcomes, statistics and
  close counts. CI regenerates the shared fixture; Rust executes the same cases.
- [ ] Verify final-head cross-platform CI, workspace format, strict Clippy and
  tests, then open the PR with actual results and compatibility notes.

Run [34787792557](https://github.com/sudoprivacy/sudohand/actions/runs/34787792557)
passed complete Linux and macOS jobs. Windows passed real browser/SDK, pipe EOF,
extension, click and typing stages but failed proxy/download/native stages.
Run [34787941181](https://github.com/sudoprivacy/sudohand/actions/runs/34787941181)
also passed complete Linux/macOS jobs; its Windows proxy and native mouse stages
passed, with only the download stage failing. The earlier proxy/visible-browser
failures need a race diagnosis. These are earlier-head
results, not a final acceptance result. Superseded workflow cancellations are
not counted as test failures or passes.

## Compatibility choices

- Keep suh's structured error envelope and nonzero failure exits. Recovery hints
  are independently worded; tests compare their presence/usefulness rather than
  copying the reference documentation verbatim.
- `page_goto` reports the live destination URL. The reference returns a stale
  pre-navigation target snapshot even when navigation succeeds.
- `storage_get/storage_set` use working Tab storage methods. The pinned reference
  CLI calls nonexistent `get_local_storage/set_local_storage` methods.
- `browser_connect` honors the transport environment variable; an explicit flag
  wins. The reference's standalone CLI currently forces its default CDP value.
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
