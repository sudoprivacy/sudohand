//! OS permission probing shared by the actuators.
//!
//! [`Permissions`] is the value every actuator reports (its JSON shape is
//! the one adc exposed under `status.permissions`). On macOS the probes
//! talk to TCC directly — Accessibility (`AXIsProcessTrusted`) and Screen
//! Recording (`CGPreflightScreenCaptureAccess`) — via two tiny framework
//! bindings, so this crate needs none of the heavy AX/CG crates.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Permissions {
    pub accessibility: bool,
    /// `None` where the platform has no such permission.
    pub screen_recording: Option<bool>,
}

/// Snapshot of the current process's permissions.
pub fn probe() -> Permissions {
    #[cfg(target_os = "macos")]
    {
        Permissions {
            accessibility: macos::accessibility_trusted(),
            screen_recording: Some(macos::screen_capture_preflight()),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Permissions::default()
    }
}

#[cfg(target_os = "macos")]
pub mod macos {
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::string::CFString;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
    }

    /// Whether this process has Accessibility permission.
    pub fn accessibility_trusted() -> bool {
        // SAFETY: plain query with no arguments.
        unsafe { AXIsProcessTrusted() }
    }

    /// Whether this process has Screen Recording permission (no prompt).
    pub fn screen_capture_preflight() -> bool {
        // SAFETY: plain query with no arguments.
        unsafe { CGPreflightScreenCaptureAccess() }
    }

    /// Ask macOS to show its own "allow accessibility access" prompt —
    /// once per process — so the user lands directly in System Settings.
    pub fn prompt_accessibility_once() {
        static PROMPTED: std::sync::Once = std::sync::Once::new();
        PROMPTED.call_once(|| {
            let key = CFString::new("AXTrustedCheckOptionPrompt");
            let dict = CFDictionary::from_CFType_pairs(&[(
                key.as_CFType(),
                CFBoolean::true_value().as_CFType(),
            )]);
            // SAFETY: valid options dictionary; result ignored.
            unsafe { AXIsProcessTrustedWithOptions(dict.as_concrete_TypeRef()) };
        });
    }
}

#[cfg(test)]
mod tests {
    use super::Permissions;

    #[test]
    fn json_shape_matches_adc() {
        let p = Permissions {
            accessibility: true,
            screen_recording: Some(false),
        };
        assert_eq!(
            serde_json::to_string(&p).unwrap(),
            r#"{"accessibility":true,"screen_recording":false}"#
        );
        let p: Permissions =
            serde_json::from_str(r#"{"accessibility":false,"screen_recording":null}"#).unwrap();
        assert_eq!(p, Permissions::default());
    }
}
