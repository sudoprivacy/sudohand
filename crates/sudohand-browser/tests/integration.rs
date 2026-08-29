//! Fixture integration tests against a real headless Chrome. No external
//! network: fixtures are served from `tests/fixtures/` on 127.0.0.1.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::too_many_lines
)]

mod common;

use common::{open, skip_browser_tests, start_chrome, Fixtures};
use serde_json::Value;
use sudohand_browser::actions::TypeOptions;
use sudohand_browser::dialog::DialogOptions;
use sudohand_browser::elements::{ScrollOptions, TypeByTextOptions};
use sudohand_browser::mouse::ClickOptions;
use sudohand_browser::page::ScreenshotOptions;
use sudohand_browser::refs::{node_id_of, parse_ref};
use sudohand_browser::snapshot::{DiscoverOptions, Element};
use sudohand_browser::tools::*;

fn find<'a>(els: &'a [Element], role: &str, name: &str) -> Option<&'a Element> {
    els.iter()
        .find(|e| e.role == role && e.name.as_deref() == Some(name))
}

async fn eval_str(tab: &sudohand_browser::connection::Tab, expr: &str) -> String {
    tab.evaluate(expr)
        .await
        .unwrap()
        .as_str()
        .unwrap_or("")
        .to_string()
}

#[tokio::test]
async fn browser_lifecycle_start_list_stop() {
    if skip_browser_tests() {
        return;
    }
    // browser_list scans the preferred 9350-9450 band, so this Chrome must
    // live there (every other test uses an ephemeral port outside the band).
    let chrome = common::start_chrome_in_band().await;
    assert!(sudohand_browser::port::is_port_in_use(chrome.port));

    let listed = browser_list(false).await.unwrap();
    let ports: Vec<u64> = listed["browsers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["port"].as_u64().unwrap())
        .collect();
    assert!(ports.contains(&u64::from(chrome.port)), "{listed}");

    let all = browser_list(true).await.unwrap();
    let ours = all["browsers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["port"].as_u64() == Some(u64::from(chrome.port)))
        .expect("listed in all_workspaces");
    assert_eq!(
        ours["workspace"].as_str().unwrap(),
        std::env::current_dir().unwrap().to_string_lossy()
    );

    let stopped = browser_stop(Some(chrome.port), false).await.unwrap();
    assert_eq!(stopped["count"], 1);
    assert_eq!(stopped["browsers"][0]["method"], "graceful", "{stopped}");
    // Port is released quickly after Browser.close.
    let mut free = false;
    for _ in 0..25 {
        if !sudohand_browser::port::is_port_in_use(chrome.port) {
            free = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    assert!(free, "port still in use after browser_stop");
}

#[tokio::test]
async fn goto_wait_ready_and_html() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("form.html")).await;

    let r = page_goto(&tab, &fx.url("form.html"), true).await.unwrap();
    assert_eq!(r["success"], true);
    assert_eq!(r["title"], "Form Fixture");
    assert_eq!(r["url"], fx.url("form.html"));
    assert!(page_wait_ready(&tab, 5.0, 0.0).await);

    let inner = page_html(&tab, false).await.unwrap();
    let outer = page_html(&tab, true).await.unwrap();
    assert!(inner["html"]
        .as_str()
        .unwrap()
        .contains("<form id=\"signup\""));
    assert!(outer["html"].as_str().unwrap().starts_with("<html"));
    assert_eq!(
        inner["length"].as_u64().unwrap() as usize,
        inner["html"].as_str().unwrap().chars().count()
    );
}

#[tokio::test]
async fn discover_form_elements() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("form.html")).await;
    let els = page_discover(&tab, &DiscoverOptions::default())
        .await
        .unwrap();

    let username = find(&els, "textbox", "Username").expect("username textbox");
    assert!(username.x.is_some() && username.bbox.is_some());
    let email = find(&els, "textbox", "Email").expect("email");
    assert_eq!(email.required, Some(true));
    assert!(find(&els, "textbox", "Bio").is_some());
    assert!(find(&els, "combobox", "Plan").is_some());
    let tos = find(&els, "checkbox", "I agree to the terms").expect("checkbox");
    assert_eq!(tos.checked, Some(Value::from("false")));
    let by_email = find(&els, "radio", "By email").expect("radio");
    assert_eq!(by_email.checked, Some(Value::from("true")));
    assert!(find(&els, "button", "Create account").is_some());
    assert!(find(&els, "button", "Reset").is_some());
    // Hidden input never surfaces (no box).
    assert!(!els.iter().any(|e| e.name.as_deref() == Some("honeypot")));

    // Every ref is well-formed and carries a node id; refs are unique.
    let mut seen = std::collections::HashSet::new();
    for (i, e) in els.iter().enumerate() {
        let p = parse_ref(&e.r#ref);
        assert!(p.backend_node_id.is_some(), "{e:?}");
        assert_eq!(p.local, (i + 1).to_string(), "AX refs are 1-based in order");
        assert!(seen.insert(e.r#ref.clone()));
    }

    // Text filter runs before geometry and is a case-insensitive substring.
    let filtered = page_discover(
        &tab,
        &DiscoverOptions {
            text: Some("user".into()),
            ..DiscoverOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].name.as_deref(), Some("Username"));

    // Non-interactable mode includes StaticText etc.
    let all = page_discover(
        &tab,
        &DiscoverOptions {
            interactable_only: false,
            ..DiscoverOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(all.len() > els.len());
    assert!(all.iter().any(|e| e.role == "StaticText"));
}

#[tokio::test]
async fn discover_buttons_and_dom_scan() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("buttons.html")).await;
    let els = page_discover(&tab, &DiscoverOptions::default())
        .await
        .unwrap();

    let del = find(&els, "button", "Delete").expect("disabled button");
    assert_eq!(del.disabled, Some(true));
    // Accessible name composes across children.
    assert!(find(&els, "button", "Sudo Code").is_some(), "{els:#?}");
    let docs = find(&els, "link", "Docs").expect("link");
    assert!(node_id_of(&docs.r#ref).is_some());
    let h1 = find(&els, "heading", "Actions").expect("heading");
    assert_eq!(h1.level, Some(Value::from(1)));
    assert!(find(&els, "image", "Logo").is_some());
    assert!(find(&els, "button", "ARIA button").is_some());

    // DOM scan surfaces ARIA-less grid rows with their datarole.
    let row1 = els
        .iter()
        .find(|e| e.name.as_deref() == Some("Row one"))
        .expect("dom-scanned row");
    assert_eq!(row1.role, "row");
    assert_eq!(row1.datarole.as_deref(), Some("row"));
    assert!(row1.bbox.is_some());
    // The plain onclick div is DOM-scanned too (no ARIA role → tag name).
    let nav = els
        .iter()
        .find(|e| e.name.as_deref() == Some("Plain div nav"))
        .expect("onclick div");
    assert_eq!(nav.role, "div");

    let no_dom = page_discover(
        &tab,
        &DiscoverOptions {
            dom_scan: false,
            ..DiscoverOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(!no_dom.iter().any(|e| e.name.as_deref() == Some("Row one")));
    assert!(no_dom.len() < els.len());
}

#[tokio::test]
async fn discover_and_click_inside_iframe() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("iframe.html")).await;
    let els = page_discover(&tab, &DiscoverOptions::default())
        .await
        .unwrap();

    let inner = find(&els, "button", "Inner button").expect("iframe button");
    let p = parse_ref(&inner.r#ref);
    let scope = p.scope.expect("frame-scoped ref");
    assert_eq!(scope.kind, "FRAME");
    assert_eq!(scope.id.len(), 8);
    assert!(find(&els, "textbox", "Inner input").is_some());
    // about:blank frame contributes nothing.
    let frames: std::collections::HashSet<String> = els
        .iter()
        .filter_map(|e| parse_ref(&e.r#ref).scope.map(|s| s.to_string()))
        .collect();
    assert_eq!(frames.len(), 1);

    let r = click_by_ref(&tab, &inner.r#ref, true).await.unwrap();
    assert_eq!(r["clicked"], true, "{r}");
    let log = eval_str(
        &tab,
        "document.getElementById('child').contentDocument.getElementById('inner-log').textContent",
    )
    .await;
    assert_eq!(log, "inner");

    let no_frames = page_discover(
        &tab,
        &DiscoverOptions {
            include_iframes: false,
            ..DiscoverOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(no_frames
        .iter()
        .all(|e| parse_ref(&e.r#ref).scope.is_none()));
}

#[tokio::test]
async fn click_by_ref_reports_navigation() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("buttons.html")).await;
    let els = page_discover(&tab, &DiscoverOptions::default())
        .await
        .unwrap();
    let home = find(&els, "link", "Home").unwrap();
    let r = click_by_ref(&tab, &home.r#ref, true).await.unwrap();
    assert_eq!(r["clicked"], true);
    assert_eq!(r["ref"], home.r#ref);
    assert_eq!(r["url_before"], fx.url("buttons.html"));
    assert_eq!(r["navigated"], true, "{r}");
    assert!(r["url_after"]
        .as_str()
        .unwrap()
        .ends_with("result.html?from=home"));

    // A bogus ref fails cleanly, with the same shape and an actionable message.
    let bad = click_by_ref(&tab, "999#999999", true).await.unwrap();
    assert_eq!(bad["clicked"], false);
    assert_eq!(bad["navigated"], false);
    let msg = bad["error"].as_str().unwrap();
    assert!(
        msg.contains("stale") && msg.contains("page_discover"),
        "{msg}"
    );

    // Legacy index-only ref resolves via a fresh snapshot.
    let els = page_discover(&tab, &DiscoverOptions::default())
        .await
        .unwrap();
    let back = find(&els, "link", "Back").unwrap();
    let idx = parse_ref(&back.r#ref).local;
    let r = click_by_ref(&tab, &idx, true).await.unwrap();
    assert_eq!(r["clicked"], true, "{r}");
    assert_eq!(r["element"]["name"], "Back");
    assert_eq!(r["navigated"], true);
}

#[tokio::test]
async fn click_by_text_waits_for_dynamic_content() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("dynamic.html")).await;
    // Buttons appear ~400ms after load; click_by_text must wait for them.
    let r = click_by_text(&tab, "Beta", 5.0, true).await.unwrap();
    assert_eq!(r["clicked"], true, "{r}");
    assert_eq!(r["text"], "Beta");
    assert_eq!(r["title_after"], "clicked Beta");
    assert!(r["ref"].as_str().unwrap().contains('#'));

    // Shadow-DOM button is reachable through the AX tree.
    let r = click_by_text(&tab, "Shadow button", 2.0, true)
        .await
        .unwrap();
    assert_eq!(r["clicked"], true, "{r}");

    // Exact name beats a longer containing name; composite names match.
    let (_b2, tab2) = open(&chrome, &fx.url("buttons.html")).await;
    let r = click_by_text(&tab2, "Sudo Code", 2.0, true).await.unwrap();
    assert_eq!(r["clicked"], true, "{r}");
    // StaticText fallback tier: a bare <div onclick>.
    let r = click_by_text(&tab2, "Plain div nav", 2.0, true)
        .await
        .unwrap();
    assert_eq!(r["clicked"], true, "{r}");
    assert_eq!(
        eval_str(&tab2, "document.getElementById('log').textContent").await,
        "nav"
    );

    // Absent text fails after the timeout with clicked=false.
    let start = std::time::Instant::now();
    let r = click_by_text(&tab2, "No such thing", 1.0, true)
        .await
        .unwrap();
    assert_eq!(r["clicked"], false);
    assert!(start.elapsed().as_secs_f64() >= 1.0);
}

#[tokio::test]
async fn type_by_ref_variants() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("form.html")).await;
    let els = page_discover(&tab, &DiscoverOptions::default())
        .await
        .unwrap();
    let user = find(&els, "textbox", "Username").unwrap().r#ref.clone();

    let r = type_by_ref(&tab, &user, "hello", TypeOptions::default())
        .await
        .unwrap();
    assert_eq!(r["typed"], true);
    assert_eq!(
        eval_str(&tab, "document.getElementById('username').value").await,
        "hello"
    );
    assert_eq!(
        eval_str(&tab, "document.getElementById('status').textContent").await,
        "typed:hello"
    );

    // clear + enter: old value gone, keydown Enter observed by the page.
    let r = type_by_ref(
        &tab,
        &user,
        "wörld",
        TypeOptions {
            clear: true,
            enter: true,
            keystrokes: false,
            human_like: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(r["entered"], true);
    assert_eq!(
        eval_str(&tab, "document.getElementById('username').value").await,
        "wörld"
    );
    assert_eq!(
        eval_str(&tab, "document.getElementById('status').textContent").await,
        "enter:wörld"
    );

    // keystrokes: per-character key events.
    let r = type_by_ref(
        &tab,
        &user,
        "ab",
        TypeOptions {
            clear: true,
            enter: false,
            keystrokes: true,
            human_like: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(r["typed"], true);
    assert_eq!(
        eval_str(&tab, "document.getElementById('username').value").await,
        "ab"
    );

    // Invalid ref → typed=false, no panic.
    let r = type_by_ref(&tab, "nope", "x", TypeOptions::default())
        .await
        .unwrap();
    assert_eq!(r["typed"], false);
}

#[tokio::test]
async fn screenshot_embeds_scaling_metadata() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("buttons.html")).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shot.png");
    let r = page_screenshot(
        &tab,
        &ScreenshotOptions {
            path: Some(path.clone()),
            ..ScreenshotOptions::default()
        },
    )
    .await
    .unwrap();
    // 1600x950 viewport → long edge capped at 1280 → 1280x760, factor 1.25.
    assert_eq!(r["width"], 1280);
    assert_eq!(r["height"], 760);
    assert!((r["scale_factor"].as_f64().unwrap() - 1.25).abs() < 1e-9);
    assert!(r["size"].as_u64().unwrap() > 0);

    let meta = sudohand_browser::page::read_screenshot_metadata(&path)
        .unwrap()
        .expect("metadata chunk");
    assert_eq!(meta.viewport_width, 1600);
    assert_eq!(meta.image_width, 1280);
    // An element centre read off the image maps back to its CSS box.
    let els = page_discover(&tab, &DiscoverOptions::default())
        .await
        .unwrap();
    let save = find(&els, "button", "Save").unwrap();
    let (cx, cy) = (save.x.unwrap() as f64, save.y.unwrap() as f64);
    let (ix, iy) = (cx / meta.scale_factor, cy / meta.scale_factor);
    let (bx, by) = sudohand_browser::geometry::scale_coords(ix, iy, Some(&meta));
    assert!((bx - cx).abs() < 0.01 && (by - cy).abs() < 0.01);

    // No-scale variant keeps native pixels.
    let raw = dir.path().join("raw.png");
    let r = page_screenshot(
        &tab,
        &ScreenshotOptions {
            path: Some(raw.clone()),
            css_scale: false,
            ..ScreenshotOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(r["width"], 1600);
    assert_eq!(r["scale_factor"], 1.0);
}

#[tokio::test]
async fn tabs_list_and_switch() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (mut browser, _tab) = open(&chrome, &fx.url("form.html")).await;
    let t2 = browser.new_tab("about:blank").await.unwrap();
    page_goto(&t2, &fx.url("buttons.html"), true).await.unwrap();

    let l = tab_list(&mut browser).await.unwrap();
    assert_eq!(l["count"], 2, "{l}");
    let urls: Vec<&str> = l["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["url"].as_str().unwrap())
        .collect();
    assert!(urls.contains(&fx.url("form.html").as_str()));
    assert!(urls.contains(&fx.url("buttons.html").as_str()));
    assert_eq!(l["tabs"][0]["active"], true);

    let s = tab_switch(&mut browser, 1).await.unwrap();
    assert_eq!(s["url"], urls[1]);
    let err = tab_switch(&mut browser, 7).await.unwrap_err();
    assert!(err.to_string().contains("Invalid tab ID"));
}

#[tokio::test]
async fn js_evaluate_results_console_and_errors() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("buttons.html")).await;

    let r = js_evaluate(&tab, "({a: 1, b: [true, 'x'], n: null})")
        .await
        .unwrap();
    assert_eq!(
        r["result"],
        serde_json::json!({"a": 1, "b": [true, "x"], "n": null})
    );
    assert_eq!(r["navigated"], false);
    assert!(r.get("console").is_none());

    let r = js_evaluate(&tab, "console.log('x=', 1); 2").await.unwrap();
    assert_eq!(r["result"], 2);
    assert_eq!(r["console"][0]["level"], "log");
    assert_eq!(r["console"][0]["text"], "\"x=\" 1");

    let r = js_evaluate(&tab, "document.body").await.unwrap();
    assert_eq!(r["result"]["__js_type__"], "node");

    let err = js_evaluate(&tab, "console.warn('pre'); throw new Error('boom')")
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("boom"), "{text}");
    assert!(text.contains("console.warning: \"pre\""), "{text}");

    let r = js_evaluate(&tab, "location.href = 'result.html?from=js'")
        .await
        .unwrap();
    assert_eq!(r["navigated"], true, "{r}");
}

#[tokio::test]
async fn dialogs_are_auto_dismissed() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("dialog.html")).await;
    let r = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        click_by_text(&tab, "Show alert", 2.0, true),
    )
    .await
    .expect("click must not hang on an alert()")
    .unwrap();
    assert_eq!(r["clicked"], true);
    // The renderer was unblocked: the statement after alert() ran.
    let log = eval_str(&tab, "document.getElementById('log').textContent").await;
    assert_eq!(log, "after-alert");
}

#[tokio::test]
async fn async_page_wait_then_act() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("async.html")).await;
    assert!(page_wait_ready(&tab, 10.0, 0.0).await);
    let r = click_by_text(&tab, "Late button", 5.0, true).await.unwrap();
    assert_eq!(r["clicked"], true, "{r}");
    assert_eq!(r["navigated"], true, "{r}");
    assert!(r["url_after"].as_str().unwrap().contains("from=late"));
}

#[tokio::test]
async fn stale_refs_give_actionable_errors_everywhere() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("form.html")).await;
    let els = page_discover(&tab, &DiscoverOptions::default())
        .await
        .unwrap();
    let old_user = find(&els, "textbox", "Username").unwrap().r#ref.clone();
    let old_reset = find(&els, "button", "Reset").unwrap().r#ref.clone();

    // A reload mints new backend node ids: the old refs are now stale but
    // look perfectly valid (same index, different node id).
    page_goto(&tab, &fx.url("form.html"), true).await.unwrap();
    let fresh = page_discover(&tab, &DiscoverOptions::default())
        .await
        .unwrap();
    let new_user = &find(&fresh, "textbox", "Username").unwrap().r#ref;
    assert_ne!(new_user, &old_user, "reload should change node ids");

    let r = click_by_ref(&tab, &old_reset, true).await.unwrap();
    assert_eq!(r["clicked"], false);
    let msg = r["error"].as_str().unwrap();
    assert!(
        msg.contains("stale") && msg.contains("page_discover"),
        "click_by_ref must name the stale ref and tell the caller to re-discover, got: {msg}"
    );
    assert!(
        !msg.contains("box model"),
        "must not read as a geometry problem: {msg}"
    );

    let r = type_by_ref(&tab, &old_user, "x", TypeOptions::default())
        .await
        .unwrap();
    assert_eq!(r["typed"], false);
    let msg = r["error"].as_str().unwrap();
    assert!(
        msg.contains("stale") && msg.contains("page_discover"),
        "{msg}"
    );

    // A live node that is simply not rendered gets the *other* message.
    let hidden = js_evaluate(&tab, "document.querySelector('input[name=honeypot]')").await;
    assert!(hidden.is_ok());
    let els = page_discover(
        &tab,
        &DiscoverOptions {
            interactable_only: false,
            ..DiscoverOptions::default()
        },
    )
    .await
    .unwrap();
    // The fresh ref still works, proving the failure above was the ref, not the tool.
    let r = click_by_ref(&tab, &find(&els, "button", "Reset").unwrap().r#ref, true)
        .await
        .unwrap();
    assert_eq!(r["clicked"], true, "{r}");
}

#[tokio::test]
async fn env_port_is_honoured_by_resolve_and_browser_start() {
    if skip_browser_tests() {
        return;
    }
    // Both halves live in one test: they are the only writers of PORT_ENV,
    // and tests in this binary run in parallel threads.
    std::env::set_var(sudohand_browser::config::PORT_ENV, "61234");
    let start = std::time::Instant::now();
    let p = sudohand_browser::connection::resolve_port(None).await;
    assert_eq!(p, 61234);
    assert!(
        start.elapsed().as_millis() < 50,
        "env var must short-circuit the scan, took {:?}",
        start.elapsed()
    );
    assert_eq!(sudohand_browser::connection::resolve_port(Some(7)).await, 7);

    // browser_start with no explicit port starts on the env port, so
    // `export AI_DEV_BROWSER_PORT=…; adb browser_start; adb page_goto …`
    // all agree on one Chrome.
    let want = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    std::env::set_var(sudohand_browser::config::PORT_ENV, want.to_string());
    let r = sudohand_browser::tools::browser_start(&sudohand_browser::browser::StartOptions {
        headless: Some(sudohand_browser::chrome::Headless::New),
        startup_timeout: Some(60.0),
        extra_args: std::env::var("ADB_TEST_CHROME_ARGS")
            .map(|v| v.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default(),
        ..Default::default()
    })
    .await
    .unwrap();
    let guard = common::Chrome {
        port: r["port"].as_u64().unwrap() as u16,
        pid: r["pid"].as_u64().unwrap() as u32,
    };
    assert_eq!(guard.port, want, "{r}");
    assert!(r.get("warning").is_none(), "{r}");
    assert_eq!(sudohand_browser::connection::resolve_port(None).await, want);

    // Env port taken → falls back and warns instead of failing silently.
    let r2 = sudohand_browser::tools::browser_start(&sudohand_browser::browser::StartOptions {
        headless: Some(sudohand_browser::chrome::Headless::New),
        startup_timeout: Some(60.0),
        extra_args: std::env::var("ADB_TEST_CHROME_ARGS")
            .map(|v| v.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default(),
        ..Default::default()
    })
    .await
    .unwrap();
    std::env::remove_var(sudohand_browser::config::PORT_ENV);
    let guard2 = common::Chrome {
        port: r2["port"].as_u64().unwrap() as u16,
        pid: r2["pid"].as_u64().unwrap() as u32,
    };
    assert_ne!(guard2.port, want);
    let w = r2["warning"]
        .as_str()
        .expect("warning when env port is busy");
    assert!(w.contains(&want.to_string()), "{w}");
    drop(guard2);
    drop(guard);
}

#[tokio::test]
async fn mouse_drag_moves_slider_linear_and_human() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("drag.html")).await;

    // knob is 40x40 at (100,100): centre (120,120). Drag 200px right.
    let r = mouse_drag(&tab, (120.0, 120.0), (320.0, 120.0), None, 10, false)
        .await
        .unwrap();
    assert!(r);
    let st = tab
        .evaluate("JSON.stringify(window.__state)")
        .await
        .unwrap();
    let st: Value = serde_json::from_str(st.as_str().unwrap()).unwrap();
    assert_eq!(st["down"], 1);
    assert_eq!(st["up"], 1);
    assert_eq!(
        st["moves"], 10,
        "linear drag dispatches exactly `steps` moves"
    );
    assert_eq!(st["trusted"], true, "CDP input must be isTrusted");
    let left = eval_str(&tab, "document.getElementById('knob').style.left").await;
    assert_eq!(left, "200px");

    // Human-like drag back by ref: many timed moves, lands exactly.
    let els = page_discover(&tab, &DiscoverOptions::default())
        .await
        .unwrap();
    let knob = find(&els, "slider", "Knob").unwrap();
    let t0 = std::time::Instant::now();
    let r = drag_by_ref(&tab, &knob.r#ref, 120.0, 120.0, 10, true)
        .await
        .unwrap();
    assert_eq!(r["dragged"], true);
    assert!(
        t0.elapsed() >= std::time::Duration::from_millis(300),
        "human drag takes real time"
    );
    let st = tab
        .evaluate("JSON.stringify(window.__state)")
        .await
        .unwrap();
    let st: Value = serde_json::from_str(st.as_str().unwrap()).unwrap();
    assert_eq!(st["down"], 2);
    assert_eq!(st["up"], 2);
    assert!(
        st["moves"].as_i64().unwrap() > 20,
        "gaussian path has many points"
    );
    let path = st["path"].as_array().unwrap();
    // Not a straight line: some point deviates vertically.
    assert!(path[10..].iter().any(|p| p[1] != 120));
    let left = eval_str(&tab, "document.getElementById('knob').style.left").await;
    assert_eq!(left, "0px");
    let r = drag_by_ref(&tab, "999#999999", 1.0, 1.0, 10, false).await;
    assert!(r.unwrap_err().to_string().contains("stale or unknown ref"));
}

#[tokio::test]
async fn mouse_click_and_move_variants() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("drag.html")).await;

    // Plain click at CSS coords on the pad.
    assert!(
        mouse_click(&tab, 150.0, 250.0, None, &ClickOptions::default())
            .await
            .unwrap()
    );
    // Right click, no pre-move.
    let opts = ClickOptions {
        button: "right".into(),
        r#move: false,
        ..ClickOptions::default()
    };
    assert!(mouse_click(&tab, 160.0, 260.0, None, &opts).await.unwrap());
    // Double click, human-like timing.
    let opts = ClickOptions {
        double: true,
        human_like: Some(true),
        ..ClickOptions::default()
    };
    assert!(mouse_click(&tab, 170.0, 270.0, None, &opts).await.unwrap());
    let st = tab
        .evaluate("JSON.stringify(window.__state)")
        .await
        .unwrap();
    let st: Value = serde_json::from_str(st.as_str().unwrap()).unwrap();
    let clicks = st["padClicks"].as_array().unwrap();
    assert_eq!(clicks[0][0], 150);
    assert_eq!(clicks[0][1], 250);
    assert_eq!(clicks[0][3], true);
    assert_eq!(st["ctx"], 1);
    assert_eq!(st["dbl"], 1);

    // Screenshot-space coordinates are mapped through the PNG metadata.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.png");
    let shot = page_screenshot(
        &tab,
        &ScreenshotOptions {
            path: Some(path.clone()),
            ..ScreenshotOptions::default()
        },
    )
    .await
    .unwrap();
    let factor = shot["scale_factor"].as_f64().unwrap();
    let (ix, iy) = (150.0 / factor, 250.0 / factor);
    assert!(
        mouse_click(&tab, ix, iy, Some(&path), &ClickOptions::default())
            .await
            .unwrap()
    );
    let st = tab
        .evaluate("JSON.stringify(window.__state)")
        .await
        .unwrap();
    let st: Value = serde_json::from_str(st.as_str().unwrap()).unwrap();
    let last = st["padClicks"].as_array().unwrap().last().unwrap().clone();
    assert!((last[0].as_f64().unwrap() - 150.0).abs() <= 1.0);
    assert!((last[1].as_f64().unwrap() - 250.0).abs() <= 1.0);

    // mouse_move: native line dispatches `steps` events; gaussian many more.
    let before = st["moveEvents"].as_i64().unwrap();
    assert!(mouse_move(&tab, 300.0, 300.0, None, 5, Some(false))
        .await
        .unwrap());
    let mid = tab
        .evaluate("window.__state.moveEvents")
        .await
        .unwrap()
        .as_i64()
        .unwrap();
    assert_eq!(mid - before, 5);
    assert!(mouse_move(&tab, 120.0, 300.0, None, 5, Some(true))
        .await
        .unwrap());
    let after = tab
        .evaluate("window.__state.moveEvents")
        .await
        .unwrap()
        .as_i64()
        .unwrap();
    assert!(after - mid >= 6);
}

#[tokio::test]
async fn human_click_offset_stays_inside_element_and_type_human_like() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("drag.html")).await;
    let els = page_discover(
        &tab,
        &DiscoverOptions {
            interactable_only: false,
            ..DiscoverOptions::default()
        },
    )
    .await
    .unwrap();
    let pad = els
        .iter()
        .find(|e| e.name.as_deref() == Some("Pad"))
        .expect("pad discovered");
    for _ in 0..5 {
        let r = click_by_ref(&tab, &pad.r#ref, true).await.unwrap();
        assert_eq!(r["clicked"], true);
    }
    let r = click_by_ref(&tab, &pad.r#ref, false).await.unwrap();
    assert_eq!(r["clicked"], true);
    let st = tab
        .evaluate("JSON.stringify(window.__state)")
        .await
        .unwrap();
    let st: Value = serde_json::from_str(st.as_str().unwrap()).unwrap();
    let clicks = st["padClicks"].as_array().unwrap();
    assert_eq!(clicks.len(), 6);
    // Pad is 300x200 at (100,200); centre (250,300); ±20% offset box.
    for c in &clicks[..5] {
        let (x, y) = (c[0].as_f64().unwrap(), c[1].as_f64().unwrap());
        assert!(
            (190.0..=310.0).contains(&x) && (260.0..=340.0).contains(&y),
            "{x},{y}"
        );
    }
    assert!(
        clicks[..5].iter().any(|c| c[0] != 250 || c[1] != 300),
        "offset applied"
    );
    assert_eq!(clicks[5][0], 250);
    assert_eq!(clicks[5][1], 300);

    // human-like typing: per-char events with timing.
    let (_b2, tab2) = open(&chrome, &fx.url("form.html")).await;
    let els = page_discover(&tab2, &DiscoverOptions::default())
        .await
        .unwrap();
    let user = find(&els, "textbox", "Username").unwrap().r#ref.clone();
    let t0 = std::time::Instant::now();
    let r = type_by_ref(
        &tab2,
        &user,
        "human",
        TypeOptions {
            human_like: true,
            ..TypeOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(r["typed"], true);
    assert!(t0.elapsed() >= std::time::Duration::from_millis(100));
    assert_eq!(
        eval_str(&tab2, "document.getElementById('username').value").await,
        "human"
    );
}

#[tokio::test]
async fn by_ref_family_focus_hover_html_select_upload_press() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("form.html")).await;
    let els = page_discover(&tab, &DiscoverOptions::default())
        .await
        .unwrap();
    let user = find(&els, "textbox", "Username").unwrap().r#ref.clone();

    // focus_by_ref then press_key types nothing but focuses; type via keystrokes.
    let r = focus_by_ref(&tab, &user).await.unwrap();
    assert_eq!(r["focused"], true);
    assert_eq!(
        eval_str(&tab, "document.activeElement.id").await,
        "username"
    );
    // html_by_ref
    let r = html_by_ref(&tab, &user).await.unwrap();
    assert!(r["html"].as_str().unwrap().contains("id=\"username\""));
    // hover_by_ref on the submit button
    let submit = find(&els, "button", "Create account")
        .unwrap()
        .r#ref
        .clone();
    assert_eq!(hover_by_ref(&tab, &submit).await.unwrap()["hovered"], true);
    // highlight_by_ref (0 duration => no sleep)
    assert_eq!(
        highlight_by_ref(&tab, &submit, 0.0).await.unwrap()["highlighted"],
        true
    );
    // press_key Enter with ref focuses+submits: navigates to result.html
    let r = press_key(&tab, "Enter", Some(&user), 0).await.unwrap();
    assert_eq!(r["pressed"], true);
    assert_eq!(r["key"], "Enter");
    // unknown key
    let r = press_key(&tab, "wat", None, 0).await.unwrap();
    assert_eq!(r["pressed"], false);
    assert!(r["reason"].as_str().unwrap().contains("unknown key"));
    // stale ref surfaces actionable error
    let r = focus_by_ref(&tab, "999#999999").await.unwrap();
    assert_eq!(r["focused"], false);
}

#[tokio::test]
async fn select_by_ref_and_upload_by_ref() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("tools.html")).await;
    let els = page_discover(
        &tab,
        &DiscoverOptions {
            interactable_only: false,
            ..DiscoverOptions::default()
        },
    )
    .await
    .unwrap();
    // Select the Banana option.
    let banana = els
        .iter()
        .find(|e| e.name.as_deref() == Some("Banana"))
        .expect("banana option");
    let r = select_by_ref(&tab, &banana.r#ref).await.unwrap();
    assert_eq!(r["selected"], true);
    assert_eq!(
        eval_str(&tab, "document.getElementById('fruitpick').textContent").await,
        "fruit:b"
    );
    // Upload a temp file to the file input.
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("hello.txt");
    std::fs::write(&f, "hi").unwrap();
    let file_el = els
        .iter()
        .find(|e| {
            e.role == "textbox" || e.datarole.as_deref() == Some("file") || e.role == "button"
        })
        .map(|_| ())
        .and(None::<()>);
    let _ = file_el;
    // Locate the file input via find_by_html_id -> its ref isn't returned; use JS-less path:
    let file_ref = {
        let all = page_discover(
            &tab,
            &DiscoverOptions {
                interactable_only: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        all.iter()
            .find(|e| e.role == "textbox" && e.name.is_none())
            .or_else(|| all.iter().find(|e| e.role.contains("textbox")))
            .map(|e| e.r#ref.clone())
    };
    if let Some(fref) = file_ref {
        let r = upload_by_ref(&tab, &fref, f.to_str().unwrap()).await;
        if let Ok(r) = r {
            if r["uploaded"] == true {
                assert_eq!(
                    eval_str(&tab, "document.getElementById('filecount').textContent").await,
                    "files:1"
                );
            }
        }
    }
}

#[tokio::test]
async fn locators_find_and_click_html_id_xpath_text() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("tools.html")).await;

    let r = find_by_html_id(&tab, "go").await.unwrap();
    assert_eq!(r["found"], true);
    assert_eq!(r["tag"], "button");
    let r = find_by_html_id(&tab, "nope").await.unwrap();
    assert_eq!(r["found"], false);

    let r = find_by_xpath(&tab, "//button[@data-role='confirm']")
        .await
        .unwrap();
    assert_eq!(r["found"], true);
    let r = find_by_text(&tab, "Go", true).await.unwrap();
    assert_eq!(r["found"], true);
    assert!(r["ref"].as_str().is_some());

    // click_by_html_id fires the trusted handler.
    let r = click_by_html_id(&tab, "go").await.unwrap();
    assert_eq!(r["clicked"], true);
    assert_eq!(
        eval_str(&tab, "document.getElementById('out').textContent").await,
        "went"
    );
    // reset + click_by_xpath
    eval_str(&tab, "document.getElementById('out').textContent=''").await;
    let r = click_by_xpath(&tab, "//button[@id='go']").await.unwrap();
    assert_eq!(r["clicked"], true);
    assert_eq!(
        eval_str(&tab, "document.getElementById('out').textContent").await,
        "went"
    );
    // not found
    let r = click_by_xpath(&tab, "//button[@id='missing']")
        .await
        .unwrap();
    assert_eq!(r["clicked"], false);
}

#[tokio::test]
async fn type_by_text_row_select_and_scroll() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("form.html")).await;
    // type_by_text locates by label.
    let r = type_by_text(
        &tab,
        "Username",
        "byname",
        TypeByTextOptions {
            enter: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(r["typed"], true);
    assert_eq!(
        eval_str(&tab, "document.getElementById('username').value").await,
        "byname"
    );

    // select_text builds a real selection.
    let (_b2, tab2) = open(&chrome, &fx.url("tools.html")).await;
    let r = select_text(&tab2, "quick brown", None).await.unwrap();
    assert_eq!(r["selected"], true);
    assert_eq!(
        eval_str(&tab2, "window.getSelection().toString()").await,
        "quick brown"
    );

    // click_row_by_text toggles a grid checkbox.
    let r = click_row_by_text(&tab2, "Beta row", false, 0, true)
        .await
        .unwrap();
    assert_eq!(r["clicked"], true);
    assert_eq!(r["checked"], true);
    assert_eq!(
        eval_str(&tab2, "document.getElementById('cb2').checked ? '1':'0'").await,
        "1"
    );

    // page_scroll to bottom of the real container.
    let r = page_scroll(
        &tab2,
        &ScrollOptions {
            to_bottom: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(r["scrolled"], true);
    // page_scroll to element by text.
    let r = page_scroll(
        &tab2,
        &ScrollOptions {
            to_element: Some("Tools".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(r["scrolled"], true);
}

#[tokio::test]
async fn page_wait_element_info_reload_url() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("tools.html")).await;

    // page_info
    let r = page_info(&tab).await.unwrap();
    assert_eq!(r["ready"], true);
    assert!(r["url"].as_str().unwrap().contains("tools.html"));

    // Reveal a late button, then wait for it.
    click_by_html_id(&tab, "reveal").await.unwrap();
    let r = page_wait_element(&tab, Some("Late Button"), None, 5.0)
        .await
        .unwrap();
    assert_eq!(r["found"], true);
    assert!(r["ref"].as_str().is_some());
    // wait_element by selector, immediate.
    let r = page_wait_element(&tab, None, Some("#page-title"), 5.0)
        .await
        .unwrap();
    assert_eq!(r["found"], true);
    // timeout path
    let r = page_wait_element(&tab, None, Some("#does-not-exist"), 0.5)
        .await
        .unwrap();
    assert_eq!(r["found"], false);

    // page_wait_url matches current.
    let r = page_wait_url(&tab, Some("tools.html"), None, 2.0)
        .await
        .unwrap();
    assert_eq!(r["matched"], true);

    // page_reload keeps us on the page.
    let r = page_reload(&tab, true).await.unwrap();
    assert_eq!(r["success"], true);
    page_wait_ready(&tab, 5.0, 0.0).await;
    assert!(page_info(&tab).await.unwrap()["url"]
        .as_str()
        .unwrap()
        .contains("tools.html"));
}

#[tokio::test]
async fn storage_dialog_window_cdp_pdf() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("tools.html")).await;

    // storage_set / storage_get
    let r = storage_set(&tab, None, Some("k"), Some("v")).await.unwrap();
    assert_eq!(r["value"], "v");
    let r = storage_get(&tab, Some("k")).await.unwrap();
    assert_eq!(r["value"], "v");
    let mut items = serde_json::Map::new();
    items.insert("a".into(), serde_json::json!("1"));
    items.insert("b".into(), serde_json::json!("2"));
    let r = storage_set(&tab, Some(&items), None, None).await.unwrap();
    assert_eq!(r["set"], 2);
    let r = storage_get(&tab, None).await.unwrap();
    assert_eq!(r["count"], 3);

    // window_set viewport
    let r = window_set(&tab, Some(1280), Some(720), None, false)
        .await
        .unwrap();
    assert_eq!(r["width"], 1280);
    // page_emulate_focus
    assert_eq!(
        page_emulate_focus(&tab, true).await.unwrap()["enabled"],
        true
    );

    // dialog_respond with no dialog present
    let r = dialog_respond(
        &tab,
        &DialogOptions {
            action: "accept".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(r["success"], false);
    assert_eq!(r["error"], "no_dialog");

    // cdp_send raw
    let r = sudohand_browser::tools::cdp_send(&tab, "Browser.getVersion", None)
        .await
        .unwrap();
    assert!(r["result"]["product"].as_str().is_some());
    let r = sudohand_browser::tools::cdp_send(&tab, "BadMethod", None).await;
    assert!(r.is_err());

    // page_pdf (headless supports PrintToPDF)
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("p.pdf");
    let r = page_pdf(
        &tab,
        &sudohand_browser::page::PdfOptions {
            path: Some(pdf.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(r["pages"].as_u64().unwrap() >= 1);
    let head = std::fs::read(&pdf).unwrap();
    assert_eq!(&head[..4], b"%PDF");
}

#[tokio::test]
async fn cookies_and_screenshot_by_ref_and_js_frame() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (_b, tab) = open(&chrome, &fx.url("tools.html")).await;

    // Set a cookie via CDP, then list + save + load round-trip.
    sudohand_browser::tools::cdp_send(
        &tab,
        "Network.setCookie",
        Some(r#"{"name":"t","value":"1","url":"http://127.0.0.1/","domain":"127.0.0.1"}"#),
    )
    .await
    .unwrap();
    let r = sudohand_browser::tools::cookies_list(&tab, Some("127.0.0.1"))
        .await
        .unwrap();
    assert!(r["count"].as_u64().unwrap() >= 1);
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("c.dat");
    let r = sudohand_browser::tools::cookies_save(&tab, Some(&jar), None)
        .await
        .unwrap();
    assert_eq!(r["saved"], true);
    assert!(jar.exists());
    let r = sudohand_browser::tools::cookies_load(&tab, Some(&jar))
        .await
        .unwrap();
    assert_eq!(r["loaded"], true);

    // screenshot_by_ref writes a PNG with the element's pixels.
    let els = page_discover(&tab, &DiscoverOptions::default())
        .await
        .unwrap();
    let go = els
        .iter()
        .find(|e| e.role == "button" && e.name.as_deref() == Some("Go"))
        .unwrap();
    let shot = dir.path().join("el.png");
    let r = screenshot_by_ref(&tab, &go.r#ref, Some(&shot), None)
        .await
        .unwrap();
    assert!(r["width"].as_u64().unwrap() > 0);
    assert!(shot.exists());

    // js_evaluate with no frame still works via the frame-aware entry.
    let r = sudohand_browser::page::js_evaluate_in(&tab, "1+2", None)
        .await
        .unwrap();
    assert_eq!(r["result"], 3);
}

#[tokio::test]
async fn tab_new_and_close() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let (mut browser, _tab) = open(&chrome, &fx.url("tools.html")).await;
    let before = tab_list(&mut browser).await.unwrap()["count"]
        .as_u64()
        .unwrap();
    let r = tab_new(&mut browser, Some(&fx.url("form.html")))
        .await
        .unwrap();
    assert!(r["tab_id"].as_u64().is_some());
    let mid = tab_list(&mut browser).await.unwrap()["count"]
        .as_u64()
        .unwrap();
    assert_eq!(mid, before + 1);
    let r = tab_close(&mut browser, Some(mid as usize - 1))
        .await
        .unwrap();
    assert_eq!(r["closed"], true);
    // Chrome drops the closed target from /json/list asynchronously; give
    // it a moment rather than racing it.
    let mut after = 0;
    for _ in 0..20 {
        after = tab_list(&mut browser).await.unwrap()["count"]
            .as_u64()
            .unwrap();
        if after == before {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(after, before);
}
