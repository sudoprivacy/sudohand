//! Shared SDK/CLI decision text and operation-specific recovery instructions.
//! Keep help in the browser crate so Clap help and `suh describe` cannot drift
//! from the SDK docs for these tools. Coverage expands with each migrated family.

pub const CLICK_BY_HTML_ID: &str = include_str!("../help/click_by_html_id.md");
pub const CLICK_BY_XPATH: &str = include_str!("../help/click_by_xpath.md");
pub const JS_EVALUATE: &str = include_str!("../help/js_evaluate.md");
pub const PAGE_DISCOVER: &str = include_str!("../help/page_discover.md");
pub const WINDOW_SET: &str = include_str!("../help/window_set.md");
pub const MOUSE_DRAG: &str = include_str!("../help/mouse_drag.md");
pub const DOWNLOAD: &str = include_str!("../help/download.md");
pub const DOWNLOAD_FAILURE: &str = include_str!("../help/failures/download.md");
pub const LOCATOR_FAILURE: &str = include_str!("../help/failures/locator.md");
pub const EVALUATION_FAILURE: &str = include_str!("../help/failures/evaluation.md");
