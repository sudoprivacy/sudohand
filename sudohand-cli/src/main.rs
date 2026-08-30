//! `suh` — umbrella CLI dispatching to the actuators.
//!
//! One binary, domain subcommands (no cryptic `adb`/`adc` names, and
//! no clash with Android's `adb`):
//!
//! ```text
//! suh browser ...     # was adb / ai-dev-browser
//! suh desktop ...     # was adc / ai-desktop-control
//! suh fs ...
//! suh shell ...
//! suh ext list        # installed extensions (install|uninstall|update|check|info)
//! suh <name> ...      # extension: runs `suh-<name>` (see `ext`)
//! ```
//!
//! JSON on stdout; failures print `{"error":{"kind","message"}}` to stderr
//! and exit 1 (see `sudohand_core::print_result`). Each subcommand tree is
//! filled in as its crate is ported/implemented.

#![deny(unsafe_code)]

mod browser;
mod desktop;
mod ext;
mod ext_install;
mod fs;
mod serve;
mod shell;

use clap::{Parser, Subcommand};
use std::ffi::OsString;

#[derive(Parser)]
#[command(
    name = "suh",
    version,
    about = "AI computer-control actuators: browser + desktop + filesystem + shell",
    after_help = "Extensions: `suh <name> …` runs the executable `suh-<name>` found on \
                  $SUH_EXT_PATH, ~/.suh/extensions, next to this binary, or $PATH \
                  (`suh ext list`)."
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    command: Domain,
}

#[derive(Subcommand)]
enum Domain {
    /// Drive Chrome over CDP (was `adb`).
    Browser {
        #[command(subcommand)]
        cmd: browser::Cmd,
    },
    /// Drive desktop apps: windows, AX tree, input, screenshots (was `adc`).
    Desktop {
        #[command(subcommand)]
        cmd: desktop::Cmd,
    },
    /// Filesystem operations.
    Fs {
        #[command(subcommand)]
        cmd: fs::Cmd,
    },
    /// Run a shell command (gated by the integrator; off by default).
    Shell {
        #[command(subcommand)]
        cmd: shell::Cmd,
    },
    /// Extensions: list / resolve the `suh-<name>` executables.
    Ext {
        #[command(subcommand)]
        cmd: ext::Cmd,
    },
    /// Print the command tree (domain, action, description) as TSV, plus the
    /// exit-code contract — the machine-readable API for an agent.
    Describe,
    /// Expose the actuators over a WebSocket. Agents — a remote apeiron pod or
    /// a local sudocode — connect here as clients; suh still runs on this
    /// machine. One action per frame; `shell` and the meta commands are refused.
    Serve {
        /// Address to bind. Localhost-only by default; widen deliberately.
        #[arg(long, default_value = "127.0.0.1:8787")]
        bind: String,
        /// If set, the client's first frame must be
        /// `{"type":"auth","token":"…"}` matching this value.
        #[arg(long)]
        token: Option<String>,
    },
    /// `suh <name> …` → run the `suh-<name>` extension.
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

/// The command tree as TSV: one line per `domain action`, with the action's
/// one-line description. Followed by the exit-code contract. This is the
/// agent-facing API — stable, greppable, no prose.
fn describe() -> String {
    use clap::CommandFactory;
    use std::fmt::Write as _;
    let root = Cli::command();
    let mut out = String::new();
    let _ = writeln!(out, "schema\tsuh.describe.v1");
    let _ = writeln!(out, "kind\tdomain\taction\tdescription");
    for domain in root.get_subcommands() {
        let dname = domain.get_name();
        if dname == "help" {
            continue;
        }
        let subs: Vec<_> = domain
            .get_subcommands()
            .filter(|c| c.get_name() != "help")
            .collect();
        if subs.is_empty() {
            let about = domain
                .get_about()
                .map(|s| s.to_string())
                .unwrap_or_default();
            let _ = writeln!(out, "cmd\t{dname}\t\t{}", one_line(&about));
        } else {
            for a in subs {
                let about = a.get_about().map(|s| s.to_string()).unwrap_or_default();
                let _ = writeln!(
                    out,
                    "action\t{dname}\t{}\t{}",
                    a.get_name(),
                    one_line(&about)
                );
            }
        }
    }
    out.push('\n');
    let _ = writeln!(
        out,
        "# extensions: `suh <name> ...` runs the suh-<name> executable; see `suh ext list`."
    );
    let _ = writeln!(
        out,
        "# exit: 0=ok 2=invalid_input(fix args) 4=not_found 7=permission_denied(grant/sudo) 9=io/transient(retry) 1=internal(bug)"
    );
    let _ = writeln!(
        out,
        "# output: JSON on stdout, error envelope on stderr; extension list commands default to TSV (--format json for the JSON shape)."
    );
    out
}

fn one_line(s: &str) -> String {
    s.lines().next().unwrap_or("").replace('\t', " ")
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Domain::Browser { cmd } => sudohand_core::print_result(browser::run(cmd)),
        Domain::Desktop { cmd } => sudohand_core::print_result(desktop::run(cmd)),
        Domain::Fs { cmd } => sudohand_core::print_result(fs::run(cmd)),
        Domain::Shell { cmd } => sudohand_core::print_result(shell::run(cmd)),
        Domain::Ext { cmd } => sudohand_core::print_result(ext::run_cmd(cmd)),
        Domain::Describe => {
            print!("{}", describe());
            std::process::ExitCode::SUCCESS
        }
        Domain::Serve { bind, token } => serve::run(bind, token),
        Domain::External(argv) => {
            let (name, args) = argv.split_first().expect("clap gives at least the name");
            ext::exec(&name.to_string_lossy(), args)
        }
    }
}
