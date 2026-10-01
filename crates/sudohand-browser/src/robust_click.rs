//! Observable click feedback with browser and opt-in native input fallbacks.
use crate::{connection::Tab, element::DomElement, Error, Result};
use serde_json::{json, Map, Value};
use std::time::Duration;

const SIGNATURE: &str = "({url:location.href,title:document.title,active:document.activeElement&&(document.activeElement.id||document.activeElement.tagName),len:document.body?.innerHTML.length||0})";
const TARGET: &str = r#"el => {
  let current = el.nodeType === 1 ? el : el.parentElement;
  if (!current) return null;
  const ancestors = [];
  for (let n = current; n && ancestors.length < 10; n = n.parentElement) ancestors.push(n);
  const sized = n => { const r=n.getBoundingClientRect(); return r.width>0 && r.height>0; };
  const actionable = n => n.matches('a,button,summary,label,select,[onclick],[jsaction],[jsname],[role="button"],[role="link"],[role="menuitem"],[role="menuitemcheckbox"],[role="menuitemradio"],[role="option"],[role="tab"],[role="checkbox"],[role="radio"],[role="switch"]') || n.tabIndex>=0 || getComputedStyle(n).cursor==='pointer';
  current = ancestors.find(n => sized(n) && actionable(n)) || ancestors.find(sized) || current;
  current.scrollIntoView({block:'center',inline:'center'});
  const r=current.getBoundingClientRect();
  let x=r.x, y=r.y, view=current.ownerDocument.defaultView;
  try { while(view!==view.top && view.frameElement) { const f=view.frameElement.getBoundingClientRect(); x+=f.x; y+=f.y; view=view.parent; } } catch {}
  return {x,y,w:r.width,h:r.height,tag:current.tagName.toLowerCase(),sx:view.screenX,sy:view.screenY,top:view.outerHeight-view.innerHeight};
}"#;
const SYNTHETIC: &str = r"el => {
  const node=el.nodeType===1?el:el.parentElement;
  if (!node) return false;
  const box=node.getBoundingClientRect();
  for (const name of ['pointerover','pointerenter','pointerdown','mousedown','pointerup','mouseup','click']) {
    const EventType=name.startsWith('pointer')?PointerEvent:MouseEvent;
    node.dispatchEvent(new EventType(name,{bubbles:true,cancelable:true,composed:true,clientX:box.x+box.width/2,clientY:box.y+box.height/2,button:0,buttons:1}));
  }
  return true;
}";

/// Explicit option wins over the environment. Native mouse control defaults off.
#[must_use]
pub fn resolve_os_click(explicit: Option<bool>) -> bool {
    explicit.unwrap_or_else(|| {
        matches!(
            std::env::var("AI_DEV_BROWSER_OS_CLICK")
                .unwrap_or_default()
                .trim()
                .to_lowercase()
                .as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

async fn changed(tab: &Tab, before: &Value) -> bool {
    tokio::time::sleep(Duration::from_millis(350)).await;
    tab.evaluate(SIGNATURE)
        .await
        .is_ok_and(|after| after != *before)
}

async fn native(tab: &Tab, target: &Value) -> Result<()> {
    let agent = tab.evaluate("navigator.userAgent").await?;
    if agent.as_str().is_some_and(|s| s.contains("HeadlessChrome")) {
        return Err(Error::Invalid(
            "OS click requires a visible Chrome window, not headless mode".into(),
        ));
    }
    tab.activate().await?;
    let number = |key: &str| {
        target[key]
            .as_f64()
            .ok_or_else(|| Error::Invalid("element screen coordinates unavailable".into()))
    };
    let x = number("sx")? + number("x")? + number("w")? / 2.0;
    let y = number("sy")? + number("top")? + number("y")? + number("h")? / 2.0;
    if !x.is_finite()
        || !y.is_finite()
        || x.abs() > f64::from(i32::MAX)
        || y.abs() > f64::from(i32::MAX)
    {
        return Err(Error::Invalid(
            "element screen coordinates out of range".into(),
        ));
    }
    tokio::task::spawn_blocking(move || platform_click(x.round() as i32, y.round() as i32))
        .await
        .map_err(|e| Error::Invalid(format!("OS input worker failed: {e}")))?
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn platform_click(x: i32, y: i32) -> Result<()> {
    use enigo::{Button, Coordinate, Direction, Enigo, Mouse, Settings};
    let settings = Settings {
        open_prompt_to_get_permissions: false,
        ..Settings::default()
    };
    let mut input = Enigo::new(&settings).map_err(|e| {
        Error::Invalid(format!(
            "OS input unavailable: {e}. Check desktop input permissions and the display session"
        ))
    })?;
    input
        .move_mouse(x, y, Coordinate::Abs)
        .map_err(|e| Error::Invalid(format!("OS mouse move failed: {e}")))?;
    std::thread::sleep(Duration::from_millis(120));
    input
        .button(Button::Left, Direction::Click)
        .map_err(|e| Error::Invalid(format!("OS mouse click failed: {e}")))
}

#[cfg(target_os = "linux")]
fn platform_click(x: i32, y: i32) -> Result<()> {
    use x11rb::{
        connection::Connection,
        protocol::{xproto, xtest::ConnectionExt},
    };
    let problem = |error: &dyn std::fmt::Display| {
        Error::Invalid(format!(
            "X11 native input unavailable: {error}. Check DISPLAY and XTEST support"
        ))
    };
    let (connection, screen) = x11rb::connect(None).map_err(|e| problem(&e))?;
    let x = i16::try_from(x).map_err(|e| problem(&e))?;
    let y = i16::try_from(y).map_err(|e| problem(&e))?;
    let root = connection.setup().roots[screen].root;
    connection
        .xtest_fake_input(xproto::MOTION_NOTIFY_EVENT, 0, 0, root, x, y, 0)
        .map_err(|e| problem(&e))?
        .check()
        .map_err(|e| problem(&e))?;
    std::thread::sleep(Duration::from_millis(120));
    for event in [xproto::BUTTON_PRESS_EVENT, xproto::BUTTON_RELEASE_EVENT] {
        connection
            .xtest_fake_input(event, 1, 0, root, x, y, 0)
            .map_err(|e| problem(&e))?
            .check()
            .map_err(|e| problem(&e))?;
    }
    connection.flush().map_err(|e| problem(&e))
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn platform_click(_x: i32, _y: i32) -> Result<()> {
    Err(Error::Invalid(
        "Native mouse input supports macOS, Windows, and X11 Linux".into(),
    ))
}

pub(crate) async fn click(
    tab: &Tab,
    node_id: i64,
    reference: &str,
    human_like: bool,
    os_click: bool,
) -> Map<String, Value> {
    match attempt(tab, node_id, reference, human_like, os_click).await {
        Ok(value) => value,
        Err(error) => Map::from_iter([
            ("clicked".into(), json!(false)),
            ("error".into(), json!(error.to_string())),
        ]),
    }
}

async fn attempt(
    tab: &Tab,
    node_id: i64,
    reference: &str,
    human_like: bool,
    os_click: bool,
) -> Result<Map<String, Value>> {
    crate::actions::resolve_ref_node(tab, reference, node_id).await?;
    let element = DomElement::new(node_id);
    let target = element.apply(tab, TARGET).await?;
    let before = tab.evaluate(SIGNATURE).await?;
    let mut method = None;
    if let (Some(x), Some(y), Some(w), Some(h)) = (
        target["x"].as_f64(),
        target["y"].as_f64(),
        target["w"].as_f64(),
        target["h"].as_f64(),
    ) {
        if w > 0.0 && h > 0.0 {
            let centre = (x + w / 2.0, y + h / 2.0);
            let dispatched = if human_like {
                crate::human::click_box(tab, centre, w, h, None, None).await
            } else {
                tab.mouse_click(centre.0, centre.1).await
            };
            if dispatched.is_ok() && changed(tab, &before).await {
                method = Some("trusted");
            }
        }
    }
    if method.is_none()
        && element.apply(tab, SYNTHETIC).await.is_ok()
        && changed(tab, &before).await
    {
        method = Some("synthetic");
    }
    if method.is_none()
        && element
            .apply(
                tab,
                "el => {const n=el.nodeType===1?el:el.parentElement; n?.click?.(); return true;}",
            )
            .await
            .is_ok()
        && changed(tab, &before).await
    {
        method = Some("js_click");
    }
    let mut hint = None;
    if method.is_none() && os_click {
        match native(tab, &target).await {
            Ok(()) => {
                if changed(tab, &before).await {
                    method = Some("os");
                }
            }
            Err(error) => hint = Some(error.to_string()),
        }
    }
    let mut result = Map::from_iter([
        ("clicked".into(), json!(true)),
        ("verified".into(), json!(method.is_some())),
        ("method".into(), json!(method)),
        ("node_id".into(), json!(node_id)),
        ("target".into(), target["tag"].clone()),
    ]);
    if let Some(hint) = hint {
        result.insert("hint".into(), json!(hint));
    }
    Ok(result)
}
