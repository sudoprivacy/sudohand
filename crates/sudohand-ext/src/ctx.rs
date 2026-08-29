//! Runtime context handed to [`Extension::run`](crate::Extension::run):
//! lazy access to the actuator backends. Real backends by default; fake
//! ones when `SUH_FAKE=1` is set or the context was built with
//! [`Ctx::fake`] — so an extension's tests run the same code path without
//! touching the machine.

use crate::manifest::Requires;
use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use sudohand_desktop::vlm::Vlm;
use sudohand_desktop::DesktopBackend;
use sudohand_fs::FsBackend;
use sudohand_shell::ShellBackend;

pub struct Ctx {
    fake: Option<Fakes>,
    suh_bin: Option<PathBuf>,
    requires: Requires,
}

/// The fake backends used in fake mode; shared so a test can inspect
/// their recorded actions after a run.
#[derive(Clone)]
pub struct Fakes {
    pub desktop: Arc<sudohand_desktop::FakeBackend>,
    pub fs: Arc<sudohand_fs::FakeFs>,
    pub shell: Arc<sudohand_shell::FakeShell>,
}

impl Ctx {
    /// Real backends; fake ones if `SUH_FAKE=1`.
    pub fn from_env(requires: Requires) -> Self {
        let fake = std::env::var("SUH_FAKE").is_ok_and(|v| v == "1");
        let suh_bin = std::env::var_os("SUH_BIN").map(PathBuf::from);
        let mut ctx = Self {
            fake: None,
            suh_bin,
            requires,
        };
        if fake {
            ctx.fake = Some(ctx.default_fakes());
        }
        ctx
    }

    /// Fake backends; the desktop fake reports `requires.apps` as running.
    pub fn fake(requires: Requires) -> Self {
        let mut ctx = Self {
            fake: None,
            suh_bin: None,
            requires,
        };
        ctx.fake = Some(ctx.default_fakes());
        ctx
    }

    /// Fake mode with backends you built yourself.
    pub fn with_fakes(fakes: Fakes) -> Self {
        Self {
            fake: Some(fakes),
            suh_bin: None,
            requires: Requires::default(),
        }
    }

    fn default_fakes(&self) -> Fakes {
        let apps: Vec<&str> = self.requires.apps.iter().map(String::as_str).collect();
        Fakes {
            desktop: sudohand_desktop::FakeBackend::with_running(&apps),
            fs: sudohand_fs::FakeFs::new(),
            shell: sudohand_shell::FakeShell::new(),
        }
    }

    pub fn is_fake(&self) -> bool {
        self.fake.is_some()
    }

    /// The fakes, when in fake mode (tests inspect `.actions()`).
    pub fn fakes(&self) -> Option<&Fakes> {
        self.fake.as_ref()
    }

    /// Fake mode skips the platform gate so tests run anywhere.
    pub(crate) fn ignore_platform(&self) -> bool {
        self.is_fake()
    }

    /// Path of the `suh` that invoked us (`SUH_BIN`), for shelling out to
    /// `suh browser …` and friends.
    pub fn suh_bin(&self) -> Option<&Path> {
        self.suh_bin.as_deref()
    }

    /// A [`Dispatch`](sudohand_flow::Dispatch) that runs generic-workflow
    /// steps by exec'ing the calling `suh` (`SUH_BIN`, else `suh` on PATH).
    /// This is the cross-actuator path: a step's `["desktop","locate",…]`
    /// becomes `suh desktop locate …`.
    pub fn dispatch(&self) -> sudohand_flow::CliDispatch {
        match &self.suh_bin {
            Some(b) => sudohand_flow::CliDispatch { bin: b.clone() },
            None => sudohand_flow::CliDispatch::default(),
        }
    }

    /// Run a generic cross-actuator [`Workflow`](sudohand_flow::Workflow)
    /// through [`Ctx::dispatch`].
    pub fn run_workflow(
        &self,
        wf: &sudohand_flow::Workflow,
        vars: sudohand_flow::Vars,
    ) -> Result<sudohand_flow::Report> {
        sudohand_flow::Runner::new(self.dispatch()).run(wf, vars)
    }

    pub fn requires(&self) -> &Requires {
        &self.requires
    }

    pub fn desktop(&self) -> Result<Arc<dyn DesktopBackend>> {
        if let Some(f) = &self.fake {
            return Ok(f.desktop.clone());
        }
        real_desktop()
    }

    pub fn fs(&self) -> Arc<dyn FsBackend> {
        match &self.fake {
            Some(f) => f.fs.clone(),
            None => Arc::new(sudohand_fs::RealFs::new()),
        }
    }

    pub fn shell(&self) -> Arc<dyn ShellBackend> {
        match &self.fake {
            Some(f) => f.shell.clone(),
            None => Arc::new(sudohand_shell::RealShell::with_signal_forwarding()),
        }
    }

    /// DashScope VLM from `DASHSCOPE_API_KEY` / `~/.bailian/config.json`.
    /// `locate` / `ask` override the default models. Fake mode has no VLM:
    /// every call fails with `invalid_input`.
    pub fn vlm(&self, locate: Option<&str>, ask: Option<&str>) -> Result<Arc<dyn Vlm>> {
        if self.is_fake() {
            return Ok(Arc::new(NoVlm));
        }
        let mut v = sudohand_desktop::vlm::DashScopeVlm::from_env()?;
        if let Some(m) = locate {
            v.locate_model = m.into();
        }
        if let Some(m) = ask {
            v.ask_model = m.into();
        }
        Ok(Arc::new(v))
    }
}

impl Ctx {
    /// Resolve one of the extension's registered generic workflows by name
    /// and run it through [`Ctx::run_workflow`] (actions dispatch via the
    /// calling `suh`; interactive steps prompt on stdin). Unknown names are
    /// `not_found`, missing `--var`s `invalid_input`.
    pub fn flow<E: crate::Extension>(
        &self,
        name: &str,
        vars: std::collections::HashMap<String, String>,
    ) -> Result<serde_json::Value> {
        let wf = E::registry().resolve(name)?;
        let report = self.run_workflow(&wf, sudohand_flow::Vars(to_json_map(vars)))?;
        Ok(serde_json::json!({
            "ok": report.ok, "cancelled": report.cancelled,
            "workflow": name, "steps": report.steps, "vars": report.vars,
        }))
    }
}

fn to_json_map(
    m: std::collections::HashMap<String, String>,
) -> std::collections::HashMap<String, serde_json::Value> {
    m.into_iter()
        .map(|(k, v)| (k, serde_json::Value::String(v)))
        .collect()
}

#[cfg(target_os = "macos")]
fn real_desktop() -> Result<Arc<dyn DesktopBackend>> {
    Ok(Arc::new(sudohand_desktop::MacBackend::new()))
}

#[cfg(not(target_os = "macos"))]
fn real_desktop() -> Result<Arc<dyn DesktopBackend>> {
    Err(Error::io("the desktop actuator only runs on macOS"))
}

struct NoVlm;

impl Vlm for NoVlm {
    fn locate(&self, _: &[u8], d: &str) -> Result<sudohand_desktop::vlm::NormPoint> {
        Err(Error::invalid(format!(
            "no VLM in fake mode (locate {d:?})"
        )))
    }
    fn ask(&self, _: &[u8], q: &str) -> Result<String> {
        Err(Error::invalid(format!("no VLM in fake mode (ask {q:?})")))
    }
}
