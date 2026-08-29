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
//! Each subcommand tree is filled in as its crate is ported/implemented.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "praxis", about = "AI computer-control actuators: browser + desktop + filesystem + shell")]
struct Cli {
    #[command(subcommand)]
    command: Domain,
}

#[derive(Subcommand)]
enum Domain {
    /// Drive Chrome over CDP (was `adb`).
    Browser,
    /// Drive desktop apps: windows, AX tree, input, screenshots (was `adc`).
    Desktop,
    /// Filesystem operations.
    Fs,
    /// Run a shell command (gated by the integrator; off by default).
    Shell,
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let todo = |domain: &str, crate_note: &str| {
        praxis_core::print_result::<()>(Err(praxis_core::Error::Other(format!(
            "praxis {domain}: not yet implemented — {crate_note}"
        ))))
    };
    match cli.command {
        Domain::Browser => todo("browser", praxis_browser::placeholder()),
        Domain::Desktop => todo("desktop", praxis_desktop::placeholder()),
        Domain::Fs => todo("fs", praxis_fs::placeholder()),
        Domain::Shell => todo("shell", praxis_shell::placeholder()),
    }
}
