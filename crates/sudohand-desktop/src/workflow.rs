//! Screen-geometry and window-picking helpers used by the VLM actions
//! (`locate` / `ask`) and by callers turning a normalized VLM point into a
//! screen coordinate. The workflow *engine* is no longer here — cross-actuator
//! react workflows live in `sudohand-flow`; this module keeps only the
//! desktop-specific utilities those actions need.

use crate::backend::{DesktopBackend, Screenshot};
use crate::vlm::NormPoint;
use sudohand_core::{Error, Result};

/// 0–1000 normalized → screenshot pixels → screen points.
pub fn norm_to_point(n: NormPoint, shot: &Screenshot) -> (f64, f64) {
    let px = n.x / 1000.0 * f64::from(shot.width_px);
    let py = n.y / 1000.0 * f64::from(shot.height_px);
    (
        shot.origin.0 + px / shot.scale,
        shot.origin.1 + py / shot.scale,
    )
}

/// The on-screen window whose title contains `needle` (case-insensitive),
/// biggest first. For targeting a specific window (e.g. "Settings") whose id
/// is not known ahead of time.
pub fn pick_window_titled(b: &dyn DesktopBackend, bundle: &str, needle: &str) -> Result<u32> {
    let apps = b.apps(std::slice::from_ref(&bundle.to_string()))?;
    let a = apps
        .into_iter()
        .find(|a| a.bundle_id == bundle)
        .ok_or_else(|| Error::not_found(format!("{bundle} is not running")))?;
    let ndl = needle.to_lowercase();
    let mut ws: Vec<_> = a
        .windows
        .into_iter()
        .filter(|w| w.title.to_lowercase().contains(&ndl))
        .collect();
    ws.sort_by(|x, y| {
        y.on_screen.cmp(&x.on_screen).then(
            (y.width * y.height)
                .partial_cmp(&(x.width * x.height))
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    ws.into_iter()
        .next()
        .map(|w| w.id)
        .ok_or_else(|| Error::not_found(format!("no {bundle} window titled {needle:?}")))
}

/// The app's biggest on-screen window with a title, else biggest on-screen,
/// else biggest.
pub fn pick_window(b: &dyn DesktopBackend, bundle: &str) -> Result<u32> {
    let apps = b.apps(std::slice::from_ref(&bundle.to_string()))?;
    let a = apps
        .into_iter()
        .find(|a| a.bundle_id == bundle)
        .ok_or_else(|| Error::not_found(format!("{bundle} is not running")))?;
    let mut ws = a.windows;
    ws.sort_by(|x, y| {
        y.on_screen
            .cmp(&x.on_screen)
            .then((!y.title.is_empty()).cmp(&(!x.title.is_empty())))
            .then(
                (y.width * y.height)
                    .partial_cmp(&(x.width * x.height))
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    ws.into_iter()
        .next()
        .map(|w| w.id)
        .ok_or_else(|| Error::not_found(format!("no window for {bundle}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeBackend;

    #[test]
    fn norm_to_point_mapping() {
        let b = FakeBackend::with_running(&["x"]);
        let shot = crate::DesktopBackend::screenshot(&*b, "x", 42, None).unwrap();
        let p = norm_to_point(NormPoint { x: 1000.0, y: 0.0 }, &shot);
        assert_eq!(p, (100.0 + 800.0, 50.0));
    }
}
