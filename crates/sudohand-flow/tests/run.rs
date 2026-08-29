//! The engine over a fake dispatch: substitution, binding across actuators,
//! retries, optional steps, and var checking.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::Mutex as M2;
use sudohand_core::{Error, Result};
use sudohand_flow::{Dispatch, Prompter, Runner, Step, Vars, Workflow};

/// Answers scripted up front: confirms, lines, and select indices.
struct ScriptedPrompter {
    confirms: M2<Vec<bool>>,
    lines: M2<Vec<String>>,
    selects: M2<Vec<usize>>,
}
impl ScriptedPrompter {
    fn new() -> Self {
        Self {
            confirms: M2::new(vec![]),
            lines: M2::new(vec![]),
            selects: M2::new(vec![]),
        }
    }
    fn confirms(self, v: Vec<bool>) -> Self {
        *self.confirms.lock().unwrap() = v;
        self
    }
    fn selects(self, v: Vec<usize>) -> Self {
        *self.selects.lock().unwrap() = v;
        self
    }
}
impl Prompter for ScriptedPrompter {
    fn line(&self, _m: &str, default: Option<&str>) -> sudohand_core::Result<String> {
        let mut l = self.lines.lock().unwrap();
        Ok(if l.is_empty() {
            default.unwrap_or("").to_string()
        } else {
            l.remove(0)
        })
    }
    fn confirm(&self, _m: &str) -> sudohand_core::Result<bool> {
        Ok(self.confirms.lock().unwrap().remove(0))
    }
    fn select(&self, _m: &str, _o: &[String]) -> sudohand_core::Result<usize> {
        Ok(self.selects.lock().unwrap().remove(0))
    }
}

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

#[test]
fn select_confirm_and_action_chain() {
    // `wx accounts` returns a list; select picks one; confirm proceeds;
    // then an action uses the bound wxid.
    let fake = FakeDispatch::new().script(
        "wx accounts",
        vec![Ok(
            json!({"accounts":[{"wxid":"a_1","n":3},{"wxid":"b_2","n":9}]}),
        )],
    );
    let wf = Workflow::new("init")
        .step(Step::run("scan", ["wx", "accounts"]).bind("accts"))
        .step(Step::select(
            "pick",
            "account?",
            "accts.accounts",
            "{{it.wxid}} ({{it.n}})",
            "{{it.wxid}}",
            "account",
        ))
        .step(Step::confirm("ok", "log out {{account}}?"))
        .step(Step::run(
            "cap",
            ["wx", "capture", "--account", "{{account}}"],
        ));
    let prompter = ScriptedPrompter::new()
        .selects(vec![1])
        .confirms(vec![true]);
    let report = Runner::with_prompter(fake, prompter)
        .run(&wf, Vars::new())
        .unwrap();
    assert!(report.ok && !report.cancelled);
    assert_eq!(report.vars["account"], json!("b_2"));
    assert_eq!(
        report.steps.last().unwrap().run,
        ["wx", "capture", "--account", "b_2"]
    );
}

#[test]
fn single_account_auto_selected() {
    let fake = FakeDispatch::new().script(
        "wx accounts",
        vec![Ok(json!({"accounts":[{"wxid":"solo"}]}))],
    );
    let wf = Workflow::new("init")
        .step(Step::run("scan", ["wx", "accounts"]).bind("accts"))
        .step(Step::select(
            "pick",
            "?",
            "accts.accounts",
            "{{it.wxid}}",
            "{{it.wxid}}",
            "account",
        ));
    // no select answer scripted -> must be auto-picked
    let report = Runner::with_prompter(fake, ScriptedPrompter::new())
        .run(&wf, Vars::new())
        .unwrap();
    assert_eq!(report.vars["account"], json!("solo"));
}

#[test]
fn declined_confirm_cancels_cleanly() {
    let wf = Workflow::new("init")
        .step(Step::confirm("ok", "proceed?"))
        .step(Step::run("never", ["fs", "exists", "--path", "/"]));
    let prompter = ScriptedPrompter::new().confirms(vec![false]);
    let report = Runner::with_prompter(FakeDispatch::new(), prompter)
        .run(&wf, Vars::new())
        .unwrap();
    assert!(!report.ok);
    assert!(report.cancelled);
    assert_eq!(report.steps.len(), 1); // stopped at the confirm; action never ran
}
