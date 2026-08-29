//! `praxis-fs` — filesystem actuator.
//!
//! Read / write / list / stat / move / delete, over the same
//! JSON-CLI contract as the other actuators. Thin wrapper over
//! `std::fs`; **all path-scoping / sandboxing policy lives in the
//! integrator, not here** (this crate stays policy-free).
//!
//! Placeholder until the ops land.

/// Placeholder until fs ops are implemented.
pub fn placeholder() -> &'static str {
    "praxis-fs: filesystem ops go here"
}
