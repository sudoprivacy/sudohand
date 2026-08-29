//! `sudohand-ext` — write a `suh` extension in Rust.
//!
//! An extension is an executable `suh-<name>` that `suh <name> …` execs
//! (see `sudohand-cli/src/ext.rs`). This crate turns "an executable that
//! honours the contract" into "a type that implements [`Extension`]":
//!
//! ```ignore
//! use sudohand_ext::prelude::*;
//!
//! #[derive(Subcommand)]
//! enum Cmd {
//!     /// Is the app running?
//!     Status,
//!     /// Send a message.
//!     Send { #[arg(long)] to: String, #[arg(long)] message: String },
//! }
//!
//! struct Wx;
//! impl Extension for Wx {
//!     type Cmd = Cmd;
//!     const NAME: &'static str = "wx";
//!     const ABOUT: &'static str = "WeChat operations";
//!     const PLATFORMS: &'static [Platform] = &[Platform::MacOs];
//!     fn requires() -> Requires { Requires::new().apps(["com.tencent.xinWeChat"]) }
//!     fn readonly() -> &'static [&'static str] { &["status"] }
//!     fn workflows(r: &mut Registry) {
//!         r.register_fn("wechat-send", "Send one message", &["contact", "message"], "wx", flows::send);
//!     }
//!     fn run(ctx: &Ctx, cmd: Cmd) -> Result<Value> {
//!         match cmd {
//!             Cmd::Send { to, message } => ctx.flow::<Wx>("wechat-send", vars([("contact", to), ("message", message)]), None, None),
//!             …
//!         }
//!     }
//! }
//! sudohand_ext::main!(Wx);
//! ```
//!
//! What [`run`] (and so `main!`) does for you — this *is* the process
//! contract every extension must honour, Rust or not:
//!
//! - `--manifest` prints the [`manifest::Manifest`] JSON (exit 0) without
//!   touching any backend; `--help` / `--version` behave as usual (exit 0).
//! - argv is parsed with clap; a parse error becomes
//!   `{"error":{"kind":"invalid_input",…}}` on stderr with exit 1 — never
//!   clap's bare text and exit 2.
//! - on a platform not in `PLATFORMS`, every command fails with an `io`
//!   error instead of panicking.
//! - the command's `Result<Value>` is printed with
//!   [`sudohand_core::print_result`]: JSON on stdout / error envelope on
//!   stderr.
//! - two subcommands come for free: `workflows` (list what
//!   [`Extension::workflows`] registered, plus the desktop built-ins) and
//!   `flow <name> --var k=v …` (run one by name) — so any registered
//!   workflow is runnable and discoverable without a wrapper command.
//!
//! Workflows are Rust: graphs of the fundamental desktop actions
//! ([`sudohand_desktop::workflow::Step`]) built with
//! [`sudohand_desktop::flow`] and registered by name in the [`Registry`].
//!
//! [`check`] is the black-box verifier of that contract (`suh ext check`
//! and each extension's own conformance test use it).

#![deny(unsafe_code)]

pub mod check;
pub mod ctx;
pub mod manifest;

pub use ctx::Ctx;
pub use manifest::{Manifest, Requires};
pub use sudohand_core::{Error, Result};
pub use sudohand_desktop::registry::Registry;

use clap::{CommandFactory, FromArgMatches};
use serde_json::Value;
use std::collections::HashMap;
use std::ffi::OsString;
use std::process::ExitCode;

/// Everything you can import to write an extension.
pub mod prelude {
    pub use crate::{vars, Ctx, Error, Extension, Platform, Registry, Requires, Result};
    pub use clap::{Args, Subcommand, ValueEnum};
    pub use serde_json::{json, Value};
    pub use sudohand_desktop::workflow::{Runner, Step, WindowPick};
    pub use sudohand_desktop::DesktopBackend;
    pub use sudohand_fs::FsBackend;
    pub use sudohand_shell::ShellBackend;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    MacOs,
    Linux,
    Windows,
}

impl Platform {
    pub const ALL: &'static [Platform] = &[Platform::MacOs, Platform::Linux, Platform::Windows];

    pub fn current() -> Option<Platform> {
        if cfg!(target_os = "macos") {
            Some(Platform::MacOs)
        } else if cfg!(target_os = "linux") {
            Some(Platform::Linux)
        } else if cfg!(target_os = "windows") {
            Some(Platform::Windows)
        } else {
            None
        }
    }

    fn label(self) -> &'static str {
        match self {
            Platform::MacOs => "macOS",
            Platform::Linux => "Linux",
            Platform::Windows => "Windows",
        }
    }
}

/// One `suh-<NAME>` executable.
pub trait Extension {
    /// The subcommand tree: one clap `Subcommand` enum, one level deep,
    /// verbs as variant names (`Status`, `Open`, `Send`).
    type Cmd: clap::Subcommand;

    /// What follows `suh-` in the file name and `suh ` on the command line.
    const NAME: &'static str;
    /// One line, shown by `--help` and in the manifest.
    const ABOUT: &'static str;
    /// Where it runs; elsewhere every command fails with an `io` error.
    const PLATFORMS: &'static [Platform] = Platform::ALL;
    /// Reported by `--version` and in the manifest. Set it to
    /// `env!("CARGO_PKG_VERSION")`.
    const VERSION: &'static str = "0.0.0";

    /// OS permissions, environment variables and apps the extension needs.
    fn requires() -> Requires {
        Requires::default()
    }

    /// Subcommand names (kebab-case, as typed) that do not change anything;
    /// everything else is reported as `mutates: true` in the manifest.
    fn readonly() -> &'static [&'static str] {
        &[]
    }

    /// Register this extension's workflows — Rust graphs over the desktop
    /// actions, see [`sudohand_desktop::flow`]. Use `Self::NAME` as the
    /// `source`. They show up in `workflows`, run via `flow <name>` and
    /// [`Ctx::flow`], and are listed in the manifest.
    fn workflows(_registry: &mut Registry) {}

    /// Desktop built-ins plus [`Extension::workflows`].
    fn registry() -> Registry {
        let mut r = Registry::builtins();
        Self::workflows(&mut r);
        r
    }

    /// Execute one parsed command.
    fn run(ctx: &Ctx, cmd: Self::Cmd) -> Result<Value>;

    /// The clap command tree (`suh <NAME> <cmd> …`), used for parsing, help
    /// and the manifest.
    fn command() -> clap::Command {
        let bin = format!("suh {}", Self::NAME);
        let base = clap::Command::new(bin.clone())
            .bin_name(bin)
            .about(Self::ABOUT)
            .version(Self::VERSION)
            .subcommand_required(true)
            .arg_required_else_help(true);
        <Self::Cmd as clap::Subcommand>::augment_subcommands(base)
            .subcommand(
                clap::Command::new(WORKFLOWS)
                    .about("List the registered workflows (name, description, variables, source)"),
            )
            .subcommand(
                clap::Command::new(FLOW)
                    .about("Run a registered workflow by name")
                    .arg(clap::Arg::new("name").required(true).help("Workflow name"))
                    .arg(
                        clap::Arg::new("var")
                            .long("var")
                            .action(clap::ArgAction::Append)
                            .value_name("NAME=VALUE")
                            .help("Substituted into {{name}} placeholders"),
                    )
                    .arg(
                        clap::Arg::new("model")
                            .long("model")
                            .help("Grounding (locate) model"),
                    )
                    .arg(
                        clap::Arg::new("ask_model")
                            .long("ask-model")
                            .help("Verification (ask) model"),
                    ),
            )
    }

    /// `readonly()` plus the built-in `workflows`.
    fn readonly_all() -> Vec<&'static str> {
        let mut v = Self::readonly().to_vec();
        v.push(WORKFLOWS);
        v
    }

    fn manifest() -> Manifest {
        Manifest::from_command(
            Self::NAME,
            Self::VERSION,
            Self::PLATFORMS,
            Self::requires(),
            &Self::readonly_all(),
            &Self::command(),
            Self::registry().list(),
        )
    }
}

/// The whole `main`: parse `std::env::args_os()`, dispatch, print, exit.
pub fn run<E: Extension>() -> ExitCode {
    run_from::<E>(std::env::args_os(), &Ctx::from_env(E::requires()))
}

/// [`run`] over an explicit argv and context (tests use it with a fake
/// [`Ctx`]). `argv[0]` is the program name, as in `std::env::args_os()`.
pub fn run_from<E: Extension>(argv: impl IntoIterator<Item = OsString>, ctx: &Ctx) -> ExitCode {
    let argv: Vec<OsString> = argv.into_iter().collect();
    if argv.get(1).is_some_and(|a| a == "--manifest") {
        return sudohand_core::print_result(Ok(E::manifest()));
    }
    let parsed = match parse::<E>(argv) {
        Ok(Some(p)) => p,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => return sudohand_core::print_result::<()>(Err(e)),
    };
    sudohand_core::print_result(dispatch::<E>(ctx, parsed))
}

const WORKFLOWS: &str = "workflows";
const FLOW: &str = "flow";

/// A parsed invocation: the extension's own command, or one of the two
/// built-in workflow subcommands.
pub enum Parsed<C> {
    User(C),
    Workflows,
    Flow {
        name: String,
        vars: HashMap<String, String>,
        locate: Option<String>,
        ask: Option<String>,
    },
}

/// Parse argv. `Ok(None)` means help/version was printed.
fn parse<E: Extension>(argv: Vec<OsString>) -> Result<Option<Parsed<E::Cmd>>> {
    let mut command = E::command();
    let matches = match command.try_get_matches_from_mut(argv) {
        Ok(m) => m,
        Err(e) => {
            use clap::error::ErrorKind as K;
            return match e.kind() {
                K::DisplayHelp
                | K::DisplayVersion
                | K::DisplayHelpOnMissingArgumentOrSubcommand => {
                    // clap renders help/version itself; both are exit 0.
                    let _ = e.print();
                    Ok(None)
                }
                _ => Err(Error::invalid(clap_message(&e))),
            };
        }
    };
    match matches.subcommand() {
        Some((WORKFLOWS, _)) => Ok(Some(Parsed::Workflows)),
        Some((FLOW, m)) => {
            let name = m.get_one::<String>("name").cloned().unwrap_or_default();
            let vars = parse_vars(m.get_many::<String>("var").into_iter().flatten())?;
            Ok(Some(Parsed::Flow {
                name,
                vars,
                locate: m.get_one::<String>("model").cloned(),
                ask: m.get_one::<String>("ask_model").cloned(),
            }))
        }
        _ => E::Cmd::from_arg_matches(&matches)
            .map(|c| Some(Parsed::User(c)))
            .map_err(|e| Error::invalid(clap_message(&e))),
    }
}

/// `name=value` pairs → map. Exposed for extensions that take `--var`
/// themselves.
pub fn parse_vars<'a>(
    pairs: impl IntoIterator<Item = &'a String>,
) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for v in pairs {
        let (k, val) = v
            .split_once('=')
            .ok_or_else(|| Error::invalid(format!("--var {v:?}: expected name=value")))?;
        map.insert(k.to_string(), val.to_string());
    }
    Ok(map)
}

/// Build a vars map: `vars([("contact", to), ("message", msg)])`.
pub fn vars<K: Into<String>, V: Into<String>>(
    pairs: impl IntoIterator<Item = (K, V)>,
) -> HashMap<String, String> {
    pairs
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
}

/// Clap's message without ANSI styling or the trailing usage block, folded
/// onto one line an agent can read back: `the following required arguments
/// were not provided: --message <MESSAGE>`.
fn clap_message(e: &clap::Error) -> String {
    let full = e.render().to_string();
    let words: Vec<&str> = full
        .lines()
        .map(str::trim)
        .take_while(|l| !l.starts_with("Usage:") && !l.starts_with("For more information"))
        .filter(|l| !l.is_empty())
        .collect();
    let msg = words.join(" ");
    let msg = msg.strip_prefix("error: ").unwrap_or(&msg).trim();
    if msg.is_empty() {
        "invalid arguments".into()
    } else {
        msg.to_string()
    }
}

/// Platform gate, then the extension's command or a built-in one.
pub fn dispatch<E: Extension>(ctx: &Ctx, parsed: Parsed<E::Cmd>) -> Result<Value> {
    if !ctx.ignore_platform() {
        let here = Platform::current();
        if !here.is_some_and(|p| E::PLATFORMS.contains(&p)) {
            let want: Vec<_> = E::PLATFORMS.iter().map(|p| p.label()).collect();
            return Err(Error::io(format!(
                "suh {} only runs on {}",
                E::NAME,
                want.join(" / ")
            )));
        }
    }
    match parsed {
        Parsed::User(cmd) => E::run(ctx, cmd),
        Parsed::Workflows => Ok(serde_json::json!({ "workflows": E::registry().list() })),
        Parsed::Flow {
            name,
            vars,
            locate,
            ask,
        } => ctx.flow::<E>(&name, vars, locate.as_deref(), ask.as_deref()),
    }
}

/// `fn main()` for an [`Extension`] type.
#[macro_export]
macro_rules! main {
    ($ext:ty) => {
        fn main() -> ::std::process::ExitCode {
            $crate::run::<$ext>()
        }
    };
}

/// In-process test driver: parse `args` (without the program name) and run
/// on `ctx`, returning the command's result instead of printing it.
pub fn test_run<E: Extension>(ctx: &Ctx, args: &[&str]) -> Result<Value> {
    let argv = std::iter::once(OsString::from(format!("suh-{}", E::NAME)))
        .chain(args.iter().map(OsString::from))
        .collect();
    match parse::<E>(argv)? {
        Some(p) => dispatch::<E>(ctx, p),
        None => Ok(Value::Null),
    }
}

// Keep the trait bound usable through clap's derive without users naming it.
#[doc(hidden)]
pub fn __assert_factory<T: CommandFactory>() {}
