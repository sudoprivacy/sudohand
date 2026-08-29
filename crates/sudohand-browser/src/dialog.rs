//! `dialog_respond` — port of `core/dialog.py`.

use std::sync::Arc;
use std::time::Duration;

use chromiumoxide_cdp::cdp::browser_protocol::page::{EnableParams, HandleJavaScriptDialogParams};
use serde_json::{json, Value};

use crate::connection::Tab;
use crate::{Error, Result};

/// Handle an open dialog now. `Ok(false)` when none is showing.
async fn handle_dialog(tab: &Tab, accept: bool, prompt_text: Option<&str>) -> Result<bool> {
    let p = HandleJavaScriptDialogParams {
        accept,
        prompt_text: prompt_text.map(str::to_string),
    };
    match tab.send(p).await {
        Ok(_) => Ok(true),
        Err(e) if e.to_string().contains("No dialog is showing") => Ok(false),
        Err(e) => Err(e),
    }
}

/// Options for [`dialog_respond`].
#[derive(Debug, Clone, Default)]
pub struct DialogOptions {
    /// `accept` (OK / submit) or `dismiss` (Cancel).
    pub action: String,
    /// Text for `prompt()` when accepting.
    pub prompt_text: Option<String>,
    /// Register an auto-handler for all future dialogs on this tab.
    pub auto_handle: bool,
    /// Block up to this many seconds for a dialog to appear.
    pub wait_timeout: f64,
}

/// Explicit control over `alert()` / `confirm()` / `prompt()` / `beforeunload`.
/// `{success, action}` or `{success: false, error, message}`.
pub async fn dialog_respond(tab: &Tab, opts: &DialogOptions) -> Result<Value> {
    let accept = opts.action != "dismiss";
    let done = if accept { "accepted" } else { "dismissed" };

    if opts.auto_handle {
        let setup = async {
            tab.send(EnableParams::default()).await?;
            let conn = Arc::clone(tab.connection());
            let mut rx = conn.subscribe();
            tokio::spawn(async move {
                while let Ok(ev) = rx.recv().await {
                    if ev.method == "Page.javascriptDialogOpening" {
                        let _ = conn.send(HandleJavaScriptDialogParams::new(accept)).await;
                    }
                }
            });
            Ok::<(), Error>(())
        };
        return Ok(match setup.await {
            Ok(()) => json!({"success": true, "action": "auto_handler_enabled"}),
            Err(e) => json!({"success": false, "error": "setup_failed", "message": e.to_string()}),
        });
    }

    if opts.wait_timeout > 0.0 {
        let mut elapsed = 0.0;
        let interval = 0.2;
        while elapsed < opts.wait_timeout {
            match handle_dialog(tab, accept, opts.prompt_text.as_deref()).await {
                Ok(true) => return Ok(json!({"success": true, "action": done})),
                Ok(false) => {}
                Err(e) => {
                    return Ok(
                        json!({"success": false, "error": "unknown", "message": e.to_string()}),
                    );
                }
            }
            tokio::time::sleep(Duration::from_secs_f64(interval)).await;
            elapsed += interval;
        }
        return Ok(json!({
            "success": false,
            "error": "timeout",
            "message": format!("No dialog appeared within {}s", opts.wait_timeout),
        }));
    }

    Ok(
        match handle_dialog(tab, accept, opts.prompt_text.as_deref()).await {
            Ok(true) => json!({"success": true, "action": done}),
            Ok(false) => json!({
                "success": false,
                "error": "no_dialog",
                "message": "No dialog is currently showing",
            }),
            Err(e) => json!({"success": false, "error": "unknown", "message": e.to_string()}),
        },
    )
}
