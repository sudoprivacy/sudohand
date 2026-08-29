//! Agent-layer helpers on the fake backend. The graph orchestration tests
//! (conditional edge, GoTo recovery, abort) run against the reference graph
//! in `sudoprivacy/suh-wx` (`src/flows.rs`).

use sudohand_desktop::vlm::NormPoint;
use sudohand_desktop::workflow::norm_to_point;
use sudohand_desktop::FakeBackend;

#[test]
fn norm_to_point_mapping() {
    let b = FakeBackend::with_running(&["x"]);
    let shot = sudohand_desktop::DesktopBackend::screenshot(&*b, "x", 42, None).unwrap();
    let p = norm_to_point(NormPoint { x: 1000.0, y: 0.0 }, &shot);
    assert_eq!(p, (100.0 + 800.0, 50.0));
}
