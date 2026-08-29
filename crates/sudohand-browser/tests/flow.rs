//! Browser workflows against the fixture server + headless Chrome, plus the
//! registry's name/var validation (which needs a live `Runner`).

mod common;

use common::{skip_browser_tests, start_chrome, Fixtures};
use std::collections::HashMap;
use sudohand_browser::flow::{self, run_graph, StepTask};
use sudohand_browser::registry::Registry;
use sudohand_browser::workflow::{Runner, Step};
use sudohand_browser::Error as BrowserError;
use sudohand_core::Error;

fn err_of<T>(r: Result<T, Error>) -> Error {
    match r {
        Ok(_) => panic!("expected an error"),
        Err(e) => e,
    }
}

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

async fn runner(port: u16) -> Runner {
    Runner::connect(Some(port), None).await.expect("connect")
}

#[tokio::test]
async fn registry_lists_validates_and_runs_builtins() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let rn = runner(chrome.port).await;

    let reg = Registry::builtins();
    let names: Vec<_> = reg.list().into_iter().map(|e| e.name).collect();
    assert_eq!(names, ["form-signup", "page-extract"]);

    let err = err_of(reg.prepare("form-signup", &rn, &HashMap::new()));
    assert!(matches!(err, Error::InvalidInput(_)), "{err}");
    assert!(err.to_string().contains("--var url="));
    assert!(matches!(
        err_of(reg.prepare("nope", &rn, &HashMap::new())),
        Error::NotFound(_)
    ));

    // page-extract: open a fixture and pull its title.
    let report = reg
        .run(
            "page-extract",
            &rn,
            vars(&[
                ("url", &fx.url("buttons.html")),
                ("expression", "document.title"),
            ]),
        )
        .await
        .unwrap();
    assert_eq!(report.len(), 3);
    assert_eq!(report[2].step, "extract: js(document.title)");
    assert_eq!(report[2].result, Some("Buttons Fixture".into()));

    // form-signup: fill, submit, land on result.html, verify the query.
    let report = reg
        .run(
            "form-signup",
            &rn,
            vars(&[
                ("url", &fx.url("form.html")),
                ("username", "kai"),
                ("email", "kai@example.com"),
            ]),
        )
        .await
        .unwrap();
    let steps: Vec<_> = report.iter().map(|r| r.step.as_str()).collect();
    assert_eq!(
        steps,
        [
            "open: goto(".to_string() + &fx.url("form.html") + ")",
            "username: type_text(Username, kai)".into(),
            "email: type_text(Email, kai@example.com)".into(),
            "submit: click_text(Create account)".into(),
            "landed: wait_url(result)".into(),
            "verify: verify(document.getElementById('query').textContent)".into(),
        ]
    );
    let landed = report[4].result.as_ref().unwrap();
    assert_eq!(landed["matched"], true);
    assert!(landed["url"].as_str().unwrap().contains("username=kai"));
}

#[tokio::test]
async fn soft_failures_fail_the_step_and_goto_recovers() {
    if skip_browser_tests() {
        return;
    }
    let fx = Fixtures::serve();
    let chrome = start_chrome().await;
    let rn = runner(chrome.port).await;
    let v = vars(&[("url", &fx.url("buttons.html"))]);

    // A missing button is a soft failure in adb (`clicked: false`) → Err here.
    rn.step(
        &Step::Goto {
            url: "{{url}}".into(),
            wait: true,
        },
        &v,
    )
    .await
    .unwrap();
    let err = rn
        .step(
            &Step::ClickText {
                text: "No such button".into(),
                timeout: 0.2,
            },
            &v,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Io(_)), "{err}");
    assert!(err.to_string().contains("clicked=false"), "{err}");

    // verify: expectation not met → Err with both strings.
    let err = rn
        .step(
            &Step::Verify {
                expression: "document.title".into(),
                expect: "Nope".into(),
            },
            &v,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("expected \"Nope\""));

    // Graph recovery: `Back` only exists on result.html, so the first click
    // fails (soft `clicked: false`), GoTo runs `go_home` (which navigates
    // there), and the retry succeeds and lands back on buttons.html.
    let open = StepTask::new(
        "open",
        &rn,
        Step::Goto {
            url: fx.url("buttons.html"),
            wait: true,
        },
    );
    let go_home = StepTask::new(
        "go_home",
        &rn,
        Step::ClickText {
            text: "Home".into(),
            timeout: 2.0,
        },
    );
    let back = StepTask::new(
        "back",
        &rn,
        Step::ClickText {
            text: "Back".into(),
            timeout: 0.2,
        },
    )
    .recover_with("go_home", 3);
    let landed = StepTask::new(
        "landed",
        &rn,
        Step::WaitUrl {
            pattern: "buttons.html".into(),
            timeout: 5.0,
        },
    );
    let done = std::sync::Arc::new(flow::EndTask);
    use graph_flow::Task;
    let graph = flow::graph_flow::GraphBuilder::new("recover")
        .add_task(open.clone())
        .add_task(back.clone())
        .add_task(go_home.clone())
        .add_task(landed.clone())
        .add_task(done.clone())
        .set_start_task(open.id())
        .add_edge(open.id(), back.id())
        .add_edge(go_home.id(), back.id())
        .add_edge(back.id(), landed.id())
        .add_edge(landed.id(), done.id())
        .with_max_execution_steps(20)
        .build()
        .unwrap();
    let report = run_graph(graph, HashMap::new()).await.unwrap();
    let steps: Vec<_> = report.iter().map(|r| r.step.as_str()).collect();
    assert_eq!(
        steps,
        [
            "open: goto(".to_string() + &fx.url("buttons.html") + ")",
            "go_home: click_text(Home)".into(),
            "back: click_text(Back)".into(),
            "landed: wait_url(buttons.html)".into(),
        ]
    );
    assert_eq!(report[1].result.as_ref().unwrap()["navigated"], true);

    // The browser error type maps onto sudohand categories.
    let e: Error = BrowserError::Invalid("bad ref".into()).into();
    assert_eq!(e.code(), "invalid_input");
    let e: Error = BrowserError::Chrome("no chrome".into()).into();
    assert_eq!(e.code(), "not_found");
}
