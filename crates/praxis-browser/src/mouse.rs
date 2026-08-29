//! Coordinate-level mouse tools — port of `core/mouse.py`: `mouse_move`,
//! `mouse_click`, `mouse_drag`.
//!
//! When a screenshot path is given, coordinates are mapped from image space
//! to CSS viewport space using the metadata `page_screenshot` embeds, so
//! numbers read off a screenshot are directly usable.

use std::path::Path;

use crate::connection::Tab;
use crate::geometry::scale_coords;
use crate::human::{self, MoveOptions};
use crate::page::read_screenshot_metadata;
use crate::Result;

/// Map `(x, y)` from screenshot space to CSS space if `screenshot` is given.
pub fn screenshot_coords(x: f64, y: f64, screenshot: Option<&Path>) -> Result<(f64, f64)> {
    match screenshot {
        Some(p) => {
            let meta = read_screenshot_metadata(p)?;
            Ok(scale_coords(x, y, meta.as_ref()))
        }
        None => Ok((x, y)),
    }
}

/// Move the cursor to `(x, y)` — native `steps`-step line, or the gaussian
/// actuator when `human_like` (default: config `use_gaussian_path`).
pub async fn mouse_move(
    tab: &Tab,
    x: f64,
    y: f64,
    screenshot: Option<&Path>,
    steps: usize,
    human_like: Option<bool>,
) -> Result<bool> {
    let (x, y) = screenshot_coords(x, y, screenshot)?;
    let use_human = human_like.unwrap_or_else(|| human::get_config().use_gaussian_path);
    if use_human {
        human::mouse_move(
            tab,
            x,
            y,
            MoveOptions {
                use_gaussian: Some(true),
                ..MoveOptions::default()
            },
        )
        .await?;
    } else {
        tab.mouse_move_steps(x, y, steps).await?;
        human::set_last_mouse_pos(tab, x, y);
    }
    Ok(true)
}

/// Options for [`mouse_click`].
#[derive(Debug, Clone)]
pub struct ClickOptions {
    /// `left` / `right` / `middle`.
    pub button: String,
    /// Modifier bitmask: 1=Alt, 2=Ctrl, 4=Meta, 8=Shift.
    pub modifiers: i64,
    /// Double-click.
    pub double: bool,
    /// Human-like timing (default: config `use_gaussian_path || click_hold_enabled`).
    pub human_like: Option<bool>,
    /// Move the cursor to the target before clicking.
    pub r#move: bool,
}

impl Default for ClickOptions {
    fn default() -> Self {
        Self {
            button: "left".into(),
            modifiers: 0,
            double: false,
            human_like: None,
            r#move: true,
        }
    }
}

/// Click at raw coordinates (screenshot space if `screenshot` is given).
pub async fn mouse_click(
    tab: &Tab,
    x: f64,
    y: f64,
    screenshot: Option<&Path>,
    opts: &ClickOptions,
) -> Result<bool> {
    let (x, y) = screenshot_coords(x, y, screenshot)?;
    let cfg = human::get_config();
    let use_human = opts
        .human_like
        .unwrap_or(cfg.use_gaussian_path || cfg.click_hold_enabled);
    if use_human {
        if opts.double {
            human::mouse_double_click(tab, x, y, &opts.button, opts.r#move, None).await?;
        } else {
            human::mouse_click(tab, x, y, &opts.button, opts.r#move, None).await?;
        }
    } else {
        if opts.r#move {
            tab.mouse_move_steps(x, y, 1).await?;
        }
        let btn = human::mouse_button(&opts.button);
        tab.mouse_click_with(x, y, btn.clone(), opts.modifiers)
            .await?;
        if opts.double {
            tab.mouse_click_with(x, y, btn.clone(), opts.modifiers)
                .await?;
        }
        human::set_last_mouse_pos(tab, x, y);
    }
    Ok(true)
}

/// Drag from `from` to `to` (screenshot space if `screenshot` is given).
///
/// `human_like: false` is the Python behaviour: a straight line in `steps`
/// moves. `human_like: true` is a Rust extension for slider captchas and
/// the like: the press is held for the configured hold time, the pointer
/// follows a timed gaussian path (`mouse_duration` scaled by distance, min
/// 0.3 s), and there is a short pause before release.
pub async fn mouse_drag(
    tab: &Tab,
    from: (f64, f64),
    to: (f64, f64),
    screenshot: Option<&Path>,
    steps: usize,
    human_like: bool,
) -> Result<bool> {
    let from = screenshot_coords(from.0, from.1, screenshot)?;
    let to = screenshot_coords(to.0, to.1, screenshot)?;
    if human_like {
        human::mouse_drag(tab, from, to, None).await?;
    } else {
        tab.mouse_drag(from, to, steps).await?;
    }
    human::set_last_mouse_pos(tab, to.0, to.1);
    Ok(true)
}
