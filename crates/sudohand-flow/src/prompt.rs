//! Asking the person running the workflow. Like [`Dispatch`](crate::Dispatch)
//! for actions, [`Prompter`] is injected so interactive steps stay testable
//! and the engine keeps no hard dependency on a terminal.

use std::io::Write;
use sudohand_core::{Error, Result};

pub trait Prompter: Send + Sync {
    /// Read a line; `default` is used when the reply is empty.
    fn line(&self, message: &str, default: Option<&str>) -> Result<String>;
    /// Yes/no.
    fn confirm(&self, message: &str) -> Result<bool>;
    /// Pick one of `options` (already rendered); return its index.
    fn select(&self, message: &str, options: &[String]) -> Result<usize>;
}

/// Prompts on stderr, reads stdin — so stdout stays clean for the result
/// JSON.
pub struct Stdio;

impl Prompter for Stdio {
    fn line(&self, message: &str, default: Option<&str>) -> Result<String> {
        match default {
            Some(d) => eprint!("{message} [{d}]: "),
            None => eprint!("{message}: "),
        }
        let _ = std::io::stderr().flush();
        let mut s = String::new();
        std::io::stdin()
            .read_line(&mut s)
            .map_err(|e| Error::io(format!("read stdin: {e}")))?;
        let s = s.trim().to_string();
        Ok(if s.is_empty() {
            default.unwrap_or("").to_string()
        } else {
            s
        })
    }

    fn confirm(&self, message: &str) -> Result<bool> {
        let a = self.line(&format!("{message} [y/N]"), Some("n"))?;
        Ok(a.eq_ignore_ascii_case("y") || a.eq_ignore_ascii_case("yes"))
    }

    fn select(&self, message: &str, options: &[String]) -> Result<usize> {
        eprintln!("{message}");
        for (i, o) in options.iter().enumerate() {
            eprintln!("  [{i}] {o}");
        }
        let a = self.line(
            &format!("pick [0-{}]", options.len().saturating_sub(1)),
            Some("0"),
        )?;
        let idx: usize = a
            .parse()
            .map_err(|_| Error::invalid(format!("not a number: {a:?}")))?;
        if idx >= options.len() {
            return Err(Error::invalid("selection out of range"));
        }
        Ok(idx)
    }
}
