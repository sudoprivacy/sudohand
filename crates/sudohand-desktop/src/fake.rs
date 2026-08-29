//! A backend that records every action and reports one running app with one
//! window. Used by consumers to test their policy/session logic (and by this
//! crate's own tests) with no Accessibility or Screen Recording permission.

use crate::backend::{
    AppInfo, AxNode, DesktopBackend, Modifiers, Permissions, Screenshot, WindowInfo,
};
use std::sync::{Arc, Mutex};
use sudohand_core::Result;

#[derive(Debug, Default)]
pub struct FakeBackend {
    pub actions: Mutex<Vec<String>>,
    pub frontmost: Mutex<Option<String>>,
    pub running: Mutex<Vec<String>>,
}

impl FakeBackend {
    pub fn with_running(apps: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            actions: Mutex::new(Vec::new()),
            frontmost: Mutex::new(None),
            running: Mutex::new(apps.iter().map(|s| s.to_string()).collect()),
        })
    }
    pub fn actions(&self) -> Vec<String> {
        self.actions.lock().unwrap().clone()
    }
    fn log(&self, s: String) {
        self.actions.lock().unwrap().push(s);
    }
}

impl DesktopBackend for FakeBackend {
    fn permissions(&self) -> Permissions {
        Permissions {
            accessibility: true,
            screen_recording: Some(true),
        }
    }
    fn apps(&self, bundle_ids: &[String]) -> Result<Vec<AppInfo>> {
        let running = self.running.lock().unwrap();
        let wanted: Vec<String> = if bundle_ids.is_empty() {
            running.clone()
        } else {
            bundle_ids.to_vec()
        };
        Ok(wanted
            .iter()
            .filter(|b| running.contains(b))
            .enumerate()
            .map(|(i, b)| AppInfo {
                bundle_id: b.clone(),
                name: b.rsplit('.').next().unwrap_or(b).to_string(),
                pid: 1000 + i as i32,
                frontmost: self.frontmost.lock().unwrap().as_deref() == Some(b),
                windows: vec![WindowInfo {
                    id: 42,
                    title: format!("{b} main"),
                    x: 100.0,
                    y: 50.0,
                    width: 800.0,
                    height: 600.0,
                    on_screen: true,
                }],
            })
            .collect())
    }
    fn activate(&self, bundle_id: &str) -> Result<()> {
        *self.frontmost.lock().unwrap() = Some(bundle_id.to_string());
        self.log(format!("activate {bundle_id}"));
        Ok(())
    }
    fn screenshot(
        &self,
        bundle_id: &str,
        window_id: u32,
        max_width: Option<u32>,
    ) -> Result<Screenshot> {
        self.log(format!("screenshot {bundle_id} {window_id} {max_width:?}"));
        let window = WindowInfo {
            id: window_id,
            title: format!("{bundle_id} main"),
            x: 100.0,
            y: 50.0,
            width: 800.0,
            height: 600.0,
            on_screen: true,
        };
        Ok(Screenshot {
            png: b"\x89PNG\r\n\x1a\nfake".to_vec(),
            width_px: 1600,
            height_px: 1200,
            scale: 2.0,
            origin: (window.x, window.y),
            window,
        })
    }
    fn ax_tree(
        &self,
        bundle_id: &str,
        window_id: Option<u32>,
        _d: usize,
        _n: usize,
    ) -> Result<AxNode> {
        self.log(format!("ax_tree {bundle_id} {window_id:?}"));
        Ok(AxNode {
            r#ref: "e1".into(),
            role: "AXWindow".into(),
            subrole: None,
            title: Some("main".into()),
            value: None,
            description: None,
            placeholder: None,
            identifier: None,
            enabled: true,
            focused: true,
            frame: Some([100.0, 50.0, 800.0, 600.0]),
            actions: vec![],
            children: vec![AxNode {
                r#ref: "e2".into(),
                role: "AXTextField".into(),
                subrole: None,
                title: None,
                value: Some(String::new()),
                description: None,
                placeholder: Some("Search".into()),
                identifier: None,
                enabled: true,
                focused: false,
                frame: Some([120.0, 60.0, 200.0, 24.0]),
                actions: vec!["AXPress".into()],
                children: vec![],
            }],
        })
    }
    fn ax_press(&self, r: &str) -> Result<()> {
        self.log(format!("press {r}"));
        Ok(())
    }
    fn ax_set_value(&self, r: &str, v: &str) -> Result<()> {
        self.log(format!("set_value {r} {v:?}"));
        Ok(())
    }
    fn ax_focus(&self, r: &str) -> Result<()> {
        self.log(format!("focus {r}"));
        Ok(())
    }
    fn click(&self, x: f64, y: f64, button: &str, count: u32) -> Result<()> {
        self.log(format!("click {x} {y} {button} x{count}"));
        Ok(())
    }
    fn type_text(&self, text: &str) -> Result<()> {
        self.log(format!("type {text:?}"));
        Ok(())
    }
    fn paste_file(&self, path: &std::path::Path) -> Result<()> {
        self.log(format!("paste_file {}", path.display()));
        Ok(())
    }
    fn key(&self, key: &str, mods: Modifiers) -> Result<()> {
        self.log(format!("key {key} {mods:?}"));
        Ok(())
    }
    fn frontmost(&self) -> Option<String> {
        self.frontmost.lock().unwrap().clone()
    }
}
