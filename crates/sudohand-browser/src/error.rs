//! One error type for the whole crate.

use std::fmt;

/// Crate-wide error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Could not reach Chrome's debug endpoint or the WebSocket died.
    #[error("connection error: {0}")]
    Connection(String),
    /// Chrome answered a CDP command with an error object.
    #[error("CDP error{}: {message} [code: {code}]", method_suffix(.method))]
    Protocol {
        /// CDP method that failed, when known.
        method: String,
        /// Chrome's error code.
        code: i64,
        /// Chrome's error message.
        message: String,
    },
    /// A CDP command did not answer within its timeout.
    #[error("CDP command timed out after {seconds}s: {method}")]
    Timeout {
        /// CDP method that hung.
        method: String,
        /// Timeout that elapsed.
        seconds: f64,
    },
    /// Page-side JavaScript threw, or returned an opaque value.
    #[error("{0}")]
    JsEvaluation(JsEvaluationError),
    /// Chrome could not be found or launched.
    #[error("{0}")]
    Chrome(String),
    /// Bad argument (invalid ref, unknown key, bad env value, …).
    #[error("{0}")]
    Invalid(String),
    /// Anything I/O.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialization.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// Image encode/decode.
    #[error("image error: {0}")]
    Image(String),
}

fn method_suffix(method: &str) -> String {
    if method.is_empty() {
        String::new()
    } else {
        format!(" ({method})")
    }
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Map onto the shared sudohand error categories for the CLI contract.
impl From<Error> for sudohand_core::Error {
    fn from(e: Error) -> Self {
        use sudohand_core::Error as C;
        match &e {
            Error::Invalid(m) => C::InvalidInput(m.clone()),
            Error::Chrome(m) => C::NotFound(m.clone()),
            Error::Io(io) => match io.kind() {
                std::io::ErrorKind::NotFound => C::NotFound(e.to_string()),
                std::io::ErrorKind::PermissionDenied => C::PermissionDenied(e.to_string()),
                _ => C::Io(e.to_string()),
            },
            Error::Json(_) => C::Internal(e.to_string()),
            Error::Connection(_)
            | Error::Protocol { .. }
            | Error::Timeout { .. }
            | Error::JsEvaluation(_)
            | Error::Image(_) => C::Io(e.to_string()),
        }
    }
}

/// JavaScript run in the page did not produce a value.
///
/// Mirrors `ai_dev_browser.core.errors.JsEvaluationError`: the message,
/// a single-line snippet of the expression, and any console lines emitted
/// before the throw.
#[derive(Debug, Clone, Default)]
pub struct JsEvaluationError {
    /// Exception description (usually `Error: <msg>` plus the JS stack).
    pub message: String,
    /// The expression that ran.
    pub expression: Option<String>,
    /// Console entries emitted during the eval, `{level, text}`.
    pub console: Vec<ConsoleEntry>,
}

/// One captured `console.*` call.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ConsoleEntry {
    /// `log` / `warn` / `error` / `info` / …
    pub level: String,
    /// Space-joined stringified arguments.
    pub text: String,
}

const SNIPPET_MAX: usize = 120;

/// Collapse an expression to a single-line, length-capped identifier.
#[must_use]
pub fn js_snippet(expression: &str) -> String {
    let flat = expression.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > SNIPPET_MAX {
        let head: String = flat.chars().take(SNIPPET_MAX - 1).collect();
        format!("{head}…")
    } else {
        flat
    }
}

impl fmt::Display for JsEvaluationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(expr) = self.expression.as_deref().filter(|e| !e.is_empty()) {
            write!(f, "\n  expression: {}", js_snippet(expr))?;
        }
        for entry in &self.console {
            write!(f, "\n  console.{}: {}", entry.level, entry.text)?;
        }
        Ok(())
    }
}

impl From<JsEvaluationError> for Error {
    fn from(e: JsEvaluationError) -> Self {
        Error::JsEvaluation(e)
    }
}
