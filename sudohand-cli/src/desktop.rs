//! `suh desktop <command> [flags]` — a thin CLI over `sudohand-desktop`.
//! Subcommands, flags and JSON output mirror `adc` one-to-one, plus the VLM
//! actions `locate` / `ask` and the cheap deterministic checks
//! `find-window` / `ax-find` (feature `agent` of sudohand-desktop). Workflow
//! orchestration is not here — cross-actuator react workflows live in
//! `sudohand-flow` and drive these actions via the CLI. Prefer the cheap
//! checks (window list, accessibility tree — free, deterministic) over a VLM
//! `ask` where the app exposes the state.
//!
//! Only the stateless commands are exposed: the ref-based actions
//! (`ax_press`/`ax_set_value`/`ax_focus`) need the `ref` map from an `ax_tree`
//! in the *same* process, which a one-shot CLI cannot carry across calls — so
//! automate WeChat-style apps with `ax-tree` (to see the layout), `screenshot`,
//! and coordinate `click` + `type`/`key`.

use clap::Subcommand;
use serde_json::Value;
use sudohand_desktop::Result;

#[derive(Subcommand)]
pub enum Cmd {
    /// Accessibility/Screen-Recording permission state and the frontmost app.
    Status,
    /// Running apps: all Dock apps, or only the given `--bundle` ids.
    /// Compact by default (bundle, name, pid, frontmost, window count);
    /// `-v` adds every window's id/title/frame.
    Apps {
        #[arg(long = "bundle")]
        bundles: Vec<String>,
        #[arg(short, long)]
        verbose: bool,
    },
    /// PNG of one window (defaults to the largest on-screen window).
    Screenshot {
        #[arg(long)]
        bundle: String,
        #[arg(long)]
        window: Option<u32>,
        #[arg(long)]
        max_width: Option<u32>,
        /// Write the PNG here (else base64 in the JSON).
        #[arg(long)]
        out: Option<String>,
    },
    /// Accessibility element tree.
    AxTree {
        #[arg(long)]
        bundle: String,
        #[arg(long)]
        window: Option<u32>,
        #[arg(long, default_value_t = 25)]
        depth: usize,
        #[arg(long, default_value_t = 2000)]
        nodes: usize,
    },
    /// Bring an app to the foreground.
    Activate {
        #[arg(long)]
        bundle: String,
    },
    /// Synthetic mouse click at screen coordinates.
    Click {
        #[arg(long)]
        x: f64,
        #[arg(long)]
        y: f64,
        #[arg(long, default_value = "left")]
        button: String,
        #[arg(long, default_value_t = 1)]
        count: u32,
    },
    /// Insert text into the focused control of the frontmost app.
    Type {
        #[arg(long)]
        text: String,
    },
    /// Paste a file (image, document, …) into the focused control of the
    /// frontmost app, like Finder copy + ⌘V.
    PasteFile {
        #[arg(long)]
        path: String,
    },
    /// One key chord, e.g. `return`, `cmd+a`, `shift+tab`.
    Key {
        #[arg(long)]
        keys: String,
    },
    /// Ask the VLM where an element is on a window; prints the screen point.
    Locate {
        #[arg(long)]
        bundle: String,
        #[arg(long)]
        window: Option<u32>,
        /// Pick the window whose title contains this (over --window / largest).
        #[arg(long)]
        window_title: Option<String>,
        /// Natural-language description of the element.
        #[arg(long)]
        find: String,
        #[arg(long)]
        model: Option<String>,
    },
    /// Ask the VLM a yes/no question about a window; prints {answer, yes}.
    Ask {
        #[arg(long)]
        bundle: String,
        #[arg(long)]
        window: Option<u32>,
        /// Pick the window whose title contains this (over --window / largest).
        #[arg(long)]
        window_title: Option<String>,
        #[arg(long)]
        question: String,
        #[arg(long)]
        model: Option<String>,
    },
    /// Cheap, deterministic check: is there a window whose title contains
    /// `--title`? Prints {found, id, title, count} — no VLM, no screenshot.
    FindWindow {
        #[arg(long)]
        bundle: String,
        #[arg(long)]
        title: String,
    },
    /// Cheap, deterministic element search over the accessibility tree.
    /// Match by `--role` (exact, case-insensitive) and/or `--text` (substring
    /// of title/value/description). Prints {found, count, point (first
    /// match's center), matches:[…]} — no VLM. Use it to check "did the
    /// dialog appear?" or to click an AX element by name.
    AxFind {
        #[arg(long)]
        bundle: String,
        #[arg(long)]
        window: Option<u32>,
        /// Pick the window whose title contains this (over --window / largest).
        #[arg(long)]
        window_title: Option<String>,
        #[arg(long)]
        role: Option<String>,
        #[arg(long)]
        text: Option<String>,
        #[arg(long, default_value_t = 40)]
        depth: usize,
        #[arg(long, default_value_t = 3000)]
        nodes: usize,
    },
}

#[cfg(target_os = "macos")]
pub fn run(cmd: Cmd) -> Result<Value> {
    run_with(
        std::sync::Arc::new(sudohand_desktop::MacBackend::new()),
        cmd,
    )
}

#[cfg(not(target_os = "macos"))]
pub fn run(_cmd: Cmd) -> Result<Value> {
    Err(sudohand_desktop::Error::io(
        "suh desktop only runs on macOS",
    ))
}

/// Backend-generic body so the JSON shapes can be tested against the fake.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub fn run_with(
    b: std::sync::Arc<dyn sudohand_desktop::DesktopBackend>,
    cmd: Cmd,
) -> Result<Value> {
    use serde_json::json;
    use sudohand_desktop::{parse_key, Error};
    let b_arc = b;
    let b = &*b_arc;
    Ok(match cmd {
        Cmd::Status => json!({
            "permissions": b.permissions(),
            "frontmost": b.frontmost(),
        }),
        Cmd::Apps { bundles, verbose } => {
            let apps = b.apps(&bundles)?;
            if verbose {
                json!({ "apps": apps })
            } else {
                let compact: Vec<Value> = apps
                    .iter()
                    .map(|a| {
                        json!({
                            "bundle_id": a.bundle_id,
                            "name": a.name,
                            "pid": a.pid,
                            "frontmost": a.frontmost,
                            "windows": a.windows.len(),
                        })
                    })
                    .collect();
                json!({ "apps": compact })
            }
        }
        Cmd::Screenshot {
            bundle,
            window,
            max_width,
            out,
        } => {
            let wid = match window {
                Some(w) => w,
                None => pick_window(b, &bundle)?,
            };
            let shot = b.screenshot(&bundle, wid, max_width)?;
            // Everything an agent needs to turn an image pixel into a click
            // point: `x_pt = origin.x + px / scale`.
            let mut v = json!({
                "app": bundle,
                "window": shot.window,
                "bytes": shot.png.len(),
                "width_px": shot.width_px,
                "height_px": shot.height_px,
                "scale": shot.scale,
                "origin": {"x": shot.origin.0, "y": shot.origin.1},
            });
            match out {
                Some(path) => {
                    std::fs::write(&path, &shot.png)
                        .map_err(|e| Error::io(format!("write {path}: {e}")))?;
                    v["path"] = json!(path);
                }
                None => {
                    v["png_base64"] = json!(sudohand_desktop::b64::encode(&shot.png));
                }
            }
            v
        }
        Cmd::AxTree {
            bundle,
            window,
            depth,
            nodes,
        } => {
            json!({"app": bundle, "tree": b.ax_tree(&bundle, window, depth, nodes)?})
        }
        Cmd::Activate { bundle } => {
            b.activate(&bundle)?;
            json!({"activated": bundle, "frontmost": b.frontmost()})
        }
        Cmd::Click {
            x,
            y,
            button,
            count,
        } => {
            b.click(x, y, &button, count)?;
            json!({"clicked": {"x": x, "y": y, "button": button, "count": count}})
        }
        Cmd::Type { text } => {
            b.type_text(&text)?;
            json!({"typed_chars": text.chars().count()})
        }
        Cmd::PasteFile { path } => {
            b.paste_file(std::path::Path::new(&path))?;
            json!({"pasted_file": path})
        }
        Cmd::Key { keys } => {
            let (k, m) = parse_key(&keys)?;
            b.key(&k, m)?;
            json!({"key": keys})
        }
        Cmd::Locate {
            bundle,
            window,
            window_title,
            find,
            model,
        } => {
            use sudohand_desktop::vlm::Vlm;
            let mut vlm = sudohand_desktop::vlm::DashScopeVlm::from_env()?;
            if let Some(m) = model {
                vlm.locate_model = m;
            }
            let wid = pick_target(b, &bundle, window, window_title.as_deref())?;
            let shot = b.screenshot(&bundle, wid, Some(1100))?;
            let t0 = std::time::Instant::now();
            let n = vlm.locate(&shot.png, &find)?;
            let (x, y) = sudohand_desktop::workflow::norm_to_point(n, &shot);
            json!({"find": find, "model": vlm.locate_model, "normalized": [n.x, n.y],
                   "point": {"x": x, "y": y}, "window": wid, "ms": t0.elapsed().as_millis()})
        }
        Cmd::Ask {
            bundle,
            window,
            window_title,
            question,
            model,
        } => {
            use sudohand_desktop::vlm::Vlm;
            let mut vlm = sudohand_desktop::vlm::DashScopeVlm::from_env()?;
            if let Some(m) = model {
                vlm.ask_model = m;
            }
            let wid = pick_target(b, &bundle, window, window_title.as_deref())?;
            let shot = b.screenshot(&bundle, wid, Some(1100))?;
            let t0 = std::time::Instant::now();
            let answer = vlm.ask(&shot.png, &question)?;
            let yes = sudohand_desktop::vlm::is_yes(&answer);
            json!({"question": question, "answer": answer, "yes": yes,
                   "model": vlm.ask_model, "window": wid, "ms": t0.elapsed().as_millis()})
        }
        Cmd::FindWindow { bundle, title } => {
            let ndl = title.to_lowercase();
            let win = b
                .apps(std::slice::from_ref(&bundle))?
                .into_iter()
                .find(|a| a.bundle_id == bundle)
                .into_iter()
                .flat_map(|a| a.windows)
                .filter(|w| w.title.to_lowercase().contains(&ndl))
                .max_by(|x, y| {
                    x.on_screen.cmp(&y.on_screen).then(
                        (x.width * x.height)
                            .partial_cmp(&(y.width * y.height))
                            .unwrap_or(std::cmp::Ordering::Equal),
                    )
                });
            match win {
                Some(w) => json!({"found": true, "id": w.id, "title": w.title}),
                None => json!({"found": false}),
            }
        }
        Cmd::AxFind {
            bundle,
            window,
            window_title,
            role,
            text,
            depth,
            nodes,
        } => {
            let wid = match (window, window_title.as_deref()) {
                (Some(w), _) => Some(w),
                (None, Some(t)) => Some(sudohand_desktop::workflow::pick_window_titled(
                    b, &bundle, t,
                )?),
                (None, None) => None,
            };
            let tree = b.ax_tree(&bundle, wid, depth, nodes)?;
            let role_ci = role.as_ref().map(|r| r.to_lowercase());
            let text_ci = text.as_ref().map(|t| t.to_lowercase());
            let mut matches = Vec::new();
            let mut stack = vec![&tree];
            while let Some(n) = stack.pop() {
                for c in &n.children {
                    stack.push(c);
                }
                if let Some(r) = &role_ci {
                    if n.role.to_lowercase() != *r {
                        continue;
                    }
                }
                if let Some(t) = &text_ci {
                    let hay = [&n.title, &n.value, &n.description]
                        .into_iter()
                        .flatten()
                        .any(|s| s.to_lowercase().contains(t));
                    if !hay {
                        continue;
                    }
                }
                let center = n
                    .frame
                    .map(|[x, y, w, h]| json!({"x": x + w / 2.0, "y": y + h / 2.0}));
                matches.push(json!({
                    "ref": n.r#ref, "role": n.role, "title": n.title,
                    "value": n.value, "frame": n.frame, "center": center,
                }));
            }
            let point = matches
                .iter()
                .find_map(|m| m.get("center").filter(|c| !c.is_null()).cloned());
            json!({"found": !matches.is_empty(), "count": matches.len(),
                   "point": point, "matches": matches})
        }
    })
}

/// The app's biggest on-screen window with a title, else biggest on-screen,
/// else biggest — the same choice adc made.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn pick_window(b: &dyn sudohand_desktop::DesktopBackend, bundle: &str) -> Result<u32> {
    sudohand_desktop::workflow::pick_window(b, bundle)
}

/// Resolve the target window: explicit id, else by title substring, else the
/// largest on-screen window.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn pick_target(
    b: &dyn sudohand_desktop::DesktopBackend,
    bundle: &str,
    window: Option<u32>,
    title: Option<&str>,
) -> Result<u32> {
    match (window, title) {
        (Some(w), _) => Ok(w),
        (None, Some(t)) => sudohand_desktop::workflow::pick_window_titled(b, bundle, t),
        (None, None) => sudohand_desktop::workflow::pick_window(b, bundle),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sudohand_desktop::FakeBackend;

    #[test]
    fn json_shapes_mirror_adc() {
        let b = FakeBackend::with_running(&["com.example.app"]);
        let v = run_with(b.clone(), Cmd::Status).unwrap();
        assert_eq!(v["permissions"]["accessibility"], true);
        assert!(v["frontmost"].is_null());

        let v = run_with(
            b.clone(),
            Cmd::Apps {
                bundles: vec![],
                verbose: false,
            },
        )
        .unwrap();
        assert_eq!(v["apps"][0]["windows"], 1);
        let v = run_with(
            b.clone(),
            Cmd::Apps {
                bundles: vec!["com.example.app".into()],
                verbose: true,
            },
        )
        .unwrap();
        assert_eq!(v["apps"][0]["windows"][0]["id"], 42);

        let v = run_with(
            b.clone(),
            Cmd::Screenshot {
                bundle: "com.example.app".into(),
                window: None,
                max_width: None,
                out: None,
            },
        )
        .unwrap();
        assert_eq!(v["window"]["id"], 42);
        assert_eq!(v["scale"], 2.0);
        assert_eq!(v["origin"]["x"], 100.0);
        assert!(v["png_base64"].as_str().unwrap().starts_with("iVBOR"));
        assert!(v.get("path").is_none());

        let v = run_with(
            b.clone(),
            Cmd::Key {
                keys: "cmd+shift+a".into(),
            },
        )
        .unwrap();
        assert_eq!(v["key"], "cmd+shift+a");
        let e = run_with(b.clone(), Cmd::Key { keys: "cmd".into() }).unwrap_err();
        assert_eq!(e.code(), "invalid_input");

        let e = run_with(
            b.clone(),
            Cmd::Screenshot {
                bundle: "com.missing".into(),
                window: None,
                max_width: None,
                out: None,
            },
        )
        .unwrap_err();
        assert_eq!(e.code(), "not_found");
    }
}
