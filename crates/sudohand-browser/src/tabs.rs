//! `tab_new` / `tab_list` / `tab_switch` / `tab_close`. Port of `core/tabs.py`.

use serde_json::{json, Value};

use crate::connection::BrowserClient;
use crate::{Error, Result};

/// `{tabs: [{id, url, title, active}], count}`. `active` marks index 0 —
/// CDP has no focus notion, same as the reference.
pub async fn tab_list(browser: &mut BrowserClient) -> Result<Value> {
    browser.update_targets().await?;
    let tabs: Vec<Value> = browser
        .page_targets()
        .iter()
        .enumerate()
        .map(|(i, t)| json!({"id": i, "url": t.url, "title": t.title, "active": i == 0}))
        .collect();
    Ok(json!({"tabs": tabs, "count": tabs.len()}))
}

/// Activate tab `tab_id` (index into [`tab_list`]). Returns `{url, title}`.
pub async fn tab_switch(browser: &mut BrowserClient, tab_id: usize) -> Result<Value> {
    browser.update_targets().await?;
    let pages: Vec<_> = browser.page_targets().into_iter().cloned().collect();
    let Some(info) = pages.get(tab_id) else {
        return Err(Error::Invalid(format!(
            "Invalid tab ID: {tab_id}. Available: 0-{}",
            pages.len().saturating_sub(1)
        )));
    };
    let tab = browser.tab(info).await?;
    tab.activate().await?;
    Ok(json!({"url": info.url, "title": info.title}))
}

/// Open a new tab at `url` (default `about:blank`). `{url, title, tab_id}`.
pub async fn tab_new(browser: &mut BrowserClient, url: Option<&str>) -> Result<Value> {
    let url = url.unwrap_or("about:blank");
    let tab = browser.new_tab(url).await?;
    browser.update_targets().await?;
    let tab_id = browser
        .page_targets()
        .iter()
        .position(|t| t.target_id == tab.target.target_id);
    Ok(json!({"url": url, "title": tab.target.title, "tab_id": tab_id}))
}

/// Close tab `tab_id` (default: the first tab). Refuses to close the last
/// tab. `{closed, remaining}`.
pub async fn tab_close(browser: &mut BrowserClient, tab_id: Option<usize>) -> Result<Value> {
    browser.update_targets().await?;
    let pages: Vec<_> = browser.page_targets().into_iter().cloned().collect();
    if pages.len() <= 1 {
        return Err(Error::Invalid("Cannot close the last tab".to_string()));
    }
    let idx = tab_id.unwrap_or(0);
    let Some(info) = pages.get(idx) else {
        return Err(Error::Invalid(format!(
            "Invalid tab ID: {idx}. Available: 0-{}",
            pages.len() - 1
        )));
    };
    browser
        .connection()
        .send(
            chromiumoxide_cdp::cdp::browser_protocol::target::CloseTargetParams::new(
                info.target_id.clone(),
            ),
        )
        .await?;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    browser.update_targets().await?;
    Ok(json!({"closed": true, "remaining": browser.page_targets().len()}))
}
