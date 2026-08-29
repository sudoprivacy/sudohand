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
struct Cli {
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
    /// `suh <name> …` → run the `suh-<name>` extension.
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Domain::Browser { cmd } => sudohand_core::print_result(browser::run(cmd)),
        Domain::Desktop { cmd } => sudohand_core::print_result(desktop::run(cmd)),
        Domain::Fs { cmd } => sudohand_core::print_result(fs::run(cmd)),
        Domain::Shell { cmd } => sudohand_core::print_result(shell::run(cmd)),
        Domain::Ext { cmd } => sudohand_core::print_result(ext::run_cmd(cmd)),
        Domain::External(argv) => {
            let (name, args) = argv.split_first().expect("clap gives at least the name");
            ext::exec(&name.to_string_lossy(), args)
        }
    }
}
