//! The engine over a fake dispatch: substitution, binding across actuators,
//! retries, optional steps, and var checking.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use sudohand_core::{Error, Result};
use sudohand_flow::{Dispatch, Runner, Step, Vars, Workflow};

struct FakeDispatch {
    calls: Mutex<Vec<Vec<String>>>,
    scripted: Mutex<HashMap<String, Vec<Result<Value>>>>,
}

impl FakeDispatch {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            scripted: Mutex::new(HashMap::new()),
        }
    }
    fn script(self, action: &str, seq: Vec<Result<Value>>) -> Self {
        self.scripted.lock().unwrap().insert(action.into(), seq);
        self
    }
}

impl Dispatch for FakeDispatch {
    fn call(&self, argv: &[String]) -> Result<Value> {
        self.calls.lock().unwrap().push(argv.to_vec());
        let key = format!(
            "{} {}",
            argv.first().cloned().unwrap_or_default(),
            argv.get(1).cloned().unwrap_or_default()
        );
        let mut s = self.scripted.lock().unwrap();
        if let Some(seq) = s.get_mut(&key) {
            if !seq.is_empty() {
                return seq.remove(0);
            }
        }
        Ok(json!({ "ok": true }))
    }
}

#[test]
fn full_run_records_calls_and_binds() {
    let fake = FakeDispatch::new().script(
        "desktop locate",
        vec![Ok(json!({"point": {"x": 12, "y": 34}}))],
    );
    let wf = Workflow::new("chain")
        .var("q")
        .step(Step::run("find", ["desktop", "locate", "--find", "{{q}}"]).bind("loc"))
        .step(Step::run(
            "click",
            [
                "desktop",
                "click",
                "--x",
                "{{loc.point.x}}",
                "--y",
                "{{loc.point.y}}",
            ],
        ))
        .step(Step::run(
            "note",
            [
                "fs",
                "write",
                "--path",
                "/tmp/x",
                "--text",
                "at {{loc.point.x}}",
            ],
        ));
    let report = Runner::new(fake)
        .run(&wf, Vars::from_pairs([("q", "Log out")]))
        .unwrap();
    assert!(report.ok);
    assert_eq!(
        report.steps[1].run,
        ["desktop", "click", "--x", "12", "--y", "34"]
    );
    assert_eq!(report.steps[2].run.last().unwrap(), "at 12");
}

#[test]
fn retries_then_succeeds() {
    let fake = FakeDispatch::new().script(
        "desktop click",
        vec![Err(Error::io("miss")), Ok(json!({"clicked": true}))],
    );
    let wf = Workflow::new("retry")
        .step(Step::run("c", ["desktop", "click", "--x", "1", "--y", "2"]).attempts(3));
    let report = Runner::new(fake).run(&wf, Vars::new()).unwrap();
    assert!(report.ok);
    assert_eq!(report.steps[0].attempts, 2);
}

#[test]
fn failure_stops_unless_optional() {
    let fake = FakeDispatch::new().script("shell run", vec![Err(Error::io("boom"))]);
    let wf = Workflow::new("stop")
        .step(Step::run("a", ["shell", "run", "--", "false"]))
        .step(Step::run("b", ["fs", "exists", "--path", "/tmp"]));
    let report = Runner::new(fake).run(&wf, Vars::new()).unwrap();
    assert!(!report.ok);
    assert_eq!(report.steps.len(), 1);

    let fake = FakeDispatch::new().script("shell run", vec![Err(Error::io("boom"))]);
    let wf = Workflow::new("go")
        .step(Step::run("a", ["shell", "run", "--", "false"]).optional())
        .step(Step::run("b", ["fs", "exists", "--path", "/tmp"]));
    let report = Runner::new(fake).run(&wf, Vars::new()).unwrap();
    assert!(report.ok);
    assert_eq!(report.steps.len(), 2);
    assert!(report.steps[0].error.is_some());
}

#[test]
fn missing_var_is_invalid_input() {
    let wf = Workflow::new("needs").var("who");
    let err = Runner::new(FakeDispatch::new())
        .run(&wf, Vars::new())
        .unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err}");
    assert!(err.to_string().contains("who"));
}
