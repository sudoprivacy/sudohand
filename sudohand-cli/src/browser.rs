//! `suh browser <tool> [flags]` — thin CLI over `sudohand-browser`, one
//! subcommand per tool, names identical to adb / the Python
//! `ai_dev_browser.tools.<name>` modules (kebab-case aliases too).
//! Output is JSON on stdout; failures use the sudohand error envelope.

#![allow(clippy::struct_excessive_bools, clippy::too_many_lines)]

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use serde_json::{json, Value};
use sudohand_browser::actions::TypeOptions;
use sudohand_browser::browser::{Reuse, StartOptions};
use sudohand_browser::chrome::Headless;
use sudohand_browser::connection::{get_active_tab, BrowserClient, Tab};
use sudohand_browser::dialog::DialogOptions;
use sudohand_browser::elements::{ScrollOptions, TypeByTextOptions};
use sudohand_browser::image_cap::ImageCap;
use sudohand_browser::mouse::ClickOptions;
use sudohand_browser::page::{PdfOptions, ScreenshotOptions};
use sudohand_browser::snapshot::DiscoverOptions;

/// Connection-scope flags every tab-taking tool accepts.
#[derive(Args, Debug, Clone)]
pub struct Conn {
    /// Browser transport (or AI_DEV_BROWSER_TRANSPORT)
    #[arg(long, value_parser = ["cdp", "extension"])]
    transport: Option<String>,
    /// Chrome debugging port (auto-detects: AI_DEV_BROWSER_PORT → workspace scan → 9350)
    #[arg(short, long)]
    port: Option<u16>,
    /// Act on the tab whose URL contains this substring (or AI_DEV_BROWSER_TAB_URL)
    #[arg(long)]
    tab_url: Option<String>,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum ReuseArg {
    None,
    Any,
}

// Flatten small parser groups: deriving all browser commands in one enum used
// almost 2 MiB of stack in debug builds, exceeding Windows and Tokio stacks.
#[derive(Subcommand, Debug)]
pub enum Cmd {
    #[command(flatten)]
    Browser(BrowserCommands),
    #[command(flatten)]
    Navigation(NavigationCommands),
    #[command(flatten)]
    PageOutput(PageOutputCommands),
    #[command(flatten)]
    Reference(ReferenceCommands),
    #[command(flatten)]
    Locator(LocatorCommands),
    #[command(flatten)]
    Mouse(MouseCommands),
    #[command(flatten)]
    Tab(TabCommands),
    #[command(flatten)]
    Runtime(RuntimeCommands),
    #[command(flatten)]
    Cookie(CookieCommands),
    #[command(flatten)]
    Download(DownloadCommands),
    #[command(flatten)]
    Vlm(VlmCommands),
}

#[derive(Subcommand, Debug)]
pub enum BrowserCommands {
    /// Check an existing browser connection without launching Chrome
    #[command(name = "browser_connect", alias = "browser-connect")]
    BrowserConnect {
        #[arg(long, value_parser = ["cdp", "extension"])]
        transport: Option<String>,
        #[arg(long)]
        port: Option<u16>,
    },
    /// Disconnect the extension bridge without closing the user's Chrome
    #[command(name = "browser_disconnect", alias = "browser-disconnect")]
    BrowserDisconnect,
    /// Internal persistent extension bridge
    #[command(name = "bridge-serve", hide = true)]
    BridgeServe {
        #[arg(long, default_value_t = sudohand_browser::bridge::PORT)]
        port: u16,
    },
    /// Start a browser instance — isolated (temp profile) unless --profile is given
    #[command(name = "browser_start", alias = "browser-start")]
    BrowserStart {
        /// Debug port (auto-assigned if omitted)
        #[arg(long)]
        port: Option<u16>,
        /// Headless mode: `new` (default when flag given), `old`, `1`/`true`; falls back to AI_DEV_BROWSER_HEADLESS
        #[arg(long, num_args = 0..=1, default_missing_value = "new")]
        headless: Option<String>,
        /// Initial URL (default about:blank)
        #[arg(long)]
        url: Option<String>,
        /// Named persistent profile (login survives, same-profile calls reuse)
        #[arg(long)]
        profile: Option<String>,
        /// Force a temporary profile
        #[arg(long)]
        temp: bool,
        /// Reuse strategy for a named profile
        #[arg(long, value_enum, default_value_t = ReuseArg::Any)]
        reuse: ReuseArg,
        /// Seconds to wait for the debug port
        #[arg(long, default_value_t = 30.0)]
        startup_timeout: f64,
        /// Extra Chrome flags appended after the defaults
        #[arg(long, num_args = 0..)]
        extra_args: Vec<String>,
        /// Override/remove default flags as JSON: '{"--flag": null, "--other": "v"}'
        #[arg(long)]
        override_default_args: Option<String>,
        /// Route Chrome's stderr to null
        #[arg(long)]
        silent_stderr: bool,
        /// Restore legacy automation marker flags (stealth is on by default)
        #[arg(long)]
        no_stealth: bool,
        /// Override the browser timezone (for example Asia/Tokyo)
        #[arg(long)]
        timezone: Option<String>,
        /// Override geolocation as latitude,longitude
        #[arg(long, allow_hyphen_values = true)]
        geo: Option<String>,
        /// Explicit locale override (language is never inferred from a proxy)
        #[arg(long)]
        locale: Option<String>,
        /// Derive location through Chrome; defaults on when a proxy is configured
        #[arg(long, num_args = 0..=1, default_missing_value = "true", value_parser = clap::builder::BoolishValueParser::new(), conflicts_with = "no_match_proxy")]
        match_proxy: Option<bool>,
        /// Disable proxy location lookup
        #[arg(long)]
        no_match_proxy: bool,
    },
    /// Stop browser instance(s)
    #[command(name = "browser_stop", alias = "browser-stop")]
    BrowserStop {
        /// Port of the browser to stop
        #[arg(long)]
        port: Option<u16>,
        /// Stop every registered Chrome started by this tool
        #[arg(long)]
        stop_all: bool,
    },
    /// Clean up managed orphan browsers only; scope is required
    #[command(name = "browser_cleanup", alias = "browser-cleanup")]
    BrowserCleanup {
        #[arg(long, value_parser = ["temp", "profile", "workspace"])]
        scope: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// List all Chrome instances with managed/external classification
    #[command(name = "browser_list", alias = "browser-list")]
    BrowserList {
        /// Show Chromes from all workspaces
        #[arg(long)]
        all_workspaces: bool,
    },
}

#[derive(Subcommand, Debug)]
#[allow(clippy::enum_variant_names)] // Keep names aligned with the existing CLI actions.
pub enum NavigationCommands {
    /// Navigate to a URL
    #[command(name = "page_goto", alias = "page-goto")]
    PageGoto {
        #[command(flatten)]
        conn: Conn,
        /// URL to load
        #[arg(long)]
        url: String,
        /// Open in a new tab
        #[arg(long)]
        tab_new: bool,
        /// Don't wait for document.readyState === "complete"
        #[arg(long = "no-wait", action = clap::ArgAction::SetFalse)]
        wait: bool,
    },
    /// Discover interactable elements (accessibility tree + DOM scan) with refs
    #[command(name = "page_discover", alias = "page-discover")]
    PageDiscover {
        #[command(flatten)]
        conn: Conn,
        /// Case-insensitive substring filter on the element name
        #[arg(long)]
        text: Option<String>,
        /// Include non-interactive nodes too
        #[arg(long = "no-interactable-only", action = clap::ArgAction::SetFalse)]
        interactable_only: bool,
        /// Skip x/y/box
        #[arg(long = "no-include-coordinates", action = clap::ArgAction::SetFalse)]
        include_coordinates: bool,
        /// Skip same-origin iframes
        #[arg(long = "no-include-iframes", action = clap::ArgAction::SetFalse)]
        include_iframes: bool,
        /// Skip the DOM scan (pure accessibility-tree results)
        #[arg(long = "no-dom-scan", action = clap::ArgAction::SetFalse)]
        dom_scan: bool,
        /// Max DOM-scanned elements
        #[arg(long, default_value_t = 200)]
        dom_limit: usize,
    },
    /// Capture a screenshot scaled for LLM vision, with coordinate metadata embedded
    #[command(name = "page_screenshot", alias = "page-screenshot")]
    PageScreenshot {
        #[command(flatten)]
        conn: Conn,
        /// Output path (default $AI_DEV_BROWSER_OUTPUT_DIR or ./output/{timestamp}.png)
        #[arg(long)]
        path: Option<PathBuf>,
        /// Capture beyond the viewport
        #[arg(long)]
        full_page: bool,
        /// Don't rescale to CSS pixels / caps
        #[arg(long = "no-css-scale", action = clap::ArgAction::SetFalse)]
        css_scale: bool,
        /// Long-edge cap in px (0 disables)
        #[arg(long, default_value_t = sudohand_browser::geometry::MAX_SCREENSHOT_LONG_EDGE)]
        max_long_edge: u32,
        /// Total-pixel cap (0 disables)
        #[arg(long, default_value_t = sudohand_browser::geometry::MAX_SCREENSHOT_TOTAL_PIXELS)]
        max_total_pixels: u64,
        /// Per-call cap as JSON: '{"max_bytes": 200000, "max_dimension": 1024}' (JPEG when max_bytes)
        #[arg(long)]
        image_cap: Option<String>,
    },
    /// Current URL / title / readyState without a discover
    #[command(name = "page_info", alias = "page-info")]
    PageInfo {
        #[command(flatten)]
        conn: Conn,
    },
    /// Reload the page
    #[command(name = "page_reload", alias = "page-reload")]
    PageReload {
        #[command(flatten)]
        conn: Conn,
        /// Use the cache
        #[arg(long = "no-ignore-cache", action = clap::ArgAction::SetFalse)]
        ignore_cache: bool,
    },
    /// Wait until the URL matches (substring / regex, or exact)
    #[command(name = "page_wait_url", alias = "page-wait-url")]
    PageWaitUrl {
        #[command(flatten)]
        conn: Conn,
        /// URL substring or regex
        #[arg(long)]
        pattern: Option<String>,
        /// Exact URL
        #[arg(long)]
        exact: Option<String>,
        /// Max seconds to wait
        #[arg(long, default_value_t = 30.0)]
        timeout: f64,
    },
    /// Wait for an element to be visible (CSS selector or visible text) and return its ref
    #[command(name = "page_wait_element", alias = "page-wait-element")]
    PageWaitElement {
        #[command(flatten)]
        conn: Conn,
        /// Visible text (top frame)
        #[arg(long)]
        text: Option<String>,
        /// CSS selector (ARIA-less / datarole controls)
        #[arg(long)]
        selector: Option<String>,
        /// Max seconds to wait
        #[arg(long, default_value_t = 30.0)]
        timeout: f64,
    },
    /// Scroll: incremental gesture, to top/bottom of the real scroller, or to an element by text
    #[command(name = "page_scroll", alias = "page-scroll")]
    PageScroll {
        #[command(flatten)]
        conn: Conn,
        /// up / down
        #[arg(long, default_value = "down")]
        direction: String,
        /// Percent of the viewport height
        #[arg(long, default_value_t = 25)]
        amount: i64,
        /// Scroll the scrollable container to its bottom
        #[arg(long)]
        to_bottom: bool,
        /// Scroll the scrollable container to its top
        #[arg(long)]
        to_top: bool,
        /// Visible text of an element to scroll into view
        #[arg(long)]
        to_element: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
#[allow(clippy::enum_variant_names)] // Keep names aligned with the existing CLI actions.
pub enum PageOutputCommands {
    /// Print the page to PDF (headless only)
    #[command(name = "page_pdf", alias = "page-pdf")]
    PagePdf {
        #[command(flatten)]
        conn: Conn,
        /// Output path (default $AI_DEV_BROWSER_OUTPUT_DIR or ./output/{timestamp}.pdf)
        #[arg(long)]
        path: Option<PathBuf>,
        /// Landscape orientation
        #[arg(long)]
        landscape: bool,
        /// Skip background graphics
        #[arg(long = "no-print-background", action = clap::ArgAction::SetFalse)]
        print_background: bool,
        /// Rendering scale
        #[arg(long, default_value_t = 1.0)]
        scale: f64,
        /// Paper width in inches
        #[arg(long, default_value_t = 8.5)]
        paper_width: f64,
        /// Paper height in inches
        #[arg(long, default_value_t = 11.0)]
        paper_height: f64,
        /// Top margin in inches
        #[arg(long, default_value_t = 0.0)]
        margin_top: f64,
        /// Bottom margin in inches
        #[arg(long, default_value_t = 0.0)]
        margin_bottom: f64,
        /// Left margin in inches
        #[arg(long, default_value_t = 0.0)]
        margin_left: f64,
        /// Right margin in inches
        #[arg(long, default_value_t = 0.0)]
        margin_right: f64,
        /// Page ranges, e.g. "1-5" or "1,3,5-9"
        #[arg(long, default_value = "")]
        page_ranges: String,
    },
    /// Make the page behave as if its window were focused
    #[command(name = "page_emulate_focus", alias = "page-emulate-focus")]
    PageEmulateFocus {
        #[command(flatten)]
        conn: Conn,
        /// Disable focus emulation
        #[arg(long = "no-enabled", action = clap::ArgAction::SetFalse)]
        enabled: bool,
    },
    /// Wait until document.readyState === "complete"
    #[command(name = "page_wait_ready", alias = "page-wait-ready")]
    PageWaitReady {
        #[command(flatten)]
        conn: Conn,
        /// Max seconds to wait
        #[arg(long, default_value_t = 30.0)]
        timeout: f64,
        /// Extra settle time after load
        #[arg(long, default_value_t = 0.5)]
        idle_time: f64,
    },
    /// Whole-document HTML
    #[command(name = "page_html", alias = "page-html")]
    PageHtml {
        #[command(flatten)]
        conn: Conn,
        /// outerHTML of the document element instead of innerHTML
        #[arg(long)]
        outer: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ReferenceCommands {
    /// Click an element by ref from page_discover
    #[command(name = "click_by_ref", alias = "click-by-ref")]
    ClickByRef {
        #[command(flatten)]
        conn: Conn,
        /// Element ref, e.g. "5#214" or "FRAME_ABC123:5#214"
        #[arg(long = "ref")]
        r#ref: String,
        /// Bare press/release at the box centre instead of the human-like actuator
        #[arg(long = "no-human-like", action = clap::ArgAction::SetFalse)]
        human_like: bool,
        /// Allow system mouse input as the last fallback (or AI_DEV_BROWSER_OS_CLICK)
        #[arg(long, num_args = 0..=1, default_missing_value = "true", value_parser = clap::builder::BoolishValueParser::new())]
        os_click: Option<bool>,
    },
    /// Locate by accessible name / visible text and click
    #[command(name = "click_by_text", alias = "click-by-text")]
    ClickByText {
        #[command(flatten)]
        conn: Conn,
        /// Text to look for (case-insensitive substring)
        #[arg(long)]
        text: String,
        /// Seconds to wait for the text to appear
        #[arg(long, default_value_t = 10.0)]
        timeout: f64,
        /// Bare press/release at the box centre instead of the human-like actuator
        #[arg(long = "no-human-like", action = clap::ArgAction::SetFalse)]
        human_like: bool,
        /// Allow system mouse input as the last fallback (or AI_DEV_BROWSER_OS_CLICK)
        #[arg(long, num_args = 0..=1, default_missing_value = "true", value_parser = clap::builder::BoolishValueParser::new())]
        os_click: Option<bool>,
    },
    /// Type into the element a ref names
    #[command(name = "type_by_ref", alias = "type-by-ref")]
    TypeByRef {
        #[command(flatten)]
        conn: Conn,
        /// Element ref from page_discover
        #[arg(long = "ref")]
        r#ref: String,
        /// Text to type
        #[arg(long)]
        text: String,
        /// Clear existing content first
        #[arg(long)]
        clear: bool,
        /// Press Enter after typing
        #[arg(long)]
        enter: bool,
        /// Per-character key events (live filters / autocomplete)
        #[arg(long)]
        keystrokes: bool,
        /// Type character-by-character with human timing (anti-bot pages)
        #[arg(long)]
        human_like: bool,
    },
    /// Focus an element without clicking
    #[command(name = "focus_by_ref", alias = "focus-by-ref")]
    FocusByRef {
        #[command(flatten)]
        conn: Conn,
        /// Element ref from page_discover
        #[arg(long = "ref")]
        r#ref: String,
    },
    /// Move the cursor over an element (hover menus, tooltips)
    #[command(name = "hover_by_ref", alias = "hover-by-ref")]
    HoverByRef {
        #[command(flatten)]
        conn: Conn,
        /// Element ref from page_discover
        #[arg(long = "ref")]
        r#ref: String,
    },
    /// Draw a red overlay on an element for a few seconds
    #[command(name = "highlight_by_ref", alias = "highlight-by-ref")]
    HighlightByRef {
        #[command(flatten)]
        conn: Conn,
        /// Element ref from page_discover
        #[arg(long = "ref")]
        r#ref: String,
        /// Seconds to show the overlay
        #[arg(long, default_value_t = 2.0)]
        duration: f64,
    },
    /// outerHTML of one element
    #[command(name = "html_by_ref", alias = "html-by-ref")]
    HtmlByRef {
        #[command(flatten)]
        conn: Conn,
        /// Element ref from page_discover
        #[arg(long = "ref")]
        r#ref: String,
    },
    /// Screenshot just one element's box
    #[command(name = "screenshot_by_ref", alias = "screenshot-by-ref")]
    ScreenshotByRef {
        #[command(flatten)]
        conn: Conn,
        /// Element ref from page_discover
        #[arg(long = "ref")]
        r#ref: String,
        /// Output path (default $AI_DEV_BROWSER_OUTPUT_DIR or ./output/{timestamp}_element.png)
        #[arg(long)]
        path: Option<PathBuf>,
        /// Per-call cap as JSON: '{"max_bytes": 200000, "max_dimension": 1024}'
        #[arg(long)]
        image_cap: Option<String>,
    },
    /// Select an <option> inside a native <select>
    #[command(name = "select_by_ref", alias = "select-by-ref")]
    SelectByRef {
        #[command(flatten)]
        conn: Conn,
        /// Ref of the <option>
        #[arg(long = "ref")]
        r#ref: String,
    },
    /// Set files on an <input type="file">
    #[command(name = "upload_by_ref", alias = "upload-by-ref")]
    UploadByRef {
        #[command(flatten)]
        conn: Conn,
        /// Ref of the file input
        #[arg(long = "ref")]
        r#ref: String,
        /// Comma-separated absolute file paths
        #[arg(long)]
        paths: String,
    },
    /// Press a key (Enter, Tab, Escape, Backspace, Delete, Space, arrows, Home, End, PageUp/Down)
    #[command(name = "press_key", alias = "press-key")]
    PressKey {
        #[command(flatten)]
        conn: Conn,
        /// Key name (case-insensitive; aliases esc, del, return, up/down/left/right)
        #[arg(long)]
        key: String,
        /// Focus this ref first
        #[arg(long = "ref")]
        r#ref: Option<String>,
        /// Modifier bitmask: 1=Alt, 2=Ctrl, 4=Meta, 8=Shift
        #[arg(long, default_value_t = 0)]
        modifiers: i64,
    },
}

#[derive(Subcommand, Debug)]
pub enum LocatorCommands {
    /// Locate by accessible name and report {found, ref, role, name, x, y}
    #[command(name = "find_by_text", alias = "find-by-text")]
    FindByText {
        #[command(flatten)]
        conn: Conn,
        /// Text to look for (case-insensitive substring)
        #[arg(long)]
        text: String,
        /// Only match buttons / links / inputs (no StaticText fallback)
        #[arg(long)]
        interactable_only: bool,
    },
    /// {found, tag, text, visible, attrs} for an html id (same-origin frames included)
    #[command(name = "find_by_html_id", alias = "find-by-html-id")]
    FindByHtmlId {
        #[command(flatten)]
        conn: Conn,
        /// Value of the id attribute
        #[arg(long)]
        html_id: String,
    },
    /// {found, tag, text, visible, attrs} for the first XPath match (same-origin frames included)
    #[command(name = "find_by_xpath", alias = "find-by-xpath")]
    FindByXpath {
        #[command(flatten)]
        conn: Conn,
        /// XPath expression
        #[arg(long)]
        xpath: String,
    },
    /// Trusted click on the element with an html id
    #[command(name = "click_by_html_id", alias = "click-by-html-id")]
    ClickByHtmlId {
        #[command(flatten)]
        conn: Conn,
        /// Value of the id attribute
        #[arg(long)]
        html_id: String,
    },
    /// Trusted click on the first XPath match
    #[command(name = "click_by_xpath", alias = "click-by-xpath")]
    ClickByXpath {
        #[command(flatten)]
        conn: Conn,
        /// XPath expression
        #[arg(long)]
        xpath: String,
    },
    /// Click / double-click a grid row by its text, or toggle its checkbox
    #[command(name = "click_row_by_text", alias = "click-row-by-text")]
    ClickRowByText {
        #[command(flatten)]
        conn: Conn,
        /// Text the row contains
        #[arg(long)]
        text: String,
        /// Double-click the row
        #[arg(long)]
        double: bool,
        /// 0-based index when several rows match
        #[arg(long, default_value_t = 0)]
        nth: i64,
        /// Toggle the row's checkbox instead of clicking the row
        #[arg(long)]
        checkbox: bool,
    },
    /// Locate an input by its label / placeholder / accessible name and type into it
    #[command(name = "type_by_text", alias = "type-by-text")]
    TypeByText {
        #[command(flatten)]
        conn: Conn,
        /// Accessible name of the input
        #[arg(long)]
        name: String,
        /// Text to type
        #[arg(long)]
        text: String,
        /// Clear existing content first
        #[arg(long)]
        clear: bool,
        /// Seconds to wait for the name to appear
        #[arg(long, default_value_t = 10.0)]
        timeout: f64,
        /// Human timing between keystrokes
        #[arg(long, num_args = 0..=1, default_missing_value = "true", value_parser = clap::builder::BoolishValueParser::new(), conflicts_with = "no_human_like")]
        human_like: Option<bool>,
        /// Disable human timing between keystrokes
        #[arg(long)]
        no_human_like: bool,
        /// Press Enter after typing
        #[arg(long)]
        enter: bool,
        /// Real per-character key events
        #[arg(long)]
        keystrokes: bool,
    },
    /// Build a real text selection over on-page text
    #[command(name = "select_text", alias = "select-text")]
    SelectText {
        #[command(flatten)]
        conn: Conn,
        /// Text to select (case-sensitive substring of one text node)
        #[arg(long)]
        text: String,
        /// Extend the selection to the end of this text
        #[arg(long)]
        to_text: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum MouseCommands {
    /// Drag the element a ref names to destination coordinates
    #[command(name = "drag_by_ref", alias = "drag-by-ref")]
    DragByRef {
        #[command(flatten)]
        conn: Conn,
        /// Element ref to drag from
        #[arg(long = "ref")]
        r#ref: String,
        /// Destination X (CSS px)
        #[arg(long)]
        to_x: f64,
        /// Destination Y (CSS px)
        #[arg(long)]
        to_y: f64,
        /// Intermediate move steps (linear drag)
        #[arg(long, default_value_t = 10)]
        steps: usize,
        /// Human-like drag: hold, gaussian path with timing, pause, release
        #[arg(long)]
        human_like: bool,
    },
    /// Move the cursor to coordinates without clicking
    #[command(name = "mouse_move", alias = "mouse-move")]
    MouseMove {
        #[command(flatten)]
        conn: Conn,
        /// X coordinate (screenshot space if --screenshot is given)
        #[arg(long)]
        x: f64,
        /// Y coordinate
        #[arg(long)]
        y: f64,
        /// Screenshot PNG whose embedded metadata maps image px → CSS px
        #[arg(long)]
        screenshot: Option<PathBuf>,
        /// Intermediate steps (native line)
        #[arg(long, default_value_t = 10)]
        steps: usize,
        /// Straight line instead of the gaussian path
        #[arg(long = "no-human-like", action = clap::ArgAction::SetFalse)]
        human_like: bool,
    },
    /// Click at raw coordinates (canvas / SVG / screenshot-derived)
    #[command(name = "mouse_click", alias = "mouse-click")]
    MouseClick {
        #[command(flatten)]
        conn: Conn,
        /// X coordinate (screenshot space if --screenshot is given)
        #[arg(long)]
        x: f64,
        /// Y coordinate
        #[arg(long)]
        y: f64,
        /// Screenshot PNG whose embedded metadata maps image px → CSS px
        #[arg(long)]
        screenshot: Option<PathBuf>,
        /// left / right / middle
        #[arg(long, default_value = "left")]
        button: String,
        /// Modifier bitmask: 1=Alt, 2=Ctrl, 4=Meta, 8=Shift
        #[arg(long, default_value_t = 0)]
        modifiers: i64,
        /// Double-click
        #[arg(long)]
        double: bool,
        /// Bare press/release instead of the human-like actuator
        #[arg(long = "no-human-like", action = clap::ArgAction::SetFalse)]
        human_like: bool,
        /// Skip the pre-click cursor move (fewest events; heavy SPAs)
        #[arg(long = "no-move", action = clap::ArgAction::SetFalse)]
        r#move: bool,
    },
    /// Drag from one coordinate to another
    #[command(name = "mouse_drag", alias = "mouse-drag")]
    MouseDrag {
        #[command(flatten)]
        conn: Conn,
        /// Start X (screenshot space if --screenshot is given)
        #[arg(long)]
        from_x: f64,
        /// Start Y
        #[arg(long)]
        from_y: f64,
        /// End X
        #[arg(long)]
        to_x: f64,
        /// End Y
        #[arg(long)]
        to_y: f64,
        /// Screenshot PNG whose embedded metadata maps image px → CSS px
        #[arg(long)]
        screenshot: Option<PathBuf>,
        /// Intermediate move steps (linear drag)
        #[arg(long, default_value_t = 10)]
        steps: usize,
        /// Human-like drag: hold, gaussian path with timing, pause, release
        #[arg(long)]
        human_like: bool,
    },
}

#[derive(Subcommand, Debug)]
#[allow(clippy::enum_variant_names)] // Keep names aligned with the existing CLI actions.
pub enum TabCommands {
    /// List open tabs
    #[command(name = "tab_list", alias = "tab-list")]
    TabList {
        #[command(flatten)]
        conn: Conn,
    },
    /// Activate a tab by index
    #[command(name = "tab_switch", alias = "tab-switch")]
    TabSwitch {
        #[command(flatten)]
        conn: Conn,
        /// Tab index from tab_list
        #[arg(long)]
        tab_id: usize,
    },
    /// Open a new tab
    #[command(name = "tab_new", alias = "tab-new")]
    TabNew {
        #[command(flatten)]
        conn: Conn,
        /// URL to open (default about:blank)
        #[arg(long)]
        url: Option<String>,
    },
    /// Close a tab by index (never the last one)
    #[command(name = "tab_close", alias = "tab-close")]
    TabClose {
        #[command(flatten)]
        conn: Conn,
        /// Tab index from tab_list (default 0)
        #[arg(long)]
        tab_id: Option<usize>,
    },
}

#[derive(Subcommand, Debug)]
pub enum RuntimeCommands {
    /// Evaluate JavaScript in the page (raw escape hatch)
    #[command(name = "js_evaluate", alias = "js-evaluate")]
    JsEvaluate {
        #[command(flatten)]
        conn: Conn,
        /// JavaScript expression; the last expression's value is returned
        #[arg(long)]
        expression: String,
        /// Run inside a cross-origin iframe: URL substring or target id
        #[arg(long)]
        frame: Option<String>,
    },
    /// Send a raw CDP command: --method Domain.command --params '{...}'
    #[command(name = "cdp_send", alias = "cdp-send")]
    CdpSend {
        #[command(flatten)]
        conn: Conn,
        /// CDP method, e.g. Browser.getVersion
        #[arg(long)]
        method: String,
        /// JSON object of parameters (snake_case or camelCase keys)
        #[arg(long)]
        params: Option<String>,
    },
    /// Accept / dismiss a JavaScript dialog (alert / confirm / prompt / beforeunload)
    #[command(name = "dialog_respond", alias = "dialog-respond")]
    DialogRespond {
        #[command(flatten)]
        conn: Conn,
        /// accept or dismiss
        #[arg(long, default_value = "accept")]
        action: String,
        /// Text for prompt() when accepting
        #[arg(long)]
        prompt_text: Option<String>,
        /// Auto-respond to all future dialogs on this tab
        #[arg(long)]
        auto_handle: bool,
        /// Seconds to wait for a dialog to appear
        #[arg(long, default_value_t = 0.0)]
        wait_timeout: f64,
    },
    /// Set the render viewport, OS window state, or focus
    #[command(name = "window_set", alias = "window-set")]
    WindowSet {
        #[command(flatten)]
        conn: Conn,
        /// Render viewport width (innerWidth)
        #[arg(long)]
        width: Option<u32>,
        /// Render viewport height (innerHeight)
        #[arg(long)]
        height: Option<u32>,
        /// normal / maximized / minimized / fullscreen (headed only)
        #[arg(long)]
        state: Option<String>,
        /// Bring the window to front (headed only)
        #[arg(long)]
        focus: bool,
    },
    /// Read localStorage (one key or all)
    #[command(name = "storage_get", alias = "storage-get")]
    StorageGet {
        #[command(flatten)]
        conn: Conn,
        /// Key to read (default: all)
        #[arg(long)]
        key: Option<String>,
    },
    /// Write localStorage (--items JSON, or --key/--value)
    #[command(name = "storage_set", alias = "storage-set")]
    StorageSet {
        #[command(flatten)]
        conn: Conn,
        /// JSON object of key/value pairs
        #[arg(long)]
        items: Option<String>,
        /// Single key
        #[arg(long)]
        key: Option<String>,
        /// Value for --key
        #[arg(long)]
        value: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum CookieCommands {
    /// List browser cookies
    #[command(name = "cookies_list", alias = "cookies-list")]
    CookiesList {
        #[command(flatten)]
        conn: Conn,
        /// Domain substring filter
        #[arg(long)]
        domain: Option<String>,
    },
    /// Save browser cookies to a JSON file
    #[command(name = "cookies_save", alias = "cookies-save")]
    CookiesSave {
        #[command(flatten)]
        conn: Conn,
        /// File path (default ~/.ai-dev-browser/cookies.dat)
        #[arg(long)]
        path: Option<PathBuf>,
        /// Regex; only cookies whose JSON matches are saved
        #[arg(long)]
        pattern: Option<String>,
    },
    /// Load cookies from a JSON file into the browser
    #[command(name = "cookies_load", alias = "cookies-load")]
    CookiesLoad {
        #[command(flatten)]
        conn: Conn,
        /// File path (default ~/.ai-dev-browser/cookies.dat)
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Import cookies for a domain from the user's real browser (macOS/Linux)
    #[command(name = "cookies_import", alias = "cookies-import")]
    CookiesImport {
        #[command(flatten)]
        conn: Conn,
        /// Domain to import (e.g. ".grok.com")
        #[arg(long)]
        domain: String,
        /// Source browser: chrome / chromium / brave / edge
        #[arg(long, default_value = "chrome")]
        browser: String,
        /// Explicit Chrome user-data directory
        #[arg(long)]
        user_data_dir: Option<String>,
    },
    /// Open a visible browser for manual login, then export cookies on close
    #[command(name = "login_interactive", alias = "login-interactive")]
    LoginInteractive {
        /// Login page URL
        #[arg(long)]
        url: String,
        /// Where to save cookies (default ~/.ai-dev-browser/cookies.dat)
        #[arg(long)]
        cookies_path: Option<PathBuf>,
    },
    /// Extract full cookies from a running browser, including HttpOnly and session cookies
    #[command(name = "cookies_extract_live", alias = "cookies-extract-live")]
    CookiesExtractLive {
        #[command(flatten)]
        conn: Conn,
        /// Domain substring; an empty string selects all cookies
        #[arg(long)]
        domain: String,
    },
    /// Extract full cookies from the source browser's on-disk database
    #[command(name = "cookies_extract_offline", alias = "cookies-extract-offline")]
    CookiesExtractOffline {
        #[arg(long)]
        domain: String,
        #[arg(long, default_value = "chrome")]
        browser: String,
        #[arg(long)]
        user_data_dir: Option<String>,
    },
    /// Extract + decrypt cookies for a domain (no automation browser)
    #[command(name = "cookies_extract", alias = "cookies-extract")]
    CookiesExtract {
        /// Domain to read (e.g. ".grok.com")
        #[arg(long)]
        domain: String,
        /// Source browser: chrome / chromium / brave / edge
        #[arg(long, default_value = "chrome")]
        browser: String,
        /// Explicit Chrome user-data directory
        #[arg(long)]
        user_data_dir: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum DownloadCommands {
    /// Download a file by URL into a directory
    #[command(name = "download")]
    Download {
        #[command(flatten)]
        conn: Conn,
        /// Direct URL of the file
        #[arg(long)]
        url: String,
        /// Download directory (default ./downloads)
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Trusted-click a download link (XPath) and wait for the file
    #[command(name = "download_link", alias = "download-link")]
    DownloadLink {
        #[command(flatten)]
        conn: Conn,
        /// XPath of the link / button
        #[arg(long)]
        xpath: String,
        /// Directory to save into (default ./downloads)
        #[arg(long)]
        download_dir: Option<PathBuf>,
        /// Seconds to wait for completion
        #[arg(long, default_value_t = 30.0)]
        timeout: f64,
    },
}

#[derive(Subcommand, Debug)]
pub enum VlmCommands {
    /// Ask the VLM where an element is on the page; returns a clickable CSS
    /// point (usable by `mouse_click --x --y`).
    #[command(name = "locate", alias = "vlm_locate")]
    Locate {
        #[command(flatten)]
        conn: Conn,
        /// Natural-language description of the element.
        #[arg(long)]
        find: String,
        #[arg(long)]
        model: Option<String>,
    },
    /// Ask the VLM a yes/no question about the page; returns {answer, yes}.
    #[command(name = "ask", alias = "vlm_ask")]
    Ask {
        #[command(flatten)]
        conn: Conn,
        #[arg(long)]
        question: String,
        #[arg(long)]
        model: Option<String>,
    },
}

/// Screenshot the active tab to a temp file (css-scaled, capped), returning
/// the PNG bytes and the image dims + `scale_factor` (CSS px per image px).
async fn vlm_screenshot(tab: &Tab) -> sudohand_browser::Result<(Vec<u8>, u32, u32, f64)> {
    let path = std::env::temp_dir().join(format!("suh_browser_vlm_{}.png", std::process::id()));
    let opts = ScreenshotOptions {
        path: Some(path.clone()),
        full_page: false,
        css_scale: true,
        max_long_edge: 1400,
        max_total_pixels: 0,
        image_cap: None,
    };
    let meta = sudohand_browser::page::page_screenshot(tab, &opts).await?;
    let width = meta.get("width").and_then(Value::as_u64).unwrap_or(0) as u32;
    let height = meta.get("height").and_then(Value::as_u64).unwrap_or(0) as u32;
    let sf = meta
        .get("scale_factor")
        .and_then(Value::as_f64)
        .unwrap_or(1.0);
    let png = std::fs::read(&path).map_err(sudohand_browser::Error::from)?;
    let _ = std::fs::remove_file(&path);
    Ok((png, width, height, sf))
}

async fn ensure_bridge() -> sudohand_browser::Result<()> {
    use sudohand_browser::bridge;
    if bridge::status(bridge::PORT).await.is_some() {
        return Ok(());
    }
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .args(["browser", "bridge-serve"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    let mut child = command.spawn()?;
    for _ in 0..25 {
        if bridge::status(bridge::PORT).await.is_some() {
            return Ok(());
        }
        if child.try_wait()?.is_some() {
            return Err(sudohand_browser::Error::Connection(
                "extension bridge could not start; port 9522 may be occupied".into(),
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(sudohand_browser::Error::Connection(
        "extension bridge startup timed out".into(),
    ))
}

fn transport(explicit: Option<&str>) -> String {
    explicit
        .map(str::to_owned)
        .or_else(|| std::env::var("AI_DEV_BROWSER_TRANSPORT").ok())
        .unwrap_or_else(|| "cdp".into())
}

async fn connected(conn: &Conn) -> sudohand_browser::Result<BrowserClient> {
    match transport(conn.transport.as_deref()).as_str() {
        "extension" => {
            ensure_bridge().await?;
            BrowserClient::connect_extension(sudohand_browser::bridge::PORT).await
        }
        "cdp" => {
            let port = sudohand_browser::connection::resolve_port(conn.port).await;
            BrowserClient::connect("127.0.0.1", port).await
        }
        other => Err(sudohand_browser::Error::Invalid(format!(
            "Unknown browser transport: {other}"
        ))),
    }
}

async fn browser_and_tab(conn: &Conn) -> sudohand_browser::Result<(BrowserClient, Tab)> {
    let mut browser = connected(conn).await?;
    let tab = get_active_tab(&mut browser, conn.tab_url.as_deref()).await?;
    Ok((browser, tab))
}

fn parse_overrides(raw: Option<&str>) -> sudohand_browser::Result<Vec<(String, Option<String>)>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let v: Value = serde_json::from_str(raw)?;
    let obj = v.as_object().ok_or_else(|| {
        sudohand_browser::Error::Invalid(
            "--override-default-args must be a JSON object".to_string(),
        )
    })?;
    Ok(obj
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                match v {
                    Value::Null => None,
                    Value::String(s) => Some(s.clone()),
                    other => Some(other.to_string()),
                },
            )
        })
        .collect())
}

// Keep each command in a separate future. One async match placed every arm's
// polling temporaries in a ~946 KiB debug stack frame, overflowing Windows' main
// thread before even offline commands could execute.
fn run_async(
    tool: Cmd,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = sudohand_browser::Result<Value>>>> {
    match tool {
        Cmd::Browser(BrowserCommands::BridgeServe { port }) => Box::pin(async move {
            sudohand_browser::bridge::serve(port).await?;
            Ok(json!({"stopped": true}))
        }),
        Cmd::Browser(BrowserCommands::BrowserDisconnect) => Box::pin(async move {
            sudohand_browser::bridge::disconnect(sudohand_browser::bridge::PORT).await
        }),
        Cmd::Browser(BrowserCommands::BrowserConnect { transport, port }) => Box::pin(async move {
            let selected = self::transport(transport.as_deref());
            if selected == "extension" {
                ensure_bridge().await?;
                for _ in 0..20 {
                    if sudohand_browser::bridge::status(sudohand_browser::bridge::PORT)
                        .await
                        .is_some_and(|status| status["extension_connected"] == true)
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
            sudohand_browser::browser::browser_connect(Some(&selected), port).await
        }),
        Cmd::Browser(BrowserCommands::BrowserStart {
            port,
            headless,
            url,
            profile,
            temp,
            reuse,
            startup_timeout,
            extra_args,
            override_default_args,
            silent_stderr,
            no_stealth,
            timezone,
            geo,
            locale,
            match_proxy,
            no_match_proxy,
        }) => Box::pin(async move {
            let opts = StartOptions {
                port,
                headless: headless.as_deref().map(Headless::parse),
                url,
                profile,
                temp,
                reuse: match reuse {
                    ReuseArg::None => Reuse::None,
                    ReuseArg::Any => Reuse::Any,
                },
                startup_timeout: Some(startup_timeout),
                extra_args,
                override_default_args: parse_overrides(override_default_args.as_deref())?,
                silent_stderr,
                stealth: Some(!no_stealth),
                timezone,
                geo,
                locale,
                match_proxy: if no_match_proxy {
                    Some(false)
                } else {
                    match_proxy
                },
            };
            sudohand_browser::tools::browser_start(&opts).await
        }),
        Cmd::Browser(BrowserCommands::BrowserStop { port, stop_all }) => {
            Box::pin(async move { sudohand_browser::tools::browser_stop(port, stop_all).await })
        }
        Cmd::Browser(BrowserCommands::BrowserCleanup {
            scope,
            profile,
            dry_run,
        }) => Box::pin(async move {
            use sudohand_browser::cleanup::CleanupScope;
            let scope = match scope.as_str() {
                "temp" => CleanupScope::Temp,
                "profile" => CleanupScope::Profile,
                _ => CleanupScope::Workspace,
            };
            sudohand_browser::cleanup::browser_cleanup(scope, profile.as_deref(), dry_run).await
        }),
        Cmd::Browser(BrowserCommands::BrowserList { all_workspaces }) => {
            Box::pin(async move { sudohand_browser::tools::browser_list(all_workspaces).await })
        }
        Cmd::Navigation(NavigationCommands::PageGoto {
            conn,
            url,
            tab_new,
            wait,
        }) => Box::pin(async move {
            let (mut browser, tab) = browser_and_tab(&conn).await?;
            let tab = if tab_new {
                browser.new_tab("about:blank").await?
            } else {
                tab
            };
            sudohand_browser::tools::page_goto(&tab, &url, wait).await
        }),
        Cmd::Navigation(NavigationCommands::PageDiscover {
            conn,
            text,
            interactable_only,
            include_coordinates,
            include_iframes,
            dom_scan,
            dom_limit,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            let opts = DiscoverOptions {
                text,
                interactable_only,
                include_coordinates,
                include_iframes,
                dom_scan,
                dom_limit,
            };
            Ok(serde_json::to_value(
                sudohand_browser::tools::page_discover(&tab, &opts).await?,
            )?)
        }),
        Cmd::Navigation(NavigationCommands::PageScreenshot {
            conn,
            path,
            full_page,
            css_scale,
            max_long_edge,
            max_total_pixels,
            image_cap,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            let opts = ScreenshotOptions {
                path,
                full_page,
                css_scale,
                max_long_edge,
                max_total_pixels,
                image_cap: image_cap.as_deref().map(ImageCap::from_json).transpose()?,
            };
            sudohand_browser::tools::page_screenshot(&tab, &opts).await
        }),
        Cmd::Vlm(VlmCommands::Locate { conn, find, model }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            let (png, width, height, sf) = vlm_screenshot(&tab).await?;
            let mut vlm = sudohand_vlm::DashScopeVlm::from_env()
                .map_err(|e| sudohand_browser::Error::Invalid(e.to_string()))?;
            if let Some(m) = model {
                vlm.locate_model = m;
            }
            let locate_model = vlm.locate_model.clone();
            let t0 = std::time::Instant::now();
            let find2 = find.clone();
            let n =
                tokio::task::spawn_blocking(move || sudohand_vlm::Vlm::locate(&vlm, &png, &find2))
                    .await
                    .map_err(|e| sudohand_browser::Error::Invalid(format!("vlm join: {e}")))?
                    .map_err(|e| sudohand_browser::Error::Invalid(e.to_string()))?;
            let ix = n.x / 1000.0 * f64::from(width);
            let iy = n.y / 1000.0 * f64::from(height);
            Ok(json!({
                "find": find, "model": locate_model,
                "normalized": [n.x, n.y],
                "image": {"x": ix, "y": iy},
                "point": {"x": ix * sf, "y": iy * sf},
                "ms": t0.elapsed().as_millis(),
            }))
        }),
        Cmd::Vlm(VlmCommands::Ask {
            conn,
            question,
            model,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            let (png, _w, _h, _sf) = vlm_screenshot(&tab).await?;
            let mut vlm = sudohand_vlm::DashScopeVlm::from_env()
                .map_err(|e| sudohand_browser::Error::Invalid(e.to_string()))?;
            if let Some(m) = model {
                vlm.ask_model = m;
            }
            let ask_model = vlm.ask_model.clone();
            let t0 = std::time::Instant::now();
            let question2 = question.clone();
            let answer =
                tokio::task::spawn_blocking(move || sudohand_vlm::Vlm::ask(&vlm, &png, &question2))
                    .await
                    .map_err(|e| sudohand_browser::Error::Invalid(format!("vlm join: {e}")))?
                    .map_err(|e| sudohand_browser::Error::Invalid(e.to_string()))?;
            Ok(json!({
                "question": question, "answer": answer,
                "yes": sudohand_vlm::is_yes(&answer),
                "model": ask_model, "ms": t0.elapsed().as_millis(),
            }))
        }),
        Cmd::Navigation(NavigationCommands::PageInfo { conn }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::page_info(&tab).await
        }),
        Cmd::Navigation(NavigationCommands::PageReload { conn, ignore_cache }) => {
            Box::pin(async move {
                let (_b, tab) = browser_and_tab(&conn).await?;
                sudohand_browser::tools::page_reload(&tab, ignore_cache).await
            })
        }
        Cmd::Navigation(NavigationCommands::PageWaitUrl {
            conn,
            pattern,
            exact,
            timeout,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::page_wait_url(
                &tab,
                pattern.as_deref(),
                exact.as_deref(),
                timeout,
            )
            .await
        }),
        Cmd::Navigation(NavigationCommands::PageWaitElement {
            conn,
            text,
            selector,
            timeout,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::page_wait_element(
                &tab,
                text.as_deref(),
                selector.as_deref(),
                timeout,
            )
            .await
        }),
        Cmd::Navigation(NavigationCommands::PageScroll {
            conn,
            direction,
            amount,
            to_bottom,
            to_top,
            to_element,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            let opts = ScrollOptions {
                direction,
                amount,
                to_bottom,
                to_top,
                to_element,
            };
            sudohand_browser::tools::page_scroll(&tab, &opts).await
        }),
        Cmd::PageOutput(PageOutputCommands::PagePdf {
            conn,
            path,
            landscape,
            print_background,
            scale,
            paper_width,
            paper_height,
            margin_top,
            margin_bottom,
            margin_left,
            margin_right,
            page_ranges,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            let opts = PdfOptions {
                path,
                landscape,
                print_background,
                scale,
                paper_width,
                paper_height,
                margin_top,
                margin_bottom,
                margin_left,
                margin_right,
                page_ranges,
            };
            sudohand_browser::tools::page_pdf(&tab, &opts).await
        }),
        Cmd::PageOutput(PageOutputCommands::PageEmulateFocus { conn, enabled }) => {
            Box::pin(async move {
                let (_b, tab) = browser_and_tab(&conn).await?;
                sudohand_browser::tools::page_emulate_focus(&tab, enabled).await
            })
        }
        Cmd::Reference(ReferenceCommands::FocusByRef { conn, r#ref }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::focus_by_ref(&tab, &r#ref).await
        }),
        Cmd::Reference(ReferenceCommands::HoverByRef { conn, r#ref }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::hover_by_ref(&tab, &r#ref).await
        }),
        Cmd::Reference(ReferenceCommands::HighlightByRef {
            conn,
            r#ref,
            duration,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::highlight_by_ref(&tab, &r#ref, duration).await
        }),
        Cmd::Reference(ReferenceCommands::HtmlByRef { conn, r#ref }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::html_by_ref(&tab, &r#ref).await
        }),
        Cmd::Reference(ReferenceCommands::ScreenshotByRef {
            conn,
            r#ref,
            path,
            image_cap,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            let cap = image_cap.as_deref().map(ImageCap::from_json).transpose()?;
            sudohand_browser::tools::screenshot_by_ref(&tab, &r#ref, path.as_deref(), cap.as_ref())
                .await
        }),
        Cmd::Reference(ReferenceCommands::SelectByRef { conn, r#ref }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::select_by_ref(&tab, &r#ref).await
        }),
        Cmd::Reference(ReferenceCommands::UploadByRef { conn, r#ref, paths }) => {
            Box::pin(async move {
                let (_b, tab) = browser_and_tab(&conn).await?;
                sudohand_browser::tools::upload_by_ref(&tab, &r#ref, &paths).await
            })
        }
        Cmd::Reference(ReferenceCommands::PressKey {
            conn,
            key,
            r#ref,
            modifiers,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::press_key(&tab, &key, r#ref.as_deref(), modifiers).await
        }),
        Cmd::Locator(LocatorCommands::FindByText {
            conn,
            text,
            interactable_only,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::find_by_text(&tab, &text, interactable_only).await
        }),
        Cmd::Locator(LocatorCommands::FindByHtmlId { conn, html_id }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::find_by_html_id(&tab, &html_id).await
        }),
        Cmd::Locator(LocatorCommands::FindByXpath { conn, xpath }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::find_by_xpath(&tab, &xpath).await
        }),
        Cmd::Locator(LocatorCommands::ClickByHtmlId { conn, html_id }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::click_by_html_id(&tab, &html_id).await
        }),
        Cmd::Locator(LocatorCommands::ClickByXpath { conn, xpath }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::click_by_xpath(&tab, &xpath).await
        }),
        Cmd::Locator(LocatorCommands::ClickRowByText {
            conn,
            text,
            double,
            nth,
            checkbox,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::click_row_by_text(&tab, &text, double, nth, checkbox).await
        }),
        Cmd::Locator(LocatorCommands::TypeByText {
            conn,
            name,
            text,
            clear,
            timeout,
            human_like,
            no_human_like,
            enter,
            keystrokes,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            let opts = TypeByTextOptions {
                clear,
                timeout,
                human_like: if no_human_like {
                    Some(false)
                } else {
                    // The reference CLI's bool/None argument defaults to true;
                    // its SDK still defaults to the humanization config.
                    Some(human_like.unwrap_or(true))
                },
                enter,
                keystrokes,
            };
            sudohand_browser::tools::type_by_text(&tab, &name, &text, opts).await
        }),
        Cmd::Locator(LocatorCommands::SelectText {
            conn,
            text,
            to_text,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::select_text(&tab, &text, to_text.as_deref()).await
        }),
        Cmd::Tab(TabCommands::TabNew { conn, url }) => Box::pin(async move {
            let mut browser = connected(&conn).await?;
            sudohand_browser::tools::tab_new(&mut browser, url.as_deref()).await
        }),
        Cmd::Tab(TabCommands::TabClose { conn, tab_id }) => Box::pin(async move {
            let mut browser = connected(&conn).await?;
            sudohand_browser::tools::tab_close(&mut browser, tab_id).await
        }),
        Cmd::Runtime(RuntimeCommands::CdpSend {
            conn,
            method,
            params,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::cdp_send(&tab, &method, params.as_deref()).await
        }),
        Cmd::Runtime(RuntimeCommands::DialogRespond {
            conn,
            action,
            prompt_text,
            auto_handle,
            wait_timeout,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            let opts = DialogOptions {
                action,
                prompt_text,
                auto_handle,
                wait_timeout,
            };
            sudohand_browser::tools::dialog_respond(&tab, &opts).await
        }),
        Cmd::Runtime(RuntimeCommands::WindowSet {
            conn,
            width,
            height,
            state,
            focus,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::window_set(&tab, width, height, state.as_deref(), focus).await
        }),
        Cmd::Runtime(RuntimeCommands::StorageGet { conn, key }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::storage_get(&tab, key.as_deref()).await
        }),
        Cmd::Runtime(RuntimeCommands::StorageSet {
            conn,
            items,
            key,
            value,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            let items: Option<serde_json::Map<String, Value>> = match items {
                Some(raw) => match serde_json::from_str::<Value>(&raw)? {
                    Value::Object(m) => Some(m),
                    _ => {
                        return Err(sudohand_browser::Error::Invalid(
                            "--items must be a JSON object".to_string(),
                        ));
                    }
                },
                None => None,
            };
            sudohand_browser::tools::storage_set(
                &tab,
                items.as_ref(),
                key.as_deref(),
                value.as_deref(),
            )
            .await
        }),
        Cmd::Cookie(CookieCommands::CookiesList { conn, domain }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::cookies_list(&tab, domain.as_deref()).await
        }),
        Cmd::Cookie(CookieCommands::CookiesSave {
            conn,
            path,
            pattern,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::cookies_save(&tab, path.as_deref(), pattern.as_deref()).await
        }),
        Cmd::Cookie(CookieCommands::CookiesLoad { conn, path }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::cookies_load(&tab, path.as_deref()).await
        }),
        Cmd::Cookie(CookieCommands::CookiesImport {
            conn,
            domain,
            browser,
            user_data_dir,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::cookies_import(
                &tab,
                &domain,
                &browser,
                user_data_dir.as_deref(),
            )
            .await
        }),
        Cmd::Cookie(CookieCommands::LoginInteractive { url, cookies_path }) => {
            Box::pin(async move {
                sudohand_browser::tools::login_interactive(&url, cookies_path.as_deref()).await
            })
        }
        Cmd::Cookie(CookieCommands::CookiesExtractLive { conn, domain }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::cookies_extract_live(&tab, &domain).await
        }),
        Cmd::Cookie(CookieCommands::CookiesExtractOffline {
            domain,
            browser,
            user_data_dir,
        }) => Box::pin(async move {
            let cookies = sudohand_browser::tools::cookies_extract_offline(
                &domain,
                &browser,
                user_data_dir.as_deref(),
            )?;
            Ok(Value::Array(
                cookies
                    .iter()
                    .map(|c| {
                        json!({
                            "name": c.name, "value": c.value, "domain": c.domain, "path": c.path,
                            "secure": c.secure, "httpOnly": c.http_only, "expires": c.expires,
                        })
                    })
                    .collect(),
            ))
        }),
        Cmd::Cookie(CookieCommands::CookiesExtract {
            domain,
            browser,
            user_data_dir,
        }) => Box::pin(async move {
            let cookies = sudohand_browser::tools::cookies_extract(
                &domain,
                &browser,
                user_data_dir.as_deref(),
            )?;
            Ok(json!({
                "count": cookies.len(),
                "cookies": cookies.iter().map(|c| json!({
                    "name": c.name, "value": c.value, "domain": c.domain, "path": c.path,
                    "secure": c.secure, "httpOnly": c.http_only, "expires": c.expires,
                })).collect::<Vec<_>>(),
            }))
        }),
        Cmd::Download(DownloadCommands::Download { conn, url, path }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::download(&tab, &url, path.as_deref()).await
        }),
        Cmd::Download(DownloadCommands::DownloadLink {
            conn,
            xpath,
            download_dir,
            timeout,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::download_link(&tab, &xpath, download_dir.as_deref(), timeout)
                .await
        }),
        Cmd::PageOutput(PageOutputCommands::PageWaitReady {
            conn,
            timeout,
            idle_time,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            let ready = sudohand_browser::tools::page_wait_ready(&tab, timeout, idle_time).await;
            if ready {
                Ok(json!({"ready": true}))
            } else {
                Ok(json!({"error": "Operation failed"}))
            }
        }),
        Cmd::PageOutput(PageOutputCommands::PageHtml { conn, outer }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::page_html(&tab, outer).await
        }),
        Cmd::Reference(ReferenceCommands::ClickByRef {
            conn,
            r#ref,
            human_like,
            os_click,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::actions::click_by_ref_with_os_click(
                &tab, &r#ref, human_like, os_click,
            )
            .await
        }),
        Cmd::Reference(ReferenceCommands::ClickByText {
            conn,
            text,
            timeout,
            human_like,
            os_click,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::actions::click_by_text_with_os_click(
                &tab, &text, timeout, human_like, os_click,
            )
            .await
        }),
        Cmd::Mouse(MouseCommands::DragByRef {
            conn,
            r#ref,
            to_x,
            to_y,
            steps,
            human_like,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::drag_by_ref(&tab, &r#ref, to_x, to_y, steps, human_like).await
        }),
        Cmd::Mouse(MouseCommands::MouseMove {
            conn,
            x,
            y,
            screenshot,
            steps,
            human_like,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            let moved = sudohand_browser::tools::mouse_move(
                &tab,
                x,
                y,
                screenshot.as_deref(),
                steps,
                Some(human_like),
            )
            .await?;
            Ok(json!({"moved": moved}))
        }),
        Cmd::Mouse(MouseCommands::MouseClick {
            conn,
            x,
            y,
            screenshot,
            button,
            modifiers,
            double,
            human_like,
            r#move,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            let opts = ClickOptions {
                button,
                modifiers,
                double,
                human_like: Some(human_like),
                r#move,
            };
            let clicked =
                sudohand_browser::tools::mouse_click(&tab, x, y, screenshot.as_deref(), &opts)
                    .await?;
            Ok(json!({"clicked": clicked}))
        }),
        Cmd::Mouse(MouseCommands::MouseDrag {
            conn,
            from_x,
            from_y,
            to_x,
            to_y,
            screenshot,
            steps,
            human_like,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            let dragged = sudohand_browser::tools::mouse_drag(
                &tab,
                (from_x, from_y),
                (to_x, to_y),
                screenshot.as_deref(),
                steps,
                human_like,
            )
            .await?;
            Ok(json!({"dragged": dragged}))
        }),
        Cmd::Reference(ReferenceCommands::TypeByRef {
            conn,
            r#ref,
            text,
            clear,
            enter,
            keystrokes,
            human_like,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::tools::type_by_ref(
                &tab,
                &r#ref,
                &text,
                TypeOptions {
                    clear,
                    enter,
                    keystrokes,
                    human_like,
                },
            )
            .await
        }),
        Cmd::Tab(TabCommands::TabList { conn }) => Box::pin(async move {
            let mut browser = connected(&conn).await?;
            sudohand_browser::tools::tab_list(&mut browser).await
        }),
        Cmd::Tab(TabCommands::TabSwitch { conn, tab_id }) => Box::pin(async move {
            let mut browser = connected(&conn).await?;
            sudohand_browser::tools::tab_switch(&mut browser, tab_id).await
        }),
        Cmd::Runtime(RuntimeCommands::JsEvaluate {
            conn,
            expression,
            frame,
        }) => Box::pin(async move {
            let (_b, tab) = browser_and_tab(&conn).await?;
            sudohand_browser::page::js_evaluate_in(&tab, &expression, frame.as_deref()).await
        }),
    }
}

/// Entry from [`run`]: dispatch to the tool body, mapping its browser error
/// onto the shared envelope.
async fn run_flow(cmd: Cmd) -> sudohand_core::Result<Value> {
    run_async(cmd).await.map_err(sudohand_core::Error::from)
}

/// Entry point for `suh browser <tool>`: a current-thread runtime, the
/// tool's own JSON on success, and the sudohand error envelope on failure.
pub fn run(cmd: Cmd) -> sudohand_core::Result<Value> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| sudohand_core::Error::internal(format!("tokio: {e}")))?;
    rt.block_on(run_flow(cmd))
}
