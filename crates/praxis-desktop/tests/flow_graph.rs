//! The graph-flow orchestration against the fake backend + a scripted VLM:
//! the conditional edge, the GoTo recovery loop, and abort on exhausted
//! attempts.

use praxis_desktop::flow::{run_graph, wechat_send};
use praxis_desktop::vlm::{NormPoint, Vlm};
use praxis_desktop::workflow::{norm_to_point, Runner};
use praxis_desktop::{FakeBackend, Result};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

struct FakeVlm {
    answers: Mutex<Vec<&'static str>>,
}

impl Vlm for FakeVlm {
    fn locate(&self, _png: &[u8], _d: &str) -> Result<NormPoint> {
        Ok(NormPoint { x: 500.0, y: 250.0 })
    }
    fn ask(&self, _png: &[u8], _q: &str) -> Result<String> {
        let mut a = self.answers.lock().unwrap();
        Ok(if a.is_empty() { "yes" } else { a.remove(0) }.to_string())
    }
}

fn vars() -> HashMap<String, String> {
    [
        ("contact".to_string(), "Kai".to_string()),
        ("message".to_string(), "[ADC test] ignore".to_string()),
    ]
    .into()
}

fn setup(answers: Vec<&'static str>) -> (Arc<FakeBackend>, Runner) {
    let b = FakeBackend::with_running(&["com.tencent.xinWeChat"]);
    let vlm = Arc::new(FakeVlm {
        answers: Mutex::new(answers),
    });
    let r = Runner::new(b.clone(), vlm);
    (b, r)
}

#[test]
fn norm_to_point_mapping() {
    let b = FakeBackend::with_running(&["x"]);
    let shot = praxis_desktop::DesktopBackend::screenshot(&*b, "x", 42, None).unwrap();
    let p = norm_to_point(NormPoint { x: 1000.0, y: 0.0 }, &shot);
    assert_eq!(p, (100.0 + 800.0, 50.0));
}

#[tokio::test]
async fn chat_already_open_skips_search() {
    // ensure_chat: yes → focus_input check: yes → verify transcript
    let (b, r) = setup(vec!["yes", "yes", "[ADC test] ignore"]);
    let report = run_graph(wechat_send(&r).unwrap(), vars()).await.unwrap();
    let names: Vec<_> = report
        .iter()
        .map(|s| s.step.split(':').next().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "ensure_chat",
            "focus_input",
            "type_message",
            "send",
            "wait_sent",
            "verify_sent"
        ]
    );
    let log = b.actions();
    assert!(!log.iter().any(|l| l.contains("type \"Kai\"")), "{log:?}");
    assert!(log.contains(&"type \"[ADC test] ignore\"".to_string()));
}

#[tokio::test]
async fn wrong_chat_goes_through_search_and_retries_once() {
    // ensure_chat: no → pick_from_list check: no → GoTo open_search →
    // select_all/type/wait → pick_result check: yes → focus_input: yes → verify ok
    let (b, r) = setup(vec!["no", "no", "yes", "yes", "[ADC test] ignore"]);
    let report = run_graph(wechat_send(&r).unwrap(), vars()).await.unwrap();
    let names: Vec<_> = report
        .iter()
        .map(|s| s.step.split(':').next().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "ensure_chat",
            // pick_from_list failed (no report) → search path
            "open_search",
            "select_all",
            "type_contact",
            "wait_results",
            "pick_result",
            "focus_input",
            "type_message",
            "send",
            "wait_sent",
            "verify_sent"
        ]
    );
    assert_eq!(
        b.actions()
            .iter()
            .filter(|l| l.as_str() == "type \"Kai\"")
            .count(),
        1
    );
}

#[tokio::test]
async fn search_result_wrong_retries_search_then_succeeds() {
    // ensure_chat: no → pick_from_list: no → search → pick_result: no →
    // GoTo open_search → pick_result: yes → focus_input: yes → verify
    let (b, r) = setup(vec!["no", "no", "no", "yes", "yes", "[ADC test] ignore"]);
    let report = run_graph(wechat_send(&r).unwrap(), vars()).await.unwrap();
    let names: Vec<_> = report
        .iter()
        .map(|s| s.step.split(':').next().unwrap())
        .collect();
    assert_eq!(names.iter().filter(|n| **n == "open_search").count(), 2);
    assert_eq!(names.last(), Some(&"verify_sent"));
    assert_eq!(
        b.actions()
            .iter()
            .filter(|l| l.as_str() == "type \"Kai\"")
            .count(),
        2
    );
}

#[tokio::test]
async fn input_not_empty_aborts_before_typing() {
    let (b, r) = setup(vec!["yes", "no, there is a draft"]);
    let err = run_graph(wechat_send(&r).unwrap(), vars())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("focus_input"), "{err}");
    assert!(!b.actions().iter().any(|l| l.starts_with("type")));
}
