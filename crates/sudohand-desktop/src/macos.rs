//! macOS backend for `sudohand-desktop`: Accessibility (AXUIElement),
//! CoreGraphics window list + synthetic events, `screencapture` for images.
//!
//! This is the one module in the crate that uses `unsafe`: the
//! Accessibility API is a C API with manual CF memory management. Every
//! unsafe block is a CF call whose ownership rule ("Copy"/"Create" = we own
//! it, "Get" = borrowed) is stated at the call site.

#![allow(unsafe_code)]

use crate::backend::{
    AppInfo, AxNode, DesktopBackend, Modifiers, Permissions, Screenshot, WindowInfo,
};
use accessibility_sys::{
    kAXChildrenAttribute, kAXDescriptionAttribute, kAXEnabledAttribute, kAXErrorSuccess,
    kAXFocusedApplicationAttribute, kAXFocusedAttribute, kAXIdentifierAttribute,
    kAXMainWindowAttribute, kAXPlaceholderValueAttribute, kAXPositionAttribute, kAXRaiseAction,
    kAXRoleAttribute, kAXSizeAttribute, kAXSubroleAttribute, kAXTitleAttribute, kAXValueAttribute,
    kAXValueTypeCGPoint, kAXValueTypeCGSize, kAXWindowsAttribute, AXError,
    AXUIElementCopyActionNames, AXUIElementCopyAttributeValue, AXUIElementCreateApplication,
    AXUIElementCreateSystemWide, AXUIElementGetPid, AXUIElementGetTypeID, AXUIElementPerformAction,
    AXUIElementRef, AXUIElementSetAttributeValue, AXValueGetValue,
};
use core_foundation::array::CFArray;
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::{CGPoint, CGSize};
use core_graphics::window::{
    copy_window_info, kCGNullWindowID, kCGWindowListExcludeDesktopElements, kCGWindowListOptionAll,
};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSApplicationActivationOptions, NSApplicationActivationPolicy, NSPasteboard,
    NSPasteboardTypeString, NSPasteboardWriting, NSRunningApplication, NSWorkspace,
};
use objc2_foundation::{NSArray, NSString, NSURL};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use sudohand_core::permissions::macos as perm;
use sudohand_core::{Error, Result};

/// An owned AXUIElement reference. AX elements are process-global handles
/// and may be used from any thread.
struct AxElem(AXUIElementRef);
unsafe impl Send for AxElem {}
unsafe impl Sync for AxElem {}
impl Drop for AxElem {
    fn drop(&mut self) {
        // SAFETY: we own one retain on this element.
        unsafe { core_foundation::base::CFRelease(self.0 as CFTypeRef) };
    }
}
impl Clone for AxElem {
    fn clone(&self) -> Self {
        // SAFETY: retaining a valid CF object.
        unsafe { core_foundation::base::CFRetain(self.0 as CFTypeRef) };
        AxElem(self.0)
    }
}

impl AxElem {
    fn app(pid: i32) -> Self {
        // SAFETY: Create rule → owned.
        AxElem(unsafe { AXUIElementCreateApplication(pid) })
    }

    /// Read an attribute, surfacing the AX error. `Ok(None)` means the
    /// attribute exists but has no value.
    fn attr_res(&self, name: &str) -> std::result::Result<Option<CFType>, AXError> {
        let key = CFString::new(name);
        let mut out: CFTypeRef = std::ptr::null();
        // SAFETY: Copy rule → we own `out` on success.
        let err =
            unsafe { AXUIElementCopyAttributeValue(self.0, key.as_concrete_TypeRef(), &mut out) };
        if err == kAXErrorSuccess || err == accessibility_sys::kAXErrorNoValue {
            if out.is_null() {
                return Ok(None);
            }
            // SAFETY: Copy rule → owned.
            return Ok(Some(unsafe { CFType::wrap_under_create_rule(out) }));
        }
        Err(err)
    }

    /// Read an attribute, treating any failure (unsupported, no value, app
    /// not responding, …) as "absent". Use [`attr_res`](Self::attr_res)
    /// where the distinction matters.
    fn attr(&self, name: &str) -> Option<CFType> {
        self.attr_res(name).ok().flatten()
    }

    fn string(&self, name: &str) -> Option<String> {
        let v = self.attr(name)?;
        cf_to_string(&v)
    }

    fn bool(&self, name: &str) -> Option<bool> {
        self.attr(name)?.downcast::<CFBoolean>().map(|b| b.into())
    }

    fn elems(&self, name: &str) -> Vec<AxElem> {
        let Some(v) = self.attr(name) else {
            return Vec::new();
        };
        if !v.instance_of::<CFArray>() {
            return Vec::new();
        }
        // SAFETY: `v` is a CFArray we own; Get rule borrows it for the wrapper.
        let arr: CFArray<CFType> = unsafe { CFArray::wrap_under_get_rule(v.as_CFTypeRef() as _) };
        let ax_type = unsafe { AXUIElementGetTypeID() };
        arr.iter()
            .filter(|item| item.type_of() == ax_type)
            .map(|item| {
                // SAFETY: the array owns the element; retain our own reference.
                unsafe { core_foundation::base::CFRetain(item.as_CFTypeRef()) };
                AxElem(item.as_CFTypeRef() as AXUIElementRef)
            })
            .collect()
    }

    fn point(&self, name: &str) -> Option<CGPoint> {
        let v = self.attr(name)?;
        let mut p = CGPoint::new(0.0, 0.0);
        // SAFETY: `p` is a valid CGPoint out-param for kAXValueTypeCGPoint.
        let ok = unsafe {
            AXValueGetValue(
                v.as_CFTypeRef() as _,
                kAXValueTypeCGPoint,
                &mut p as *mut _ as *mut _,
            )
        };
        ok.then_some(p)
    }

    fn size(&self, name: &str) -> Option<CGSize> {
        let v = self.attr(name)?;
        let mut s = CGSize::new(0.0, 0.0);
        // SAFETY: as above, for CGSize.
        let ok = unsafe {
            AXValueGetValue(
                v.as_CFTypeRef() as _,
                kAXValueTypeCGSize,
                &mut s as *mut _ as *mut _,
            )
        };
        ok.then_some(s)
    }

    fn actions(&self) -> Vec<String> {
        let mut out: core_foundation::array::CFArrayRef = std::ptr::null();
        // SAFETY: Copy rule → owned on success.
        let err = unsafe { AXUIElementCopyActionNames(self.0, &mut out) };
        if err != kAXErrorSuccess || out.is_null() {
            return Vec::new();
        }
        let arr: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(out) };
        arr.iter().filter_map(|a| cf_to_string(&a)).collect()
    }

    fn perform(&self, action: &str) -> std::result::Result<(), AXError> {
        let a = CFString::new(action);
        // SAFETY: valid element and action name.
        let err = unsafe { AXUIElementPerformAction(self.0, a.as_concrete_TypeRef()) };
        if err == kAXErrorSuccess {
            Ok(())
        } else {
            Err(err)
        }
    }

    fn set(&self, name: &str, value: &CFType) -> std::result::Result<(), AXError> {
        let key = CFString::new(name);
        // SAFETY: valid element, attribute name and CF value.
        let err = unsafe {
            AXUIElementSetAttributeValue(self.0, key.as_concrete_TypeRef(), value.as_CFTypeRef())
        };
        if err == kAXErrorSuccess {
            Ok(())
        } else {
            Err(err)
        }
    }
}

fn cf_to_string(v: &CFType) -> Option<String> {
    if let Some(s) = v.downcast::<CFString>() {
        return Some(s.to_string());
    }
    if let Some(n) = v.downcast::<CFNumber>() {
        return n
            .to_i64()
            .map(|i| i.to_string())
            .or_else(|| n.to_f64().map(|f| f.to_string()));
    }
    if let Some(b) = v.downcast::<CFBoolean>() {
        let b: bool = b.into();
        return Some(b.to_string());
    }
    None
}

fn ax_err(what: &str, err: AXError) -> Error {
    let msg = match err {
        accessibility_sys::kAXErrorAPIDisabled => {
            "Accessibility permission not granted".to_string()
        }
        accessibility_sys::kAXErrorInvalidUIElement => {
            "element no longer exists (refresh with desktop.ax_tree)".to_string()
        }
        accessibility_sys::kAXErrorActionUnsupported => {
            "element does not support this action".to_string()
        }
        accessibility_sys::kAXErrorAttributeUnsupported => {
            "element does not support this attribute".to_string()
        }
        accessibility_sys::kAXErrorCannotComplete => "the application did not respond".to_string(),
        accessibility_sys::kAXErrorNotImplemented => {
            "the application does not implement accessibility".to_string()
        }
        e => format!("AXError {e}"),
    };
    let full = format!("{what}: {msg}");
    if err == accessibility_sys::kAXErrorAPIDisabled {
        Error::perm(full)
    } else {
        Error::io(full)
    }
}

/// Real backend. Holds the `ref` → element map from the last `ax_tree`.
#[derive(Debug)]
pub struct MacBackend {
    refs: Mutex<HashMap<String, RefEntry>>,
    /// How long to leave pasted content on the pasteboard before restoring
    /// the user's clipboard (the target reads it on its main thread).
    paste_settle: Duration,
}

impl Default for MacBackend {
    fn default() -> Self {
        Self {
            refs: Mutex::new(HashMap::new()),
            paste_settle: Duration::from_millis(120),
        }
    }
}

struct RefEntry {
    elem: AxElem,
    app: String,
}
impl std::fmt::Debug for RefEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RefEntry({})", self.app)
    }
}

impl MacBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the pasteboard settle delay used by `type_text`/`paste_file`.
    pub fn with_paste_settle(mut self, d: Duration) -> Self {
        self.paste_settle = d;
        self
    }

    fn running(&self, bundle_id: &str) -> Vec<(i32, String, bool)> {
        let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(
            &NSString::from_str(bundle_id),
        );
        apps.iter()
            .map(|a| {
                (
                    a.processIdentifier(),
                    a.localizedName().map(|n| n.to_string()).unwrap_or_default(),
                    a.isActive(),
                )
            })
            .collect()
    }

    fn windows_of(&self, pid: i32) -> Vec<WindowInfo> {
        let Some(list) = copy_window_info(
            kCGWindowListOptionAll | kCGWindowListExcludeDesktopElements,
            kCGNullWindowID,
        ) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for item in list.iter() {
            // SAFETY: window-info arrays hold CFDictionary entries; Get rule borrows.
            let dict: CFDictionary<CFString, CFType> =
                unsafe { CFDictionary::wrap_under_get_rule(*item as _) };
            let get =
                |k: &str| -> Option<CFType> { dict.find(CFString::new(k)).map(|v| v.clone()) };
            let owner = get("kCGWindowOwnerPID")
                .and_then(|v| v.downcast::<CFNumber>())
                .and_then(|n| n.to_i64());
            if owner != Some(pid as i64) {
                continue;
            }
            let layer = get("kCGWindowLayer")
                .and_then(|v| v.downcast::<CFNumber>())
                .and_then(|n| n.to_i64())
                .unwrap_or(0);
            if layer != 0 {
                continue; // menus, tooltips, status items
            }
            let id = get("kCGWindowNumber")
                .and_then(|v| v.downcast::<CFNumber>())
                .and_then(|n| n.to_i64())
                .unwrap_or(0) as u32;
            let title = get("kCGWindowName")
                .and_then(|v| cf_to_string(&v))
                .unwrap_or_default();
            let on_screen = get("kCGWindowIsOnscreen")
                .and_then(|v| v.downcast::<CFBoolean>())
                .map(|b| b.into())
                .unwrap_or(false);
            let (mut x, mut y, mut w, mut h) = (0.0, 0.0, 0.0, 0.0);
            if let Some(b) = get("kCGWindowBounds")
                .filter(|v| v.instance_of::<CFDictionary>())
                .map(|v| {
                    // SAFETY: verified to be a CFDictionary; Get rule borrows.
                    unsafe {
                        CFDictionary::<CFString, CFType>::wrap_under_get_rule(v.as_CFTypeRef() as _)
                    }
                })
            {
                let f = |k: &str| {
                    b.find(CFString::new(k))
                        .and_then(|v| v.downcast::<CFNumber>())
                        .and_then(|n| n.to_f64())
                        .unwrap_or(0.0)
                };
                x = f("X");
                y = f("Y");
                w = f("Width");
                h = f("Height");
            }
            if w < 50.0 || h < 50.0 {
                continue;
            }
            out.push(WindowInfo {
                id,
                title,
                x,
                y,
                width: w,
                height: h,
                on_screen,
            });
        }
        out
    }

    fn pid_of(&self, bundle_id: &str) -> Result<i32> {
        self.running(bundle_id)
            .first()
            .map(|(pid, _, _)| *pid)
            .ok_or_else(|| Error::not_found(format!("{bundle_id} is not running")))
    }

    fn require_ax(&self) -> Result<()> {
        if perm::accessibility_trusted() {
            Ok(())
        } else {
            // Ask macOS to show its own "allow accessibility access" prompt
            // (once per launch) so the user lands directly in Settings.
            perm::prompt_accessibility_once();
            Err(Error::perm("Accessibility permission not granted: System Settings → Privacy & Security → Accessibility → enable this app",
            ))
        }
    }

    fn lookup(&self, r: &str) -> Result<AxElem> {
        self.refs
            .lock()
            .unwrap()
            .get(r)
            .map(|e| e.elem.clone())
            .ok_or_else(|| {
                Error::not_found(format!("unknown ref {r:?}; call desktop.ax_tree first"))
            })
    }

    fn dump(
        &self,
        elem: &AxElem,
        app: &str,
        depth: usize,
        max_depth: usize,
        budget: &mut usize,
        refs: &mut HashMap<String, RefEntry>,
    ) -> Option<AxNode> {
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        let id = format!("e{}", refs.len() + 1);
        refs.insert(
            id.clone(),
            RefEntry {
                elem: elem.clone(),
                app: app.to_string(),
            },
        );
        let pos = elem.point(kAXPositionAttribute);
        let size = elem.size(kAXSizeAttribute);
        let frame = match (pos, size) {
            (Some(p), Some(s)) => Some([p.x, p.y, s.width, s.height]),
            _ => None,
        };
        let mut node = AxNode {
            r#ref: id,
            role: elem.string(kAXRoleAttribute).unwrap_or_default(),
            subrole: elem.string(kAXSubroleAttribute),
            title: elem.string(kAXTitleAttribute).filter(|s| !s.is_empty()),
            value: elem.string(kAXValueAttribute).map(|v| truncate(&v, 500)),
            description: elem
                .string(kAXDescriptionAttribute)
                .filter(|s| !s.is_empty()),
            placeholder: elem
                .string(kAXPlaceholderValueAttribute)
                .filter(|s| !s.is_empty()),
            identifier: elem
                .string(kAXIdentifierAttribute)
                .filter(|s| !s.is_empty()),
            enabled: elem.bool(kAXEnabledAttribute).unwrap_or(true),
            focused: elem.bool(kAXFocusedAttribute).unwrap_or(false),
            frame,
            actions: elem.actions(),
            children: Vec::new(),
        };
        if depth < max_depth {
            for child in elem.elems(kAXChildrenAttribute) {
                if let Some(c) = self.dump(&child, app, depth + 1, max_depth, budget, refs) {
                    node.children.push(c);
                }
            }
        }
        Some(node)
    }
}

impl MacBackend {
    /// Put something on the general pasteboard via `write`, synthesize ⌘V,
    /// then restore the user's prior clipboard *string* (best effort).
    ///
    /// - If `write` fails we restore immediately and return an error — we
    ///   must never send ⌘V with the user's old clipboard still in place.
    /// - The restore only happens if the pasteboard's `changeCount` is still
    ///   ours: if the user (or the target app) wrote to it meanwhile, their
    ///   content wins.
    /// - A non-string prior clipboard (image, files) is not preserved: after
    ///   the restore the pasteboard is empty.
    fn paste_via_pasteboard(
        &self,
        what: &str,
        write: impl FnOnce(&NSPasteboard) -> bool,
    ) -> Result<()> {
        let pb = NSPasteboard::generalPasteboard();
        // SAFETY: reading a string off the general pasteboard; returns None
        // when it holds no string type.
        let saved = unsafe { pb.stringForType(NSPasteboardTypeString) }.map(|s| s.to_string());
        let restore = |pb: &NSPasteboard| {
            pb.clearContents();
            if let Some(s) = &saved {
                // SAFETY: plain pasteboard write of an owned string.
                unsafe { pb.setString_forType(&NSString::from_str(s), NSPasteboardTypeString) };
            }
        };
        pb.clearContents();
        if !write(&pb) {
            restore(&pb);
            return Err(Error::io(format!(
                "could not put the {what} on the pasteboard"
            )));
        }
        let ours = pb.changeCount();
        // ⌘V — reuse the keycode path, which CEF/Electron/Qt do honour.
        let paste = self.key(
            "v",
            Modifiers {
                cmd: true,
                shift: false,
                alt: false,
                ctrl: false,
            },
        );
        if paste.is_ok() {
            // Give the target a moment to read the pasteboard before we
            // restore it, otherwise the paste can race the restore and land
            // the old text.
            std::thread::sleep(self.paste_settle);
        }
        if pb.changeCount() == ours {
            restore(&pb);
        }
        paste
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// Width/height from a PNG IHDR chunk.
fn png_dimensions(png: &[u8]) -> Result<(u32, u32)> {
    if png.len() < 24 || &png[..8] != b"\x89PNG\r\n\x1a\n" || &png[12..16] != b"IHDR" {
        return Err(Error::io("screencapture did not produce a PNG"));
    }
    let be = |i: usize| u32::from_be_bytes([png[i], png[i + 1], png[i + 2], png[i + 3]]);
    Ok((be(16), be(20)))
}

fn source() -> Result<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| Error::internal("could not create event source"))
}

fn keycode(name: &str) -> Option<u16> {
    Some(match name {
        "a" => 0,
        "s" => 1,
        "d" => 2,
        "f" => 3,
        "h" => 4,
        "g" => 5,
        "z" => 6,
        "x" => 7,
        "c" => 8,
        "v" => 9,
        "b" => 11,
        "q" => 12,
        "w" => 13,
        "e" => 14,
        "r" => 15,
        "y" => 16,
        "t" => 17,
        "1" => 18,
        "2" => 19,
        "3" => 20,
        "4" => 21,
        "6" => 22,
        "5" => 23,
        "=" => 24,
        "9" => 25,
        "7" => 26,
        "-" => 27,
        "8" => 28,
        "0" => 29,
        "]" => 30,
        "o" => 31,
        "u" => 32,
        "[" => 33,
        "i" => 34,
        "p" => 35,
        "return" | "enter" => 36,
        "l" => 37,
        "j" => 38,
        "'" => 39,
        "k" => 40,
        ";" => 41,
        "\\" => 42,
        "," => 43,
        "/" => 44,
        "n" => 45,
        "m" => 46,
        "." => 47,
        "tab" => 48,
        "space" => 49,
        "`" => 50,
        "delete" | "backspace" => 51,
        "escape" | "esc" => 53,
        "f5" => 96,
        "f6" => 97,
        "f7" => 98,
        "f3" => 99,
        "f8" => 100,
        "f9" => 101,
        "f11" => 103,
        "f13" => 105,
        "f14" => 107,
        "f10" => 109,
        "f12" => 111,
        "f15" => 113,
        "home" => 115,
        "pageup" => 116,
        "forwarddelete" => 117,
        "f4" => 118,
        "end" => 119,
        "f2" => 120,
        "pagedown" => 121,
        "f1" => 122,
        "left" => 123,
        "right" => 124,
        "down" => 125,
        "up" => 126,
        _ => return None,
    })
}

impl DesktopBackend for MacBackend {
    fn permissions(&self) -> Permissions {
        sudohand_core::permissions::probe()
    }

    fn apps(&self, bundle_ids: &[String]) -> Result<Vec<AppInfo>> {
        let mut out = Vec::new();
        if bundle_ids.is_empty() {
            let apps = NSWorkspace::sharedWorkspace().runningApplications();
            for a in apps.iter() {
                if a.activationPolicy() != NSApplicationActivationPolicy::Regular {
                    continue;
                }
                let Some(bundle) = a.bundleIdentifier() else {
                    continue;
                };
                let pid = a.processIdentifier();
                out.push(AppInfo {
                    bundle_id: bundle.to_string(),
                    name: a.localizedName().map(|n| n.to_string()).unwrap_or_default(),
                    pid,
                    frontmost: a.isActive(),
                    windows: self.windows_of(pid),
                });
            }
            out.sort_by_key(|x| x.name.to_lowercase());
            return Ok(out);
        }
        for b in bundle_ids {
            for (pid, name, active) in self.running(b) {
                out.push(AppInfo {
                    bundle_id: b.clone(),
                    name,
                    pid,
                    frontmost: active,
                    windows: self.windows_of(pid),
                });
            }
        }
        Ok(out)
    }

    fn activate(&self, bundle_id: &str) -> Result<()> {
        if self.frontmost().as_deref() == Some(bundle_id) {
            return Ok(());
        }
        // A locked screen (or the login window) owns the foreground; macOS will
        // not let us raise any app and CGEvent input would go nowhere useful.
        // Fail with a precise, honest message instead of a vague "did not come
        // to the front" — desktop control is simply unavailable while locked.
        if screen_is_locked() {
            return Err(Error::io(
                "screen is locked — desktop control is unavailable until the Mac is unlocked",
            ));
        }
        // Foregrounding another app from *this* process is the hard part: we
        // are a background helper (no NSApplication, not itself frontmost), and
        // macOS restricts `NSRunningApplication.activate` and AX `AXFrontmost`
        // from such a process — they often no-op silently. The one primitive
        // that reliably works is `open -b`, which asks LaunchServices to do the
        // activation on our behalf (and also launches the app if it is not
        // running). So drive that first, unconditionally, then add the AX raise
        // as a best-effort nudge to bring the *right* window up.
        let started = Instant::now();
        let st = std::process::Command::new("open")
            .args(["-b", bundle_id])
            .status()
            .map_err(|e| Error::io(format!("open -b failed: {e}")))?;
        if !st.success() {
            return Err(Error::io(format!("could not activate {bundle_id}")));
        }
        for (pid, _, _) in self.running(bundle_id) {
            if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
                app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
            }
            let elem = AxElem::app(pid);
            if let Some(w) = elem.attr(kAXMainWindowAttribute) {
                // SAFETY: AXUIElement value; Get rule while `w` is alive.
                unsafe {
                    AXUIElementPerformAction(
                        w.as_CFTypeRef() as AXUIElementRef,
                        CFString::new(kAXRaiseAction).as_concrete_TypeRef(),
                    )
                };
            }
        }
        let deadline = started + Duration::from_secs(8);
        while Instant::now() < deadline {
            if self.frontmost().as_deref() == Some(bundle_id) {
                std::thread::sleep(Duration::from_millis(150));
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let now = self.frontmost();
        Err(Error::io(format!(
            "{bundle_id} did not come to the front (frontmost is {now:?})"
        )))
    }
    fn screenshot(
        &self,
        bundle_id: &str,
        window_id: u32,
        max_width: Option<u32>,
    ) -> Result<Screenshot> {
        if !perm::screen_capture_preflight() {
            return Err(Error::perm("Screen Recording permission not granted: System Settings → Privacy & Security → Screen & System Audio Recording → enable this process",
            ));
        }
        let pid = self.pid_of(bundle_id)?;
        // The window must belong to the app: `screencapture -l` would happily
        // image any window on the system otherwise.
        let window = self
            .windows_of(pid)
            .into_iter()
            .find(|w| w.id == window_id)
            .ok_or_else(|| {
                Error::not_found(format!("window {window_id} does not belong to {bundle_id}"))
            })?;
        let tmp = std::env::temp_dir().join(format!(
            "apeiron-bridge-shot-{}-{}.png",
            std::process::id(),
            window_id
        ));
        let st = std::process::Command::new("screencapture")
            .args(["-x", "-o", "-l", &window_id.to_string(), "-t", "png"])
            .arg(&tmp)
            .status()
            .map_err(|e| Error::io(format!("screencapture failed: {e}")))?;
        let bytes = std::fs::read(&tmp).map_err(|e| Error::from_io(&e));
        let _ = std::fs::remove_file(&tmp);
        if !st.success() {
            return Err(Error::io("screencapture returned an error (window gone?)"));
        }
        let mut bytes = bytes?;
        let (mut width_px, mut height_px) = png_dimensions(&bytes)?;
        if let Some(mw) = max_width.filter(|&mw| mw > 0 && mw < width_px) {
            let img = image::load_from_memory(&bytes)
                .map_err(|e| Error::io(format!("decode png: {e}")))?;
            let scaled = img.resize(mw, u32::MAX, image::imageops::FilterType::Triangle);
            let mut out = std::io::Cursor::new(Vec::new());
            scaled
                .write_to(&mut out, image::ImageFormat::Png)
                .map_err(|e| Error::io(format!("encode png: {e}")))?;
            width_px = scaled.width();
            height_px = scaled.height();
            bytes = out.into_inner();
        }
        // `screencapture -l` images the window at the display's backing scale,
        // so pixels / points of the window frame is the mapping factor.
        let scale = if window.width > 0.0 {
            f64::from(width_px) / window.width
        } else {
            1.0
        };
        Ok(Screenshot {
            png: bytes,
            width_px,
            height_px,
            scale,
            origin: (window.x, window.y),
            window,
        })
    }

    fn ax_tree(
        &self,
        bundle_id: &str,
        window_id: Option<u32>,
        max_depth: usize,
        max_nodes: usize,
    ) -> Result<AxNode> {
        self.require_ax()?;
        let pid = self.pid_of(bundle_id)?;
        let app = AxElem::app(pid);
        let mut refs = HashMap::new();
        let mut budget = max_nodes;
        let root = match window_id {
            Some(wid) => {
                // Match the AX window to the CG window by frame.
                let target = self.windows_of(pid).into_iter().find(|w| w.id == wid);
                let windows = app.elems(kAXWindowsAttribute);
                let chosen = target.and_then(|t| {
                    windows.into_iter().find(|w| {
                        let p = w.point(kAXPositionAttribute);
                        let s = w.size(kAXSizeAttribute);
                        matches!((p, s), (Some(p), Some(s)) if (p.x - t.x).abs() < 2.0 && (p.y - t.y).abs() < 2.0 && (s.width - t.width).abs() < 2.0 && (s.height - t.height).abs() < 2.0)
                    })
                });
                chosen
                    .ok_or_else(|| Error::not_found(format!("no AX window matches window {wid}")))?
            }
            None => app.clone(),
        };
        // Fail loudly when the app is hung or has no accessibility at all,
        // instead of returning an empty tree.
        root.attr_res(kAXRoleAttribute)
            .map_err(|e| ax_err("ax_tree", e))?;
        let node = self
            .dump(&root, bundle_id, 0, max_depth, &mut budget, &mut refs)
            .ok_or_else(|| Error::io("empty accessibility tree"))?;
        *self.refs.lock().unwrap() = refs;
        Ok(node)
    }

    fn ax_press(&self, r: &str) -> Result<()> {
        self.require_ax()?;
        self.lookup(r)?
            .perform("AXPress")
            .map_err(|e| ax_err("press", e))
    }

    fn ax_set_value(&self, r: &str, value: &str) -> Result<()> {
        self.require_ax()?;
        let v = CFString::new(value).as_CFType();
        self.lookup(r)?
            .set(kAXValueAttribute, &v)
            .map_err(|e| ax_err("set_value", e))
    }

    fn ax_focus(&self, r: &str) -> Result<()> {
        self.require_ax()?;
        let v = CFBoolean::true_value().as_CFType();
        self.lookup(r)?
            .set(kAXFocusedAttribute, &v)
            .map_err(|e| ax_err("focus", e))
    }

    fn click(&self, x: f64, y: f64, button: &str, count: u32) -> Result<()> {
        self.require_ax()?;
        let src = source()?;
        let point = CGPoint::new(x, y);
        let (down, up, btn) = match button {
            "right" => (
                CGEventType::RightMouseDown,
                CGEventType::RightMouseUp,
                CGMouseButton::Right,
            ),
            _ => (
                CGEventType::LeftMouseDown,
                CGEventType::LeftMouseUp,
                CGMouseButton::Left,
            ),
        };
        let mk = |t: CGEventType| {
            CGEvent::new_mouse_event(src.clone(), t, point, btn)
                .map_err(|_| Error::internal("could not create mouse event"))
        };
        mk(CGEventType::MouseMoved)?.post(CGEventTapLocation::HID);
        std::thread::sleep(Duration::from_millis(30));
        for i in 1..=count as i64 {
            let d = mk(down)?;
            d.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, i);
            d.post(CGEventTapLocation::HID);
            std::thread::sleep(Duration::from_millis(20));
            let u = mk(up)?;
            u.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, i);
            u.post(CGEventTapLocation::HID);
            std::thread::sleep(Duration::from_millis(40));
        }
        Ok(())
    }

    fn type_text(&self, text: &str) -> Result<()> {
        self.require_ax()?;
        // Type by pasting, not by synthesizing per-character key events.
        //
        // The obvious approach — a keycode-0 event with a Unicode string
        // attached (`CGEvent::set_string`) — works for native AppKit fields but
        // is silently dropped by Chromium/CEF, Electron and Qt text inputs
        // (WeChat, Slack, VS Code, …): those toolkits only honour key events
        // that carry a real virtual keycode. So instead we put the text on the
        // pasteboard and synthesize ⌘V, a real chord every macOS app maps to
        // Paste. See `paste_via_pasteboard` for how the user's clipboard is
        // restored.
        if text.is_empty() {
            return Ok(());
        }
        let s = NSString::from_str(text);
        self.paste_via_pasteboard("text", |pb| unsafe {
            pb.setString_forType(&s, NSPasteboardTypeString)
        })
    }

    fn paste_file(&self, path: &std::path::Path) -> Result<()> {
        self.require_ax()?;
        let abs = std::fs::canonicalize(path).map_err(|e| Error::from_io(&e))?;
        let Some(p) = abs.to_str() else {
            return Err(Error::invalid(format!(
                "path is not valid UTF-8: {}",
                abs.display()
            )));
        };
        let url = NSURL::fileURLWithPath(&NSString::from_str(p));
        let obj: objc2::rc::Retained<ProtocolObject<dyn NSPasteboardWriting>> =
            ProtocolObject::from_retained(url);
        let objs = NSArray::from_retained_slice(&[obj]);
        self.paste_via_pasteboard("file", |pb| pb.writeObjects(&objs))
    }

    fn key(&self, key: &str, mods: Modifiers) -> Result<()> {
        self.require_ax()?;
        let code = keycode(key).ok_or_else(|| Error::invalid(format!("unknown key {key:?}")))?;
        let src = source()?;
        let mut flags = CGEventFlags::CGEventFlagNull;
        if mods.cmd {
            flags |= CGEventFlags::CGEventFlagCommand;
        }
        if mods.shift {
            flags |= CGEventFlags::CGEventFlagShift;
        }
        if mods.alt {
            flags |= CGEventFlags::CGEventFlagAlternate;
        }
        if mods.ctrl {
            flags |= CGEventFlags::CGEventFlagControl;
        }
        for down in [true, false] {
            let ev = CGEvent::new_keyboard_event(src.clone(), code, down)
                .map_err(|_| Error::internal("could not create key event"))?;
            ev.set_flags(flags);
            ev.post(CGEventTapLocation::HID);
            std::thread::sleep(Duration::from_millis(15));
        }
        Ok(())
    }

    fn frontmost(&self) -> Option<String> {
        // `NSWorkspace.frontmostApplication` is only refreshed by the main
        // run loop, so in `--headless` (no NSApplication) it goes stale and
        // keeps reporting whichever app was frontmost at startup. Ask the
        // accessibility server instead: it answers synchronously and does
        // not need a run loop. Fall back to NSWorkspace if AX is unavailable
        // (no permission) — the menu-bar build has a run loop anyway.
        ax_focused_app_pid()
            .and_then(NSRunningApplication::runningApplicationWithProcessIdentifier)
            .and_then(|a| a.bundleIdentifier())
            .map(|s| s.to_string())
            .or_else(|| {
                NSWorkspace::sharedWorkspace()
                    .frontmostApplication()
                    .and_then(|a| a.bundleIdentifier())
                    .map(|s| s.to_string())
            })
    }
}

/// Whether the screen is locked (or sitting at the login window). While
/// locked, macOS gives the foreground to `loginwindow`, refuses app
/// activation, and blocks `screencapture`, so every desktop action would
/// fail — we detect it up front to return a precise error.
fn screen_is_locked() -> bool {
    // `CGSessionCopyCurrentDictionary` exposes the console session state,
    // including `CGSSessionScreenIsLocked`. It is part of CoreGraphics but not
    // wrapped by the `core-graphics` crate, so declare it directly.
    #[allow(non_snake_case)]
    extern "C" {
        fn CGSessionCopyCurrentDictionary() -> core_foundation::dictionary::CFDictionaryRef;
    }
    // SAFETY: Copy rule → we own the returned dictionary (may be null when
    // there is no active console session, e.g. over ssh).
    let dict_ref = unsafe { CGSessionCopyCurrentDictionary() };
    if dict_ref.is_null() {
        return false;
    }
    let dict: CFDictionary<CFType, CFType> =
        unsafe { CFDictionary::wrap_under_create_rule(dict_ref) };
    dict.find(CFString::new("CGSSessionScreenIsLocked").as_CFType())
        .and_then(|v| v.downcast::<CFBoolean>())
        .map(|b| b.into())
        .unwrap_or(false)
}

/// Pid of the application that currently has keyboard focus, according to
/// the accessibility server. `None` without accessibility permission.
fn ax_focused_app_pid() -> Option<i32> {
    // SAFETY: Create rule → owned; released by AxElem::drop.
    let system = AxElem(unsafe { AXUIElementCreateSystemWide() });
    let app = system.attr(kAXFocusedApplicationAttribute)?;
    // SAFETY: the attribute value is an AXUIElement; Get rule (borrowed from `app`).
    let mut pid: i32 = 0;
    let err = unsafe { AXUIElementGetPid(app.as_CFTypeRef() as AXUIElementRef, &mut pid) };
    (err == kAXErrorSuccess && pid > 0).then_some(pid)
}

#[cfg(test)]
mod tests {
    use super::png_dimensions;

    #[test]
    fn png_dimensions_reads_ihdr() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1728u32.to_be_bytes());
        png.extend_from_slice(&1084u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        assert_eq!(png_dimensions(&png).unwrap(), (1728, 1084));
        assert!(png_dimensions(b"not a png at all, definitely not").is_err());
        assert!(png_dimensions(&png[..20]).is_err());
    }
}
