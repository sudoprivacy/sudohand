//! `praxis-fs` — filesystem actuator.
//!
//! Read / write / list / stat / mkdir / move / copy / delete, over the same
//! JSON-CLI contract as the other actuators. [`RealFs`] is a thin wrapper
//! over `std::fs`; [`FakeFs`] is an in-memory tree that records every
//! mutation for side-effect-free tests. **All path-scoping / sandboxing
//! policy lives in the integrator, not here** (this crate stays
//! policy-free).

#![deny(unsafe_code)]

pub mod backend;
pub mod fake;
pub mod real;

pub use backend::{Entry, EntryKind, FsBackend};
pub use fake::FakeFs;
pub use praxis_core::{Error, Result};
pub use real::RealFs;
