//! `suh desktop <command> [flags]` — a thin CLI over `sudohand-desktop`.
//! Subcommands, flags and JSON output mirror `adc` one-to-one, including the
//! agent layer (`locate` / `workflows` / `flow`, feature `agent` of
//! sudohand-desktop). Flows are desktop-only by design — no cross-actuator
//! workflows.
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
        /// Natural-language description of the element.
        #[arg(long)]
        find: String,
        #[arg(long)]
        model: Option<String>,
    },
    /// List the registered workflows (name, description, variables).
    Workflows,
    /// Run a registered workflow by name (scripted clicks first, VLM
    /// fallback on failure).
    Flow {
        name: String,
        /// `--var name=value`, substituted into `{{name}}` placeholders.
        #[arg(long = "var")]
        vars: Vec<String>,
        /// Grounding model (default qwen3.6-27b).
        #[arg(long)]
        model: Option<String>,
        /// Verification model (default qwen3.6-flash).
        #[arg(long)]
        ask_model: Option<String>,
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
            find,
            model,
        } => {
            use sudohand_desktop::vlm::Vlm;
            let mut vlm = sudohand_desktop::vlm::DashScopeVlm::from_env()?;
            if let Some(m) = model {
                vlm.locate_model = m;
            }
            let wid = match window {
                Some(w) => w,
                None => pick_window(b, &bundle)?,
            };
            let shot = b.screenshot(&bundle, wid, Some(1100))?;
            let t0 = std::time::Instant::now();
            let n = vlm.locate(&shot.png, &find)?;
            let (x, y) = sudohand_desktop::workflow::norm_to_point(n, &shot);
            json!({"find": find, "model": vlm.locate_model, "normalized": [n.x, n.y],
                   "point": {"x": x, "y": y}, "window": wid, "ms": t0.elapsed().as_millis()})
        }
        Cmd::Workflows => {
            json!({"workflows": sudohand_desktop::registry::Registry::builtins().list()})
        }
        Cmd::Flow {
            name,
            vars,
            model,
            ask_model,
        } => {
            let map = parse_vars(vars)?;
            let mut vlm = sudohand_desktop::vlm::DashScopeVlm::from_env()?;
            if let Some(m) = model {
                vlm.locate_model = m;
            }
            if let Some(m) = ask_model {
                vlm.ask_model = m;
            }
            let runner =
                sudohand_desktop::workflow::Runner::new(b_arc.clone(), std::sync::Arc::new(vlm));
            let graph =
                sudohand_desktop::registry::Registry::builtins().prepare(&name, &runner, &map)?;
            let report = sudohand_desktop::flow::run_graph_blocking(graph, map)?;
            json!({"ok": true, "workflow": name, "steps": report})
        }
    })
}

fn parse_vars(vars: Vec<String>) -> Result<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();
    for v in vars {
        let (k, val) = v.split_once('=').ok_or_else(|| {
            sudohand_desktop::Error::invalid(format!("--var {v:?}: expected name=value"))
        })?;
        map.insert(k.to_string(), val.to_string());
    }
    Ok(map)
}

/// The app's biggest on-screen window with a title, else biggest on-screen,
/// else biggest — the same choice adc made.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn pick_window(b: &dyn sudohand_desktop::DesktopBackend, bundle: &str) -> Result<u32> {
    sudohand_desktop::workflow::pick_window(b, bundle)
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
