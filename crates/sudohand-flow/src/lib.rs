//! Generic cross-actuator workflows.
//!
//! A [`Workflow`] is a Rust-defined sequence of [`Step`]s; each step runs one
//! *basic action* — any `suh` subcommand: `fs read`, `shell run`,
//! `desktop locate`, `browser page_goto`, even another extension's command —
//! and can `bind` its JSON result into a variable that later steps reach with
//! `{{name}}` / `{{name.field}}`. Steps run through a [`Dispatch`]
//! (`CliDispatch` execs `$SUH_BIN`; tests inject a fake), so the engine links
//! no actuator crate and speaks only the CLI/JSON contract every actuator and
//! extension already follows.
//!
//! Unlike the desktop/browser flows (one actuator, VLM self-heal), a workflow
//! here composes *across* actuators — the orchestration a single actuator's
//! flow deliberately does not do. VLM element location is just another
//! action (`desktop locate --find …` → a point) chained by variable.
//!
//! ```no_run
//! use sudohand_flow::{Workflow, Step, Runner, CliDispatch, Vars};
//! let wf = Workflow::new("relogin")
//!     .var("app")
//!     .step(Step::run("quit", ["shell", "run", "--", "osascript", "-e", "quit app \"WeChat\""]))
//!     .step(Step::run("relaunch", ["shell", "run", "--", "open", "-b", "{{app}}"]))
//!     .step(Step::run("front", ["desktop", "activate", "{{app}}"]));
//! let report = Runner::new(CliDispatch::default())
//!     .run(&wf, Vars::from_pairs([("app", "com.tencent.xinWeChat")]))?;
//! # Ok::<(), sudohand_core::Error>(())
//! ```

#![deny(unsafe_code)]

mod dispatch;
mod prompt;
mod registry;
mod runner;
mod step;
mod vars;

pub use dispatch::{CliDispatch, Dispatch};
pub use prompt::{Prompter, Stdio};
pub use registry::{Entry, Registry, WorkflowFn};
pub use runner::{Report, Runner, StepReport};
pub use step::{Action, Step, Workflow};
pub use sudohand_core::{Error, Result};
pub use vars::Vars;
