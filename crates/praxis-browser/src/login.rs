//! `login_interactive` — human-in-the-loop login. Port of `core/login.py`.
//!
//! Opens a windowed Chrome on a dedicated profile, restores any saved
//! cookies, navigates to the login URL, waits for the user to log in and
//! close the window, then re-opens the same profile headless to export the
//! now-authenticated cookies to `cookies.dat`.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::browser::{browser_start, browser_stop, Reuse, StartOptions};
use crate::chrome::Headless;
use crate::connection::{connect_browser, get_active_tab};
use crate::cookies::{cookies_load, cookies_save};
use crate::port::is_port_in_use;
use crate::{Error, Result};

const LOGIN_PROFILE: &str = "login";

fn port_of(v: &Value) -> Result<u16> {
    v.get("port")
        .and_then(Value::as_u64)
        .map(|p| p as u16)
        .ok_or_else(|| {
            Error::Invalid(
                v.get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("browser_start returned no port")
                    .to_string(),
            )
        })
}

/// Open a visible browser for manual login on the `login` profile; on window
/// close, export cookies. Returns `{success, cookies_saved, cookies_path}`.
pub async fn login_interactive(url: &str, cookies_path: Option<&Path>) -> Result<Value> {
    // 1. Windowed Chrome on the dedicated login profile.
    let start = browser_start(&StartOptions {
        headless: Some(Headless::Off),
        profile: Some(LOGIN_PROFILE.to_string()),
        reuse: Reuse::None,
        ..StartOptions::default()
    })
    .await?;
    let port = port_of(&start)?;

    eprintln!("\n{}", "=".repeat(60));
    eprintln!("MANUAL LOGIN REQUIRED");
    eprintln!("{}", "=".repeat(60));
    eprintln!("  URL:  {url}");
    eprintln!("  Port: {port}");
    eprintln!("\n  1. Log in manually in the window that opened");
    eprintln!("  2. Close the browser when done — cookies are saved automatically");
    eprintln!("{}\n", "=".repeat(60));

    // 2. Restore prior cookies, navigate.
    let mut browser = connect_browser(None, Some(port)).await?;
    let tab = get_active_tab(&mut browser, None).await?;
    let _ = cookies_load(&tab, cookies_path).await;
    tab.navigate(url).await?;

    // 3. Wait for the window to close (tab heartbeat stops responding).
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if tab.evaluate("1").await.is_err() {
            break;
        }
    }
    // Wait for the process to fully exit.
    for _ in 0..20 {
        if !is_port_in_use(port) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    eprintln!("Browser closed. Saving cookies...");

    // 4. Headless Chrome on the SAME profile to export the cookies.
    let start2 = browser_start(&StartOptions {
        headless: Some(Headless::New),
        profile: Some(LOGIN_PROFILE.to_string()),
        reuse: Reuse::None,
        ..StartOptions::default()
    })
    .await?;
    let port2 = match port_of(&start2) {
        Ok(p) => p,
        Err(e) => {
            return Ok(json!({"success": true, "cookies_saved": false, "error": e.to_string()}));
        }
    };
    let export = async {
        let mut b2 = connect_browser(None, Some(port2)).await?;
        let tab2 = get_active_tab(&mut b2, None).await?;
        cookies_save(&tab2, cookies_path, None).await
    }
    .await;
    let _ = browser_stop(Some(port2), false).await;

    match export {
        Ok(saved) => {
            let path = saved.get("path").and_then(Value::as_str).unwrap_or("");
            eprintln!("Cookies saved to: {path}");
            Ok(json!({
                "success": true,
                "cookies_saved": saved.get("saved") == Some(&json!(true)),
                "cookies_path": path,
            }))
        }
        Err(e) => Ok(json!({"success": true, "cookies_saved": false, "error": e.to_string()})),
    }
}
