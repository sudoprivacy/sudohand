//! `sudohand-browser` — the browser actuator, ported from
//! [ai-dev-browser](https://github.com/joezhoujinjing/ai-dev-browser) (`adb`).
//!
//! Drives a locally installed Chrome / Chromium / Edge over the DevTools
//! Protocol and exposes the runtime subset of ai-dev-browser's tools as a
//! library API ([`tools`]); the CLI lives in `sudohand-cli`
//! (`suh browser <tool> [flags]`). Tool names, flags, JSON shapes and
//! the `ref` grammar are unchanged from `adb`.
//!
//! Module boundaries that are deliberately independent of CDP:
//! - [`refs`] — the `ref` grammar (`5#214`, `FRAME_ABC:5#214`, legacy `5`).
//! - [`geometry`] — screenshot scaling metadata and image⇄CSS coordinate maps.
//!
//! Workflow orchestration is not here — cross-actuator react workflows live
//! in `sudohand-flow` and drive these tools via the CLI.
//!
//! The crate keeps its own rich [`Error`] (CDP protocol / timeout / JS
//! evaluation details); it converts into `sudohand_core::Error` for the
//! shared `{"error":{"kind","message"}}` CLI contract.

#![forbid(unsafe_code)]
#![warn(clippy::pedantic)]
#![allow(
    clippy::module_name_repetitions,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::too_many_lines,
    clippy::struct_excessive_bools,
    clippy::doc_markdown
)]

pub mod actions;
pub mod bridge;
pub mod browser;
pub mod cdp;
pub mod cdp_send;
pub mod chrome;
pub mod cleanup;
pub mod config;
pub mod connection;
pub mod cookies;
pub mod cookies_import;
pub mod dialog;
pub mod download;
#[cfg(target_os = "windows")]
mod dpapi;
pub mod element;
pub mod elements;
pub mod error;
pub mod extension;
mod fill;
pub mod geometry;
pub mod human;
mod identity;
pub mod image_cap;
pub mod js;
pub mod login;
pub mod mouse;
pub mod page;
pub mod port;
pub mod refs;
mod registry;
pub mod robust_click;
pub mod snapshot;
pub mod sqlite;
pub mod storage;
pub mod tabs;
pub mod text_match;
pub mod window;

pub use error::{Error, Result};

/// The tool surface, re-exported by name so `sudohand_browser::tools::page_discover`
/// mirrors `ai_dev_browser.core.page_discover`.
pub mod tools {
    pub use crate::actions::{
        click_by_ref, click_by_text, drag_by_ref, focus_by_ref, highlight_by_ref, hover_by_ref,
        html_by_ref, press_key, screenshot_by_ref, select_by_ref, type_by_ref, upload_by_ref,
    };
    pub use crate::browser::{browser_connect, browser_list, browser_start, browser_stop};
    pub use crate::cdp_send::cdp_send;
    pub use crate::cleanup::{browser_cleanup, list_chromes, CleanupScope};
    pub use crate::cookies::{cookies_extract_live, cookies_list, cookies_load, cookies_save};
    pub use crate::cookies_import::{cookies_extract, cookies_extract_offline, cookies_import};
    pub use crate::dialog::dialog_respond;
    pub use crate::download::{download, download_link};
    pub use crate::elements::{
        click_by_html_id, click_by_xpath, click_row_by_text, find_by_html_id, find_by_text,
        find_by_xpath, page_scroll, page_wait_element, select_text, type_by_text,
    };
    pub use crate::login::login_interactive;
    pub use crate::mouse::{mouse_click, mouse_drag, mouse_move};
    pub use crate::page::{
        js_evaluate, page_goto, page_html, page_info, page_pdf, page_reload, page_screenshot,
        page_wait_ready, page_wait_url,
    };
    pub use crate::snapshot::page_discover;
    pub use crate::storage::{storage_get, storage_set};
    pub use crate::tabs::{tab_close, tab_list, tab_new, tab_switch};
    pub use crate::window::{page_emulate_focus, window_set};
}
