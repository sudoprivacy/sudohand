//! `praxis` — umbrella CLI dispatching to the actuators.
//!
//! One binary, domain subcommands (no cryptic `adb`/`adc` names, and
//! no clash with Android's `adb`):
//!
//! ```text
//! praxis browser ...     # was adb / ai-dev-browser
//! praxis desktop ...     # was adc / ai-desktop-control
//! praxis fs ...
//! praxis shell ...
//! ```
//!
//! JSON on stdout; failures print `{"error":{"kind","message"}}` to stderr
//! and exit 1 (see `praxis_core::print_result`). Each subcommand tree is
//! filled in as its crate is ported/implemented.

#![deny(unsafe_code)]

mod desktop;
mod fs;
mod shell;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "praxis",
    version,
    about = "AI computer-control actuators: browser + desktop + filesystem + shell"
)]
struct Cli {
    #[command(subcommand)]
    command: Domain,
}

#[derive(Subcommand)]
enum Domain {
    /// Drive Chrome over CDP (was `adb`).
    Browser,
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
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Domain::Browser => {
            praxis_core::print_result::<()>(Err(praxis_core::Error::internal(format!(
                "praxis browser: not yet implemented — {}",
                praxis_browser::placeholder()
            ))))
        }
        Domain::Desktop { cmd } => praxis_core::print_result(desktop::run(cmd)),
        Domain::Fs { cmd } => praxis_core::print_result(fs::run(cmd)),
        Domain::Shell { cmd } => praxis_core::print_result(shell::run(cmd)),
    }
}
