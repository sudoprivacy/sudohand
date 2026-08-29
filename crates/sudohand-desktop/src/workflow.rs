//! Workflow leaf steps and their executor. A [`Step`] is one desktop
//! action; a `click` carries a scripted position (`at`, in **points relative
//! to the window origin**) and/or a natural-language `find` for the VLM.
//! [`Runner::step`] clicks the scripted position first and asks the VLM to
//! locate the element only when the step's `check` question is answered
//! "no" — or when there is no scripted position. Deterministic when the
//! layout matches, self-healing when it does not.
//!
//! Steps are composed into graphs in code — see [`crate::flow`] — and
//! looked up by name through [`crate::registry`]. `{{name}}` placeholders in
//! step strings are substituted from the run's variables.

use crate::backend::{DesktopBackend, Screenshot};
use crate::vlm::{NormPoint, Vlm};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use sudohand_core::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum WindowPick {
    Id(u32),
    /// `"largest"` (anything that is not a window id).
    Largest(String),
}

impl Default for WindowPick {
    fn default() -> Self {
        WindowPick::Largest("largest".into())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// Bring the app to the front.
    Activate {},
    /// Click an element. `at` = scripted position in points relative to the
    /// window origin; `find` = description for the VLM; `check` = yes/no
    /// question asked after the click to confirm it worked.
    Click {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<[f64; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        find: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        check: Option<String>,
        #[serde(default = "one")]
        count: u32,
    },
    Type {
        text: String,
    },
    PasteFile {
        path: String,
    },
    Key {
        keys: String,
    },
    Wait {
        ms: u64,
    },
    /// Ask the VLM a question about the current screen; the answer must
    /// contain `expect` (case-insensitive) or the workflow fails.
    Verify {
        ask: String,
        expect: String,
    },
}

fn one() -> u32 {
    1
}

/// What the runner did for one step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StepReport {
    pub index: usize,
    pub step: String,
    /// `"script"`, `"vlm"` (fallback used), or `"-"`.
    pub resolved_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clicked: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    pub ms: u128,
}

#[derive(Clone)]
pub struct Runner {
    pub backend: Arc<dyn DesktopBackend>,
    pub vlm: Arc<dyn Vlm>,
    /// Screenshot width sent to the VLM (smaller = faster, cheaper).
    pub shot_width: u32,
    /// Pause after each click before the `check` screenshot.
    pub settle: Duration,
}

impl Runner {
    pub fn new(backend: Arc<dyn DesktopBackend>, vlm: Arc<dyn Vlm>) -> Self {
        Self {
            backend,
            vlm,
            shot_width: 1100,
            settle: Duration::from_millis(600),
        }
    }

    /// Run one step (with `{{var}}` substitution) against `app`, picking the
    /// window per `pick`. This is the primitive the `flow` graph tasks use.
    pub fn step(
        &self,
        app: &str,
        pick: &WindowPick,
        step: &Step,
        vars: &HashMap<String, String>,
    ) -> Result<Vec<StepReport>> {
        let mut ctx = Ctx {
            app: app.to_string(),
            pick: pick.clone(),
            vars,
        };
        let mut out = Vec::new();
        self.run_steps(std::slice::from_ref(step), &mut ctx, &mut out)?;
        Ok(out)
    }

    /// Screenshot and ask the VLM a yes/no question. Returns (is_yes, raw
    /// answer).
    pub fn ask(
        &self,
        app: &str,
        pick: &WindowPick,
        question: &str,
        vars: &HashMap<String, String>,
    ) -> Result<(bool, String)> {
        let ctx = Ctx {
            app: app.to_string(),
            pick: pick.clone(),
            vars,
        };
        let shot = self.shot(&ctx)?;
        let answer = self.vlm.ask(&shot.png, &subst(question, vars))?;
        Ok((is_yes(&answer), answer))
    }

    fn run_steps(&self, list: &[Step], ctx: &mut Ctx<'_>, out: &mut Vec<StepReport>) -> Result<()> {
        let app = ctx.app.clone();
        let vars = ctx.vars;
        for step in list {
            let index = out.len();
            let started = std::time::Instant::now();
            let mut rep = StepReport {
                index,
                step: step_name(step),
                resolved_by: "-".into(),
                clicked: None,
                answer: None,
                ms: 0,
            };
            match step {
                Step::Activate {} => self.backend.activate(&app)?,
                Step::Type { text } => self.backend.type_text(&subst(text, vars))?,
                Step::PasteFile { path } => self
                    .backend
                    .paste_file(std::path::Path::new(&subst(path, vars)))?,
                Step::Key { keys } => {
                    let (k, m) = crate::backend::parse_key(&subst(keys, vars))?;
                    self.backend.key(&k, m)?;
                }
                Step::Wait { ms } => std::thread::sleep(Duration::from_millis(*ms)),
                Step::Verify { ask, expect } => {
                    let shot = self.shot(ctx)?;
                    let answer = self.vlm.ask(&shot.png, &subst(ask, vars))?;
                    rep.answer = Some(answer.clone());
                    if !contains_ci(&answer, &subst(expect, vars)) {
                        return Err(Error::io(format!(
                            "step {index} verify failed: expected {expect:?}, model said {answer:?}"
                        )));
                    }
                }
                Step::Click {
                    at,
                    find,
                    check,
                    count,
                } => {
                    let find = find.as_deref().map(|f| subst(f, vars));
                    let check = check.as_deref().map(|c| subst(c, vars));
                    let spec = ClickSpec {
                        at: *at,
                        find: find.as_deref(),
                        check: check.as_deref(),
                        count: *count,
                    };
                    let (pt, by) = self.click_step(ctx, &spec, &mut rep)?;
                    rep.clicked = Some([pt.0, pt.1]);
                    rep.resolved_by = by.into();
                }
            }
            rep.ms = started.elapsed().as_millis();
            out.push(rep);
        }
        let _ = app;
        Ok(())
    }

    /// Screenshot the workflow's window. `"largest"` is re-resolved on every
    /// shot: apps swap windows (WeChat's search page, dialogs) mid-flow.
    fn shot(&self, ctx: &Ctx<'_>) -> Result<Screenshot> {
        let window = match ctx.pick {
            WindowPick::Id(id) => id,
            WindowPick::Largest(_) => pick_window(&*self.backend, &ctx.app)?,
        };
        let shot = self
            .backend
            .screenshot(&ctx.app, window, Some(self.shot_width))?;
        // `SUDOHAND_FLOW_DEBUG_DIR=/dir` (or adc's `ADC_FLOW_DEBUG_DIR`) keeps every
        // screenshot the runner takes (numbered) so a failed run can be
        // diagnosed after the fact.
        if let Ok(dir) = std::env::var("SUDOHAND_FLOW_DEBUG_DIR")
            .or_else(|_| std::env::var("ADC_FLOW_DEBUG_DIR"))
        {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let _ = std::fs::write(
                std::path::Path::new(&dir).join(format!("shot-{n:03}.png")),
                &shot.png,
            );
        }
        Ok(shot)
    }

    /// Scripted position first, VLM on failure. Returns the point clicked
    /// (screen points) and who resolved it.
    fn click_step(
        &self,
        ctx: &Ctx<'_>,
        spec: &ClickSpec<'_>,
        rep: &mut StepReport,
    ) -> Result<((f64, f64), &'static str)> {
        let ClickSpec {
            at,
            find,
            check,
            count,
        } = *spec;
        if at.is_none() && find.is_none() {
            return Err(Error::invalid("click step needs `at` and/or `find`"));
        }
        if let Some([rx, ry]) = at {
            let shot = self.shot(ctx)?;
            let pt = (shot.origin.0 + rx, shot.origin.1 + ry);
            self.backend.click(pt.0, pt.1, "left", count)?;
            match check {
                None => return Ok((pt, "script")),
                Some(q) => {
                    std::thread::sleep(self.settle);
                    let after = self.shot(ctx)?;
                    let answer = self.vlm.ask(&after.png, q)?;
                    rep.answer = Some(answer.clone());
                    if is_yes(&answer) {
                        return Ok((pt, "script"));
                    }
                    if find.is_none() {
                        return Err(Error::io(format!(
                            "click at {at:?} did not pass check {q:?} (model said {answer:?}) and no `find` to fall back on"
                        )));
                    }
                }
            }
        }
        let find = find.expect("checked above");
        let shot = self.shot(ctx)?;
        let norm = self.vlm.locate(&shot.png, find)?;
        let pt = norm_to_point(norm, &shot);
        self.backend.click(pt.0, pt.1, "left", count)?;
        if let Some(q) = check {
            std::thread::sleep(self.settle);
            let after = self.shot(ctx)?;
            let answer = self.vlm.ask(&after.png, q)?;
            rep.answer = Some(answer.clone());
            if !is_yes(&answer) {
                return Err(Error::io(format!(
                    "vlm click for {find:?} at ({:.0},{:.0}) did not pass check {q:?} (model said {answer:?})",
                    pt.0, pt.1
                )));
            }
        }
        Ok((pt, "vlm"))
    }
}

struct Ctx<'v> {
    app: String,
    pick: WindowPick,
    vars: &'v HashMap<String, String>,
}

/// One click step with placeholders already substituted.
#[derive(Clone, Copy)]
struct ClickSpec<'a> {
    at: Option<[f64; 2]>,
    find: Option<&'a str>,
    check: Option<&'a str>,
    count: u32,
}

/// 0–1000 normalized → screenshot pixels → screen points.
pub fn norm_to_point(n: NormPoint, shot: &Screenshot) -> (f64, f64) {
    let px = n.x / 1000.0 * f64::from(shot.width_px);
    let py = n.y / 1000.0 * f64::from(shot.height_px);
    (
        shot.origin.0 + px / shot.scale,
        shot.origin.1 + py / shot.scale,
    )
}

/// The app's biggest on-screen window with a title, else biggest on-screen,
/// else biggest.
/// The on-screen window whose title contains `needle` (case-insensitive),
/// biggest first. For targeting a specific window (e.g. "Settings") whose
/// id is not known ahead of time.
pub fn pick_window_titled(b: &dyn DesktopBackend, bundle: &str, needle: &str) -> Result<u32> {
    let apps = b.apps(std::slice::from_ref(&bundle.to_string()))?;
    let a = apps
        .into_iter()
        .find(|a| a.bundle_id == bundle)
        .ok_or_else(|| Error::not_found(format!("{bundle} is not running")))?;
    let ndl = needle.to_lowercase();
    let mut ws: Vec<_> = a
        .windows
        .into_iter()
        .filter(|w| w.title.to_lowercase().contains(&ndl))
        .collect();
    ws.sort_by(|x, y| {
        y.on_screen.cmp(&x.on_screen).then(
            (y.width * y.height)
                .partial_cmp(&(x.width * x.height))
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    ws.into_iter()
        .next()
        .map(|w| w.id)
        .ok_or_else(|| Error::not_found(format!("no {bundle} window titled {needle:?}")))
}

pub fn pick_window(b: &dyn DesktopBackend, bundle: &str) -> Result<u32> {
    let apps = b.apps(std::slice::from_ref(&bundle.to_string()))?;
    let a = apps
        .into_iter()
        .find(|a| a.bundle_id == bundle)
        .ok_or_else(|| Error::not_found(format!("{bundle} is not running")))?;
    let mut ws = a.windows;
    ws.sort_by(|x, y| {
        y.on_screen
            .cmp(&x.on_screen)
            .then((!y.title.is_empty()).cmp(&(!x.title.is_empty())))
            .then(
                (y.width * y.height)
                    .partial_cmp(&(x.width * x.height))
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    ws.into_iter()
        .next()
        .map(|w| w.id)
        .ok_or_else(|| Error::not_found(format!("no window for {bundle}")))
}

pub fn subst(s: &str, vars: &HashMap<String, String>) -> String {
    let mut out = s.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

fn contains_ci(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(&needle.to_lowercase())
}

pub fn is_yes(answer: &str) -> bool {
    let a = answer.trim().to_lowercase();
    let head = a.chars().take(12).collect::<String>();
    (head.starts_with("yes") || head.starts_with('是')) && !head.contains("no")
        || (a.contains("yes") && !a.contains("no"))
}

fn step_name(s: &Step) -> String {
    match s {
        Step::Activate {} => "activate".into(),
        Step::Click { at, find, .. } => format!(
            "click{}{}",
            at.map(|[x, y]| format!(" at({x},{y})")).unwrap_or_default(),
            find.as_ref()
                .map(|f| format!(" find({f})"))
                .unwrap_or_default()
        ),
        Step::Type { text } => format!("type({text})"),
        Step::PasteFile { path } => format!("paste_file({path})"),
        Step::Key { keys } => format!("key({keys})"),
        Step::Wait { ms } => format!("wait({ms})"),
        Step::Verify { ask, .. } => format!("verify({ask})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yes_detection() {
        assert!(is_yes("Yes"));
        assert!(is_yes("yes, the title is Kai"));
        assert!(is_yes("是"));
        assert!(!is_yes("No"));
        assert!(!is_yes("no, it shows a group chat"));
    }

    #[test]
    fn substitution_and_step_json() {
        let vars: HashMap<String, String> = [("contact".to_string(), "Kai".to_string())].into();
        assert_eq!(subst("hi {{contact}}!", &vars), "hi Kai!");
        let s: Vec<Step> = serde_json::from_str(
            r#"[{"activate":{}},{"click":{"at":[1,2],"find":"x"}},{"wait":{"ms":5}},{"verify":{"ask":"q","expect":"yes"}}]"#,
        )
        .unwrap();
        assert_eq!(s.len(), 4);
        assert_eq!(
            serde_json::from_str::<WindowPick>("918").unwrap(),
            WindowPick::Id(918)
        );
        assert_eq!(
            serde_json::from_str::<WindowPick>("\"largest\"").unwrap(),
            WindowPick::default()
        );
    }
}
