//! Human-like behaviour simulation — port of `core/human.py`.
//!
//! Adds randomness to mouse movement, clicking and typing so trusted CDP
//! input looks like a person: a quadratic Bézier base trajectory with a
//! smoothed Gaussian random walk on top, in-bounds random click offsets,
//! button hold time, keystroke timing and optional typos.
//!
//! Defaults match the Python module exactly ("free" features on, features
//! with a time cost off). Two ways to change them:
//! - library: [`configure`] / [`get_config`] (same as `human.configure(**kw)`);
//! - CLI: the `AI_DEV_BROWSER_HUMAN` environment variable, a JSON object of
//!   [`HumanConfig`] field overrides, e.g.
//!   `{"use_gaussian_path": true, "click_hold_enabled": true}`. Python has
//!   no equivalent (its CLI is one process per call and never exposes
//!   `configure`), so this is a Rust-only extension.
//!
//! No `rand` dependency: a small xoshiro256** seeded from the clock and pid
//! is all a humaniser needs, and it keeps the binary dependency-free.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use chromiumoxide_cdp::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
    MouseButton,
};
use serde::{Deserialize, Serialize};

use crate::cdp::MOUSE_EVENT_TIMEOUT;
use crate::connection::Tab;
use crate::{Error, Result};

/// Environment variable holding JSON overrides for [`HumanConfig`].
pub const HUMAN_ENV: &str = "AI_DEV_BROWSER_HUMAN";

/// Configuration for human-like behaviour. All delays are milliseconds
/// unless noted. Field names and defaults are identical to Python's
/// `HumanConfig` so a `configure(**kw)` call ports 1:1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HumanConfig {
    /// Gaussian approach path instead of a straight line (cost: +50 ms, default off).
    pub use_gaussian_path: bool,
    /// Movement duration in seconds when the gaussian path is used.
    pub mouse_duration: f64,
    /// ± variance ratio applied to `mouse_duration`.
    pub mouse_duration_variance: f64,
    /// Gaussian smoothing factor (higher = smoother).
    pub mouse_smoothness: f64,
    /// Path randomness factor (higher = more deviation from the straight line).
    pub mouse_randomness: f64,

    /// Random click offset inside the element's box (free, default on).
    pub click_offset_enabled: bool,
    /// Max offset as a ratio of the element size (±20 %).
    pub click_offset_ratio: f64,

    /// Random button hold time between press and release (cost: +45 ms, default off).
    pub click_hold_enabled: bool,
    /// Minimum hold time.
    pub click_hold_min_ms: f64,
    /// Maximum hold time.
    pub click_hold_max_ms: f64,

    /// Random interval between the two clicks of a double-click (default off).
    pub double_click_humanize: bool,
    /// Minimum double-click interval.
    pub double_click_interval_min_ms: f64,
    /// Maximum double-click interval.
    pub double_click_interval_max_ms: f64,

    /// Delays between keystrokes (cost: +350 ms / 10 chars, default off).
    pub type_humanize: bool,
    /// Minimum inter-key delay (~400 WPM).
    pub type_delay_min_ms: f64,
    /// Maximum inter-key delay (~270 WPM).
    pub type_delay_max_ms: f64,
    /// Simulate typos followed by Backspace (default off).
    pub typo_enabled: bool,
    /// Probability of a typo per character.
    pub typo_probability: f64,

    /// Minimum delay between actions ([`delay`]).
    pub action_delay_min_ms: f64,
    /// Maximum delay between actions.
    pub action_delay_max_ms: f64,

    /// Easing function for scroll (reserved; no scroll tool is ported).
    pub scroll_easing_enabled: bool,
    /// Overshoot + bounce back for scroll (reserved).
    pub scroll_overshoot_enabled: bool,
}

impl Default for HumanConfig {
    fn default() -> Self {
        Self {
            use_gaussian_path: false,
            mouse_duration: 0.05,
            mouse_duration_variance: 0.2,
            mouse_smoothness: 2.0,
            mouse_randomness: 0.5,
            click_offset_enabled: true,
            click_offset_ratio: 0.2,
            click_hold_enabled: false,
            click_hold_min_ms: 30.0,
            click_hold_max_ms: 60.0,
            double_click_humanize: false,
            double_click_interval_min_ms: 40.0,
            double_click_interval_max_ms: 80.0,
            type_humanize: false,
            type_delay_min_ms: 25.0,
            type_delay_max_ms: 45.0,
            typo_enabled: false,
            typo_probability: 0.02,
            action_delay_min_ms: 10.0,
            action_delay_max_ms: 50.0,
            scroll_easing_enabled: false,
            scroll_overshoot_enabled: false,
        }
    }
}

impl HumanConfig {
    /// Defaults with the JSON overrides from `$AI_DEV_BROWSER_HUMAN` applied.
    /// Unknown keys are rejected so a typo cannot silently do nothing.
    pub fn from_env() -> Result<Self> {
        match std::env::var(HUMAN_ENV) {
            Ok(raw) if !raw.trim().is_empty() => Self::from_json(&raw),
            _ => Ok(Self::default()),
        }
    }

    /// Defaults with the given JSON object of overrides applied.
    pub fn from_json(raw: &str) -> Result<Self> {
        let v: serde_json::Value = serde_json::from_str(raw)?;
        if !v.is_object() {
            return Err(Error::Invalid(format!(
                "{HUMAN_ENV} must be a JSON object of HumanConfig overrides"
            )));
        }
        serde_json::from_value(v).map_err(|e| {
            Error::Invalid(format!(
                "{HUMAN_ENV}: {e} (fields are the HumanConfig names, e.g. use_gaussian_path)"
            ))
        })
    }
}

// ---------------------------------------------------------------------------
// Global state: config + last mouse position per tab (keyed by target id)
// ---------------------------------------------------------------------------

fn config_cell() -> &'static Mutex<HumanConfig> {
    static CELL: OnceLock<Mutex<HumanConfig>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(HumanConfig::from_env().unwrap_or_default()))
}

/// Current global configuration (a copy).
#[must_use]
pub fn get_config() -> HumanConfig {
    config_cell()
        .lock()
        .map_or_else(|p| p.into_inner().clone(), |g| g.clone())
}

/// Replace the global configuration. Returns the new value, like Python's
/// `configure(**kwargs)`; build it from [`get_config`] to change one field.
pub fn configure(cfg: HumanConfig) -> HumanConfig {
    let mut g = config_cell()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *g = cfg.clone();
    cfg
}

fn last_pos_map() -> &'static Mutex<HashMap<String, (f64, f64)>> {
    static MAP: OnceLock<Mutex<HashMap<String, (f64, f64)>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

fn tab_key(tab: &Tab) -> String {
    tab.target.target_id.inner().clone()
}

/// Last known cursor position for a tab, `(0, 0)` if it never moved.
#[must_use]
pub fn get_last_mouse_pos(tab: &Tab) -> (f64, f64) {
    last_pos_map()
        .lock()
        .ok()
        .and_then(|m| m.get(&tab_key(tab)).copied())
        .unwrap_or((0.0, 0.0))
}

/// Record the cursor position for a tab.
pub fn set_last_mouse_pos(tab: &Tab, x: f64, y: f64) {
    if let Ok(mut m) = last_pos_map().lock() {
        m.insert(tab_key(tab), (x, y));
    }
}

// ---------------------------------------------------------------------------
// Randomness
// ---------------------------------------------------------------------------

/// xoshiro256** — small, fast, good enough for humanisation.
#[derive(Debug, Clone)]
pub struct Rng([u64; 4]);

impl Rng {
    /// Seed from the clock and process id.
    #[must_use]
    pub fn from_entropy() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        let pid = u64::from(std::process::id());
        let counter = {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        };
        Self::seed_from_u64(nanos ^ pid.rotate_left(32) ^ counter.rotate_left(17))
    }

    /// Deterministic seed (tests).
    #[must_use]
    pub fn seed_from_u64(seed: u64) -> Self {
        // splitmix64 to expand the seed.
        let mut s = seed;
        let mut next = || {
            s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = s;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        Self([next(), next(), next(), next()])
    }

    fn next_u64(&mut self) -> u64 {
        let s = &mut self.0;
        let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }

    /// Uniform in `[0, 1)`, like Python's `random.random()`.
    pub fn random(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in `[a, b]`, like `random.uniform`.
    pub fn uniform(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.random()
    }

    /// Standard normal via Box–Muller (same formula as the Python module).
    pub fn gaussian(&mut self) -> f64 {
        let u1 = self.random();
        let u2 = self.random();
        (-2.0 * (u1 + 1e-10).ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

fn rng() -> Rng {
    Rng::from_entropy()
}

// ---------------------------------------------------------------------------
// Built-in Gaussian mouse path
// ---------------------------------------------------------------------------

fn random_walk(rng: &mut Rng, length: usize, stddev: f64) -> Vec<f64> {
    let mut cumsum = 0.0;
    (0..length)
        .map(|_| {
            cumsum += rng.gaussian() * stddev;
            cumsum
        })
        .collect()
}

fn gaussian_smooth(data: &[f64], sigma: f64) -> Vec<f64> {
    if sigma <= 0.0 || data.len() < 3 {
        return data.to_vec();
    }
    let window = ((sigma * 2.0) as usize | 1).max(3);
    let half = window / 2;
    (0..data.len())
        .map(|i| {
            let start = i.saturating_sub(half);
            let end = (i + half + 1).min(data.len());
            data[start..end].iter().sum::<f64>() / (end - start) as f64
        })
        .collect()
}

fn morph_distribution(data: &[f64], target_mean: f64, target_std: f64) -> Vec<f64> {
    if data.is_empty() {
        return Vec::new();
    }
    let n = data.len() as f64;
    let mean = data.iter().sum::<f64>() / n;
    let variance = data.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
    let std = if variance > 0.0 { variance.sqrt() } else { 1.0 };
    data.iter()
        .map(|x| (x - mean) / std * target_std + target_mean)
        .collect()
}

fn bezier_quadratic(p0: f64, p1: f64, p2: f64, t: f64) -> f64 {
    (1.0 - t).powi(2) * p0 + 2.0 * (1.0 - t) * t * p1 + t * t * p2
}

/// Human-like path from `(start_x, start_y)` to `(end_x, end_y)`: a quadratic
/// Bézier with a random control point plus a smoothed Gaussian random walk,
/// morphed to the path's scale. First and last points are exact.
#[must_use]
pub fn generate_gaussian_path(
    start_x: i64,
    start_y: i64,
    end_x: i64,
    end_y: i64,
    duration: f64,
    smoothness: f64,
    randomness: f64,
) -> Vec<(i64, i64)> {
    generate_gaussian_path_with(
        &mut rng(),
        start_x,
        start_y,
        end_x,
        end_y,
        duration,
        smoothness,
        randomness,
    )
}

/// [`generate_gaussian_path`] with an explicit RNG (deterministic in tests).
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn generate_gaussian_path_with(
    rng: &mut Rng,
    start_x: i64,
    start_y: i64,
    end_x: i64,
    end_y: i64,
    duration: f64,
    smoothness: f64,
    randomness: f64,
) -> Vec<(i64, i64)> {
    let num_points = ((duration * 60.0) as usize).max(6); // 60 fps, min 6

    let stddev = randomness * 10.0;
    let random_x = random_walk(rng, num_points, stddev);
    let random_y = random_walk(rng, num_points, stddev);
    let smooth_x = gaussian_smooth(&random_x, smoothness);
    let smooth_y = gaussian_smooth(&random_y, smoothness);

    let dx = (end_x - start_x) as f64;
    let dy = (end_y - start_y) as f64;
    let morphed_x = morph_distribution(&smooth_x, dx / 2.0, (dx.abs() / 6.0).max(5.0));
    let morphed_y = morph_distribution(&smooth_y, dy / 2.0, (dy.abs() / 6.0).max(5.0));

    let (sx, sy, ex, ey) = (start_x as f64, start_y as f64, end_x as f64, end_y as f64);
    let control_x = rng.uniform(sx.min(ex), sx.max(ex));
    let control_y = rng.uniform(sy.min(ey), sy.max(ey));

    let mut path: Vec<(i64, i64)> = (0..num_points)
        .map(|i| {
            let t = if num_points > 1 {
                i as f64 / (num_points - 1) as f64
            } else {
                1.0
            };
            let bx = bezier_quadratic(sx, control_x, ex, t);
            let by = bezier_quadratic(sy, control_y, ey, t);
            // Python `int()` truncates toward zero.
            (
                (bx + morphed_x[i]).trunc() as i64,
                (by + morphed_y[i]).trunc() as i64,
            )
        })
        .collect();
    path[0] = (start_x, start_y);
    let last = path.len() - 1;
    path[last] = (end_x, end_y);
    path
}

// ---------------------------------------------------------------------------
// Click offset
// ---------------------------------------------------------------------------

/// Random offset within `±ratio` of the element size (`ratio` defaults to
/// the configured `click_offset_ratio`).
#[must_use]
pub fn calculate_click_offset(width: f64, height: f64, ratio: Option<f64>) -> (f64, f64) {
    calculate_click_offset_with(&mut rng(), width, height, ratio)
}

/// [`calculate_click_offset`] with an explicit RNG.
#[must_use]
pub fn calculate_click_offset_with(
    rng: &mut Rng,
    width: f64,
    height: f64,
    ratio: Option<f64>,
) -> (f64, f64) {
    let ratio = ratio.unwrap_or_else(|| get_config().click_offset_ratio);
    let max_x = width * ratio;
    let max_y = height * ratio;
    (rng.uniform(-max_x, max_x), rng.uniform(-max_y, max_y))
}

// ---------------------------------------------------------------------------
// CDP helpers
// ---------------------------------------------------------------------------

/// Parse a button name the way Python's `_get_mouse_button` does
/// (unknown → left).
#[must_use]
pub fn mouse_button(name: &str) -> MouseButton {
    match name.to_ascii_lowercase().as_str() {
        "right" => MouseButton::Right,
        "middle" => MouseButton::Middle,
        "none" => MouseButton::None,
        _ => MouseButton::Left,
    }
}

/// One `Input.dispatchMouseEvent`, bounded by [`MOUSE_EVENT_TIMEOUT`].
pub async fn dispatch_mouse(
    tab: &Tab,
    kind: DispatchMouseEventType,
    x: f64,
    y: f64,
    button: Option<MouseButton>,
    click_count: Option<i64>,
    modifiers: i64,
) -> Result<()> {
    let mut b = DispatchMouseEventParams::builder().r#type(kind).x(x).y(y);
    if let Some(btn) = button {
        b = b.button(btn);
    }
    if let Some(c) = click_count {
        b = b.click_count(c);
    }
    if modifiers != 0 {
        b = b.modifiers(modifiers);
    }
    tab.send_timeout(b.build().map_err(Error::Invalid)?, MOUSE_EVENT_TIMEOUT)
        .await?;
    Ok(())
}

async fn sleep_ms(ms: f64) {
    if ms > 0.0 {
        tokio::time::sleep(Duration::from_secs_f64(ms / 1000.0)).await;
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Random delay between actions (configured range unless overridden).
pub async fn delay(min_ms: Option<f64>, max_ms: Option<f64>) {
    let cfg = get_config();
    let min = min_ms.unwrap_or(cfg.action_delay_min_ms);
    let max = max_ms.unwrap_or(cfg.action_delay_max_ms);
    if max > 0.0 {
        sleep_ms(rng().uniform(min, max)).await;
    }
}

/// Options for [`mouse_move`]. `None` means "from config / last position".
#[derive(Debug, Clone, Copy, Default)]
pub struct MoveOptions {
    /// Start point (default: last known position for the tab).
    pub from: Option<(f64, f64)>,
    /// Gaussian movement duration in seconds (default: configured ± variance).
    pub duration: Option<f64>,
    /// Gaussian path (default: `use_gaussian_path`).
    pub use_gaussian: Option<bool>,
}

/// Move the cursor to `(x, y)` — along a timed gaussian path when enabled,
/// otherwise a 10-step straight line — and remember the position.
pub async fn mouse_move(tab: &Tab, x: f64, y: f64, opts: MoveOptions) -> Result<()> {
    let cfg = get_config();
    let (from_x, from_y) = opts.from.unwrap_or_else(|| get_last_mouse_pos(tab));
    let use_gaussian = opts.use_gaussian.unwrap_or(cfg.use_gaussian_path);

    if use_gaussian {
        let mut r = rng();
        let duration = opts.duration.unwrap_or_else(|| {
            let v = cfg.mouse_duration_variance;
            cfg.mouse_duration * r.uniform(1.0 - v, 1.0 + v)
        });
        let path = generate_gaussian_path_with(
            &mut r,
            from_x.trunc() as i64,
            from_y.trunc() as i64,
            x.trunc() as i64,
            y.trunc() as i64,
            duration,
            cfg.mouse_smoothness,
            cfg.mouse_randomness,
        );
        let per_point = if path.is_empty() {
            0.0
        } else {
            duration / path.len() as f64
        };
        for (px, py) in path {
            dispatch_mouse(
                tab,
                DispatchMouseEventType::MouseMoved,
                px as f64,
                py as f64,
                None,
                None,
                0,
            )
            .await?;
            if per_point > 0.0 {
                tokio::time::sleep(Duration::from_secs_f64(per_point)).await;
            }
        }
    } else {
        tab.mouse_move_steps(x, y, 10).await?;
    }
    set_last_mouse_pos(tab, x, y);
    Ok(())
}

async fn hold_if_enabled(cfg: &HumanConfig, rng: &mut Rng) {
    if cfg.click_hold_enabled {
        sleep_ms(rng.uniform(cfg.click_hold_min_ms, cfg.click_hold_max_ms)).await;
    }
}

/// Press + release at `(x, y)` with optional human hold time; moves there
/// first when `move_first`.
pub async fn mouse_click(
    tab: &Tab,
    x: f64,
    y: f64,
    button: &str,
    move_first: bool,
    from: Option<(f64, f64)>,
) -> Result<()> {
    if move_first {
        mouse_move(
            tab,
            x,
            y,
            MoveOptions {
                from,
                ..MoveOptions::default()
            },
        )
        .await?;
    }
    let cfg = get_config();
    let mut r = rng();
    let btn = mouse_button(button);
    dispatch_mouse(
        tab,
        DispatchMouseEventType::MousePressed,
        x,
        y,
        Some(btn.clone()),
        Some(1),
        0,
    )
    .await?;
    hold_if_enabled(&cfg, &mut r).await;
    dispatch_mouse(
        tab,
        DispatchMouseEventType::MouseReleased,
        x,
        y,
        Some(btn.clone()),
        Some(1),
        0,
    )
    .await?;
    Ok(())
}

/// Double-click at `(x, y)` with optional human hold time and inter-click
/// interval.
pub async fn mouse_double_click(
    tab: &Tab,
    x: f64,
    y: f64,
    button: &str,
    move_first: bool,
    from: Option<(f64, f64)>,
) -> Result<()> {
    if move_first {
        mouse_move(
            tab,
            x,
            y,
            MoveOptions {
                from,
                ..MoveOptions::default()
            },
        )
        .await?;
    }
    let cfg = get_config();
    let mut r = rng();
    let btn = mouse_button(button);
    dispatch_mouse(
        tab,
        DispatchMouseEventType::MousePressed,
        x,
        y,
        Some(btn.clone()),
        Some(1),
        0,
    )
    .await?;
    hold_if_enabled(&cfg, &mut r).await;
    dispatch_mouse(
        tab,
        DispatchMouseEventType::MouseReleased,
        x,
        y,
        Some(btn.clone()),
        Some(1),
        0,
    )
    .await?;
    if cfg.double_click_humanize {
        sleep_ms(r.uniform(
            cfg.double_click_interval_min_ms,
            cfg.double_click_interval_max_ms,
        ))
        .await;
    }
    dispatch_mouse(
        tab,
        DispatchMouseEventType::MousePressed,
        x,
        y,
        Some(btn.clone()),
        Some(2),
        0,
    )
    .await?;
    hold_if_enabled(&cfg, &mut r).await;
    dispatch_mouse(
        tab,
        DispatchMouseEventType::MouseReleased,
        x,
        y,
        Some(btn.clone()),
        Some(2),
        0,
    )
    .await?;
    Ok(())
}

async fn key_event(tab: &Tab, kind: DispatchKeyEventType, key: &str, vkey: i64) -> Result<()> {
    let p = DispatchKeyEventParams::builder()
        .r#type(kind)
        .key(key)
        .windows_virtual_key_code(vkey)
        .build()
        .map_err(Error::Invalid)?;
    tab.send(p).await?;
    Ok(())
}

async fn char_event(tab: &Tab, text: &str) -> Result<()> {
    let p = DispatchKeyEventParams::builder()
        .r#type(DispatchKeyEventType::Char)
        .text(text)
        .build()
        .map_err(Error::Invalid)?;
    tab.send(p).await?;
    Ok(())
}

/// Type `text` as `char` key events, with human timing (and optional
/// typo + Backspace) when `humanize` (default: `type_humanize`). The
/// caller focuses the element first.
pub async fn type_text(tab: &Tab, text: &str, humanize: Option<bool>) -> Result<()> {
    let cfg = get_config();
    let use_delays = humanize.unwrap_or(cfg.type_humanize);
    let mut r = rng();
    for ch in text.chars() {
        if use_delays && cfg.typo_enabled && r.random() < cfg.typo_probability {
            let delta: i32 = if r.random() < 0.5 { -1 } else { 1 };
            if let Some(wrong) = char::from_u32((ch as i32 + delta).max(0) as u32) {
                char_event(tab, &wrong.to_string()).await?;
                sleep_ms(r.uniform(100.0, 300.0)).await;
                key_event(tab, DispatchKeyEventType::RawKeyDown, "Backspace", 8).await?;
                key_event(tab, DispatchKeyEventType::KeyUp, "Backspace", 8).await?;
                sleep_ms(r.uniform(50.0, 150.0)).await;
            }
        }
        if use_delays {
            sleep_ms(r.uniform(cfg.type_delay_min_ms, cfg.type_delay_max_ms)).await;
        }
        char_event(tab, &ch.to_string()).await?;
    }
    Ok(())
}

/// Human-like left-button drag (Rust extension — Python's drag is linear):
/// press, hold, follow a timed gaussian path whose duration scales with
/// distance (≥ 0.3 s), pause, release.
pub async fn mouse_drag(
    tab: &Tab,
    from: (f64, f64),
    to: (f64, f64),
    duration: Option<f64>,
) -> Result<()> {
    let cfg = get_config();
    let mut r = rng();
    mouse_move(tab, from.0, from.1, MoveOptions::default()).await?;
    dispatch_mouse(
        tab,
        DispatchMouseEventType::MousePressed,
        from.0,
        from.1,
        Some(MouseButton::Left),
        Some(1),
        0,
    )
    .await?;
    sleep_ms(r.uniform(
        cfg.click_hold_min_ms.max(30.0),
        cfg.click_hold_max_ms.max(80.0),
    ))
    .await;
    let dist = ((to.0 - from.0).powi(2) + (to.1 - from.1).powi(2)).sqrt();
    let duration = duration.unwrap_or_else(|| {
        let v = cfg.mouse_duration_variance;
        (0.3 + dist / 600.0) * r.uniform(1.0 - v, 1.0 + v)
    });
    let path = generate_gaussian_path_with(
        &mut r,
        from.0.trunc() as i64,
        from.1.trunc() as i64,
        to.0.trunc() as i64,
        to.1.trunc() as i64,
        duration,
        cfg.mouse_smoothness,
        cfg.mouse_randomness,
    );
    let per_point = duration / path.len() as f64;
    for (px, py) in path {
        dispatch_mouse(
            tab,
            DispatchMouseEventType::MouseMoved,
            px as f64,
            py as f64,
            Some(MouseButton::Left),
            None,
            0,
        )
        .await?;
        tokio::time::sleep(Duration::from_secs_f64(per_point)).await;
    }
    // Land exactly on the target (the path's last point is truncated).
    dispatch_mouse(
        tab,
        DispatchMouseEventType::MouseMoved,
        to.0,
        to.1,
        Some(MouseButton::Left),
        None,
        0,
    )
    .await?;
    sleep_ms(r.uniform(40.0, 120.0)).await;
    dispatch_mouse(
        tab,
        DispatchMouseEventType::MouseReleased,
        to.0,
        to.1,
        Some(MouseButton::Left),
        Some(1),
        0,
    )
    .await?;
    set_last_mouse_pos(tab, to.0, to.1);
    Ok(())
}

/// Human-like click inside a box: in-bounds random offset from `center`,
/// then [`mouse_click`]. The actuator every element click goes through, so
/// `click_by_ref` and `click_by_text` click the same way.
pub async fn click_box(
    tab: &Tab,
    center: (f64, f64),
    width: f64,
    height: f64,
    from: Option<(f64, f64)>,
    use_offset: Option<bool>,
) -> Result<()> {
    let use_offset = use_offset.unwrap_or_else(|| get_config().click_offset_enabled);
    let (mut x, mut y) = center;
    if use_offset && width > 0.0 && height > 0.0 {
        let (ox, oy) = calculate_click_offset(width, height, None);
        x += ox;
        y += oy;
    }
    mouse_click(tab, x, y, "left", true, from).await
}

/// Click a DOM element with human-like behaviour (Python `click_element`):
/// its box from `DOM.getContentQuads`, then [`click_box`]. Falls back to a
/// JS click when the element has no geometry.
pub async fn click_element(tab: &Tab, element: &crate::element::DomElement) -> Result<()> {
    match element.position(tab).await {
        Ok(pos) => click_box(tab, pos.center(), pos.width(), pos.height(), None, None).await,
        Err(_) => element.click_js(tab).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_python() {
        let d = HumanConfig::default();
        assert!(!d.use_gaussian_path);
        assert!(d.click_offset_enabled);
        assert!((d.click_offset_ratio - 0.2).abs() < f64::EPSILON);
        assert!((d.mouse_duration - 0.05).abs() < f64::EPSILON);
        assert!(!d.click_hold_enabled && !d.type_humanize && !d.typo_enabled);
        assert!((d.type_delay_min_ms - 25.0).abs() < f64::EPSILON);
    }

    #[test]
    fn json_overrides_apply_and_reject_unknown_keys() {
        let c = HumanConfig::from_json(r#"{"use_gaussian_path": true, "click_hold_min_ms": 12}"#)
            .unwrap();
        assert!(c.use_gaussian_path);
        assert!((c.click_hold_min_ms - 12.0).abs() < f64::EPSILON);
        assert!((c.click_hold_max_ms - 60.0).abs() < f64::EPSILON);
        assert!(HumanConfig::from_json(r#"{"use_gausian_path": true}"#).is_err());
        assert!(HumanConfig::from_json("[1]").is_err());
    }

    #[test]
    fn rng_is_deterministic_and_in_range() {
        let mut a = Rng::seed_from_u64(7);
        let mut b = Rng::seed_from_u64(7);
        for _ in 0..1000 {
            let x = a.random();
            assert!(x.to_bits() == b.random().to_bits());
            assert!((0.0..1.0).contains(&x));
        }
        let mut c = Rng::seed_from_u64(1);
        let mean: f64 = (0..20_000).map(|_| c.gaussian()).sum::<f64>() / 20_000.0;
        assert!(mean.abs() < 0.05, "gaussian mean {mean}");
    }

    #[test]
    fn gaussian_path_has_exact_endpoints_and_frame_count() {
        let mut r = Rng::seed_from_u64(42);
        let p = generate_gaussian_path_with(&mut r, 10, 20, 300, 400, 0.5, 2.0, 0.5);
        assert_eq!(p.len(), 30);
        assert_eq!(p[0], (10, 20));
        assert_eq!(*p.last().unwrap(), (300, 400));
        // Min 6 points even for tiny durations.
        let p = generate_gaussian_path_with(&mut r, 0, 0, 5, 5, 0.0, 2.0, 0.5);
        assert_eq!(p.len(), 6);
        assert_eq!(p[0], (0, 0));
        assert_eq!(p[5], (5, 5));
        // Not a straight line: some intermediate point is off the segment.
        let p = generate_gaussian_path_with(&mut r, 0, 0, 600, 0, 0.5, 2.0, 0.5);
        assert!(p.iter().any(|&(_, y)| y != 0));
    }

    #[test]
    fn smoothing_and_morphing_match_python_semantics() {
        // window = max(3, int(sigma*2)|1); sigma=2 → 5, half=2
        let s = gaussian_smooth(&[0.0, 0.0, 10.0, 0.0, 0.0], 2.0);
        assert!((s[2] - 2.0).abs() < 1e-9);
        assert!((s[0] - (10.0 / 3.0)).abs() < 1e-9);
        assert_eq!(gaussian_smooth(&[1.0, 2.0], 2.0), vec![1.0, 2.0]);
        let m = morph_distribution(&[1.0, 2.0, 3.0], 10.0, 2.0);
        let mean = m.iter().sum::<f64>() / 3.0;
        assert!((mean - 10.0).abs() < 1e-9);
        assert!(morph_distribution(&[], 1.0, 1.0).is_empty());
    }

    #[test]
    fn click_offset_stays_within_ratio() {
        let mut r = Rng::seed_from_u64(3);
        for _ in 0..500 {
            let (ox, oy) = calculate_click_offset_with(&mut r, 100.0, 50.0, Some(0.2));
            assert!(ox.abs() <= 20.0 && oy.abs() <= 10.0);
        }
    }

    #[test]
    fn button_names() {
        assert_eq!(mouse_button("Right"), MouseButton::Right);
        assert_eq!(mouse_button("middle"), MouseButton::Middle);
        assert_eq!(mouse_button("bogus"), MouseButton::Left);
    }
}
