//! The fake backend is what consumers (apeiron-bridge) test their policy and
//! session logic against; verify its recorded shape here so a regression shows
//! up in this crate rather than only downstream.

use praxis_desktop::{parse_key, DesktopBackend, FakeBackend, Modifiers};

#[test]
fn records_actions_and_reports_one_window() {
    let b = FakeBackend::with_running(&["com.example.app", "com.other.app"]);

    // Only allowlisted-and-running apps come back, each with one window.
    let apps = b
        .apps(&["com.example.app".into(), "com.missing.app".into()])
        .unwrap();
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].bundle_id, "com.example.app");
    assert_eq!(apps[0].windows.len(), 1);
    // no filter → every running app
    assert_eq!(b.apps(&[]).unwrap().len(), 2);

    // frontmost is set by activate and read back.
    assert_eq!(b.frontmost(), None);
    b.activate("com.example.app").unwrap();
    assert_eq!(b.frontmost().as_deref(), Some("com.example.app"));

    // Every action is logged in order.
    b.click(10.0, 20.0, "left", 2).unwrap();
    b.type_text("hi").unwrap();
    let shot = b.screenshot("com.example.app", 42, None).unwrap();
    assert_eq!((shot.scale, shot.origin), (2.0, (100.0, 50.0)));
    assert_eq!(shot.window.id, 42);
    b.paste_file(std::path::Path::new("/tmp/x.png")).unwrap();
    let (k, m) = parse_key("cmd+shift+a").unwrap();
    assert_eq!(
        (k.as_str(), m),
        (
            "a",
            Modifiers {
                cmd: true,
                shift: true,
                alt: false,
                ctrl: false
            }
        )
    );
    b.key(&k, m).unwrap();

    let log = b.actions();
    assert_eq!(log[0], "activate com.example.app");
    assert_eq!(log[1], "click 10 20 left x2");
    assert_eq!(log[2], "type \"hi\"");
    assert_eq!(log[3], "screenshot com.example.app 42 None");
    assert_eq!(log[4], "paste_file /tmp/x.png");
    assert!(log[5].starts_with("key a Modifiers"));

    // ax_tree returns a stable rooted tree with refs.
    let tree = b.ax_tree("com.example.app", None, 25, 2000).unwrap();
    assert_eq!(tree.r#ref, "e1");
    assert_eq!(tree.children[0].role, "AXTextField");
}
